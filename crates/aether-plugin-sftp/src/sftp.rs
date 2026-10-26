//! SFTP URL parsing and download using libssh2.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::Path;
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

#[derive(Debug, Clone)]
struct Target {
    host: String,
    port: u16,
    username: String,
    password: String,
    path: String,
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
    let (session, _) = connect(&target)?;
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
    let (session, _) = connect(target)?;
    let sftp = session.sftp()?;
    let stat = sftp.stat(Path::new(&target.path))?;
    Ok(stat.size.unwrap_or(0))
}

fn connect(target: &Target) -> Result<(Session, TcpStream), SftpError> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut session = Session::new()?;
    session.set_tcp_stream(tcp.try_clone()?);
    session.handshake()?;
    session.userauth_password(&target.username, &target.password)?;
    Ok((session, tcp))
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
            path,
        })
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
}
