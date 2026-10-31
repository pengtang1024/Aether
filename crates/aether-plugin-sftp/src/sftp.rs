//! SFTP URL parsing and download using libssh2.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::sanitize_filename;
use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, Metadata, PluginError, PluginEvent,
    PluginSender, TaskPaths, TaskSpec,
};
use percent_encoding::percent_decode_str;
use ssh2::Session;
use thiserror::Error;
use url::Url;

const CHUNK: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const NO_CREDENTIALS: &str = "no credentials: set a password, sftp_key, or an SSH agent";

#[derive(Debug, Clone)]
struct Target {
    host: String,
    port: u16,
    username: String,
    password: String,
    auth: AuthConfig,
    path: String,
}

/// Authentication method selected for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthPlan {
    Password(String),
    PrivateKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
    Agent,
}

/// Authentication options collected from task settings and the URL query.
///
/// `None` means "not specified", so a lower-priority source can fill the gap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AuthConfig {
    key: Option<String>,
    passphrase: Option<String>,
    agent_allowed: Option<bool>,
}

impl AuthConfig {
    fn from_settings(settings: &serde_json::Value) -> Self {
        Self {
            key: text_setting(settings, "sftp_key"),
            passphrase: text_setting(settings, "sftp_passphrase"),
            agent_allowed: settings
                .get("sftp_agent")
                .and_then(serde_json::Value::as_bool),
        }
    }

    fn from_query(url: &Url) -> Self {
        let mut config = Self::default();
        for (name, value) in url.query_pairs() {
            match name.as_ref() {
                "key" => config.key = non_empty(value.into_owned()),
                "passphrase" => config.passphrase = non_empty(value.into_owned()),
                "agent" => config.agent_allowed = parse_agent_flag(&value),
                _ => {}
            }
        }
        config
    }

    /// Keep the values in `self` and fill missing ones from `fallback`.
    fn with_fallback(mut self, fallback: Self) -> Self {
        if self.key.is_none() {
            self.key = fallback.key;
        }
        if self.passphrase.is_none() {
            self.passphrase = fallback.passphrase;
        }
        if self.agent_allowed.is_none() {
            self.agent_allowed = fallback.agent_allowed;
        }
        self
    }

    fn agent_enabled(&self) -> bool {
        self.agent_allowed.unwrap_or(true)
    }
}

/// Pick the authentication method.
///
/// Priority: explicit private key (settings, then URL query), URL password,
/// then SSH agent when it is enabled and available. Missing everything is an
/// error instead of an unauthenticated connection attempt.
fn select_auth(
    config: &AuthConfig,
    password: &str,
    agent_available: bool,
) -> Result<AuthPlan, SftpError> {
    if let Some(key) = config.key.as_deref().filter(|key| !key.trim().is_empty()) {
        return Ok(AuthPlan::PrivateKey {
            path: expand_home(key.trim()),
            passphrase: config
                .passphrase
                .clone()
                .filter(|passphrase| !passphrase.is_empty()),
        });
    }
    if !password.is_empty() {
        return Ok(AuthPlan::Password(password.to_owned()));
    }
    if config.agent_enabled() && agent_available {
        return Ok(AuthPlan::Agent);
    }
    Err(SftpError::Protocol(NO_CREDENTIALS.into()))
}

fn text_setting(settings: &serde_json::Value, key: &str) -> Option<String> {
    settings
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_agent_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn agent_available() -> bool {
    std::env::var_os("SSH_AUTH_SOCK").is_some_and(|value| !value.is_empty())
}

fn expand_home(path: &str) -> PathBuf {
    expand_home_with(path, std::env::var("HOME").ok().as_deref())
}

/// Expand a leading `~` or `~/` with the supplied home directory.
fn expand_home_with(path: &str, home: Option<&str>) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return PathBuf::from(path);
    }
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return PathBuf::from(path);
    };
    if rest.is_empty() {
        PathBuf::from(home)
    } else {
        Path::new(home).join(&rest[1..])
    }
}

pub struct SftpProbe {
    pub supported: bool,
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub reason: Option<String>,
}

pub fn is_sftp_url(input: &str) -> bool {
    input.starts_with("sftp://")
}

pub fn probe(input: &str) -> SftpProbe {
    let target = match Target::parse(input) {
        Ok(target) => target,
        Err(error) => {
            return SftpProbe {
                supported: false,
                filename: None,
                size: None,
                reason: Some(error.to_string()),
            }
        }
    };
    match connect_and_stat(&target) {
        Ok(size) => SftpProbe {
            supported: true,
            filename: target.filename(),
            size: Some(size),
            reason: None,
        },
        Err(error) => SftpProbe {
            supported: true,
            filename: target.filename(),
            size: None,
            reason: Some(error.to_string()),
        },
    }
}

pub fn run(
    sender: PluginSender,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    match run_inner(&sender, &task, &paths, &control, &progress) {
        Ok(()) => {}
        Err(SftpError::Canceled) => {}
        Err(error) => {
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: error.to_plugin_error(),
            });
        }
    }
}

fn run_inner(
    sender: &PluginSender,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
) -> Result<(), SftpError> {
    if control.is_canceled() {
        return Err(SftpError::Canceled);
    }
    let target = Target::parse(&task.url)?;
    let auth = target.auth_plan(&task.settings)?;
    let (session, _) = connect(&target, &auth)?;
    let sftp = session.sftp()?;
    let mut remote = sftp.open(&target.path)?;
    let size = remote.stat()?.size.unwrap_or(0);
    drop(remote);

    fs::create_dir_all(&paths.staging)?;
    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .or_else(|| target.filename())
        .unwrap_or_else(|| "download.bin".into());
    let file_path = paths.staging.join(&filename);
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&file_path)?;
    let mut done = file.metadata()?.len();
    if done > size {
        file.set_len(0)?;
        done = 0;
    }
    file.seek(SeekFrom::Start(done))?;

    let mut remote = sftp.open(&target.path)?;
    if done > 0 {
        remote.seek(SeekFrom::Start(done))?;
    }

    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: task.url.clone(),
            size: Some(size),
            etag: None,
            content_type: None,
            files: vec![],
        },
    });

    let mut snapshot = ProgressSnapshot {
        downloaded: done,
        total: Some(size),
        connections: 1,
        ..Default::default()
    };
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut buffer = vec![0_u8; CHUNK];

    loop {
        if !control.wait_until_running() {
            return Err(SftpError::Canceled);
        }
        let read = remote.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buffer[..read])?;
        done += read as u64;

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            snapshot.downloaded = done;
            snapshot.seq = snapshot.seq.saturating_add(1);
            *progress.lock().unwrap() = snapshot.clone();
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq: snapshot.seq,
                downloaded: done,
                total: Some(size),
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                chunks_done: None,
                chunks_total: None,
            });
            last_progress = Instant::now();
        }
    }

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(SftpError::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            return Err(SftpError::ChecksumMismatch);
        }
    }

    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![FileInfo {
            relative_path: filename,
            size: done,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: None,
    })?;

    Ok(())
}

fn connect_and_stat(target: &Target) -> Result<u64, SftpError> {
    let auth = target.auth_plan(&serde_json::Value::Null)?;
    let (session, _) = connect(target, &auth)?;
    let sftp = session.sftp()?;
    let stat = sftp.stat(Path::new(&target.path))?;
    Ok(stat.size.unwrap_or(0))
}

fn connect(target: &Target, auth: &AuthPlan) -> Result<(Session, TcpStream), SftpError> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut session = Session::new()?;
    session.set_tcp_stream(tcp.try_clone()?);
    session.handshake()?;
    authenticate(&session, &target.username, auth)?;
    Ok((session, tcp))
}

fn authenticate(session: &Session, username: &str, auth: &AuthPlan) -> Result<(), SftpError> {
    match auth {
        AuthPlan::Password(password) => session.userauth_password(username, password)?,
        AuthPlan::PrivateKey { path, passphrase } => {
            session.userauth_pubkey_file(username, None, path, passphrase.as_deref())?
        }
        AuthPlan::Agent => session.userauth_agent(username)?,
    }
    if !session.authenticated() {
        return Err(SftpError::Protocol("authentication failed".into()));
    }
    Ok(())
}

impl Target {
    fn parse(input: &str) -> Result<Self, SftpError> {
        let url = Url::parse(input)?;
        if url.scheme() != "sftp" {
            return Err(SftpError::Protocol("not an sftp URL".into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| SftpError::Protocol("SFTP URL has no host".into()))?
            .to_string();
        let port = url.port().unwrap_or(22);
        let username = percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned();
        let password = percent_decode_str(url.password().unwrap_or(""))
            .decode_utf8_lossy()
            .into_owned();
        let auth = AuthConfig::from_query(&url);
        let path = percent_decode_str(url.path())
            .decode_utf8_lossy()
            .into_owned();
        if path.is_empty() || path == "/" {
            return Err(SftpError::Protocol("SFTP URL has no path".into()));
        }
        Ok(Self {
            host,
            port,
            username,
            password,
            auth,
            path,
        })
    }

    fn auth_plan(&self, settings: &serde_json::Value) -> Result<AuthPlan, SftpError> {
        let config = AuthConfig::from_settings(settings).with_fallback(self.auth.clone());
        select_auth(&config, &self.password, agent_available())
    }

    fn filename(&self) -> Option<String> {
        self.path
            .rsplit('/')
            .find(|part| !part.is_empty())
            .and_then(sanitize_filename)
    }
}

#[derive(Debug, Error)]
pub enum SftpError {
    #[error("ssh error: {0}")]
    Ssh(#[from] ssh2::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("canceled")]
    Canceled,

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl SftpError {
    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::ChecksumMismatch => PluginError::new(ErrorKind::Checksum, "SHA-256 mismatch"),
            Self::Ssh(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Io(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Url(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn private_key(path: &str, passphrase: Option<&str>) -> AuthPlan {
        AuthPlan::PrivateKey {
            path: PathBuf::from(path),
            passphrase: passphrase.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn parses_sftp_url() {
        let target = Target::parse("sftp://user:p%40ss@example.com:2222/pub/file.bin").unwrap();
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 2222);
        assert_eq!(target.username, "user");
        assert_eq!(target.password, "p@ss");
        assert_eq!(target.path, "/pub/file.bin");
        assert_eq!(target.filename().as_deref(), Some("file.bin"));
    }

    #[test]
    fn parses_auth_query_parameters() {
        let target = Target::parse(
            "sftp://user@example.com/pub/file.bin?key=%2Fhome%2Fme%2F.id_ed25519&passphrase=p%40ss&agent=0",
        )
        .unwrap();
        assert_eq!(target.path, "/pub/file.bin");
        assert_eq!(
            target.auth,
            AuthConfig {
                key: Some("/home/me/.id_ed25519".into()),
                passphrase: Some("p@ss".into()),
                agent_allowed: Some(false),
            }
        );
    }

    #[test]
    fn parses_agent_flag_variants() {
        for value in ["1", "true", "YES", "on", ""] {
            assert_eq!(parse_agent_flag(value), Some(true), "{value}");
        }
        for value in ["0", "false", "No", "off"] {
            assert_eq!(parse_agent_flag(value), Some(false), "{value}");
        }
        assert_eq!(parse_agent_flag("maybe"), None);
    }

    #[test]
    fn selects_url_password_when_no_key_is_configured() {
        assert_eq!(
            select_auth(&AuthConfig::default(), "secret", true).unwrap(),
            AuthPlan::Password("secret".into())
        );
    }

    #[test]
    fn selects_settings_key_over_url_password() {
        let target =
            Target::parse("sftp://user:url-secret@example.com/pub/file.bin?key=/query/key")
                .unwrap();
        let plan = target
            .auth_plan(&json!({
                "sftp_key": "/settings/key",
                "sftp_passphrase": "phrase",
            }))
            .unwrap();
        assert_eq!(plan, private_key("/settings/key", Some("phrase")));
    }

    #[test]
    fn settings_key_keeps_query_passphrase_as_fallback() {
        let target = Target::parse(
            "sftp://user@example.com/pub/file.bin?key=%2Fquery%2Fkey&passphrase=query-phrase",
        )
        .unwrap();
        let plan = target
            .auth_plan(&json!({ "sftp_key": "/settings/key" }))
            .unwrap();
        assert_eq!(plan, private_key("/settings/key", Some("query-phrase")));
    }

    #[test]
    fn empty_settings_key_does_not_hide_url_password() {
        let target = Target::parse("sftp://user:url-secret@example.com/pub/file.bin").unwrap();
        let plan = target.auth_plan(&json!({ "sftp_key": "   " })).unwrap();
        assert_eq!(plan, AuthPlan::Password("url-secret".into()));
    }

    #[test]
    fn uses_agent_when_enabled_and_available() {
        assert_eq!(
            select_auth(&AuthConfig::default(), "", true).unwrap(),
            AuthPlan::Agent
        );
        assert_eq!(
            select_auth(
                &AuthConfig::from_settings(&json!({ "sftp_agent": true })),
                "",
                true
            )
            .unwrap(),
            AuthPlan::Agent
        );
    }

    #[test]
    fn refuses_agent_when_unavailable() {
        let error = select_auth(&AuthConfig::default(), "", false).unwrap_err();
        assert!(error.to_string().contains(NO_CREDENTIALS), "{error}");
    }

    #[test]
    fn settings_can_disable_agent() {
        let error = select_auth(
            &AuthConfig::from_settings(&json!({ "sftp_agent": false })),
            "",
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains(NO_CREDENTIALS), "{error}");
    }

    #[test]
    fn url_can_disable_agent() {
        let target = Target::parse("sftp://user@example.com/pub/file.bin?agent=0").unwrap();
        let error = target.auth_plan(&json!({})).unwrap_err();
        assert!(error.to_string().contains(NO_CREDENTIALS), "{error}");
    }

    #[test]
    fn url_password_still_wins_over_agent() {
        let target = Target::parse("sftp://user:url-secret@example.com/pub/file.bin").unwrap();
        assert_eq!(
            target.auth_plan(&json!({})).unwrap(),
            AuthPlan::Password("url-secret".into())
        );
    }

    #[test]
    fn reports_missing_credentials() {
        let target = Target::parse("sftp://user@example.com/pub/file.bin?agent=0").unwrap();
        let error = target
            .auth_plan(&json!({ "sftp_agent": false }))
            .unwrap_err();
        assert!(error.to_string().contains(NO_CREDENTIALS), "{error}");
    }

    #[test]
    fn expands_tilde_using_home() {
        assert_eq!(
            expand_home_with("~/.ssh/id_ed25519", Some("/home/tester")),
            PathBuf::from("/home/tester/.ssh/id_ed25519")
        );
        assert_eq!(
            expand_home_with("~", Some("/home/tester")),
            PathBuf::from("/home/tester")
        );
        assert_eq!(
            expand_home_with("/absolute/key", Some("/home/tester")),
            PathBuf::from("/absolute/key")
        );
        assert_eq!(
            expand_home_with("~other/key", Some("/home/tester")),
            PathBuf::from("~other/key")
        );
        assert_eq!(expand_home_with("~/key", None), PathBuf::from("~/key"));
    }

    #[test]
    fn expands_tilde_when_selecting_private_key() {
        let plan = select_auth(
            &AuthConfig {
                key: Some("~/keys/id_test".into()),
                passphrase: None,
                agent_allowed: None,
            },
            "",
            false,
        )
        .unwrap();
        let expected = expand_home_with("~/keys/id_test", std::env::var("HOME").ok().as_deref());
        assert_eq!(
            plan,
            AuthPlan::PrivateKey {
                path: expected,
                passphrase: None
            }
        );
    }
}
