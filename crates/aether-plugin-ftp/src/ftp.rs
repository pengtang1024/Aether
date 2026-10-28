//! FTP URL parsing, probing and transfer.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::{sanitize_filename, write_all_at};
use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, Metadata, PluginError, PluginEvent,
    PluginSender, TaskPaths, TaskSpec,
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use suppaftp::rustls::{ClientConfig, RootCertStore};
use suppaftp::types::FileType;
use suppaftp::{FtpStream, RustlsConnector, RustlsFtpStream};
use thiserror::Error;
use url::Url;

const STATE_FILE: &str = ".aether-ftp-state.json";
const CHUNK: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct FtpTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub path: String,
    pub secure: bool,
}

impl FtpTarget {
    pub fn filename(&self) -> Option<String> {
        self.path
            .rsplit('/')
            .find(|part| !part.is_empty())
            .and_then(sanitize_filename)
    }
}

pub struct FtpProbe {
    pub supported: bool,
    pub protocol: Option<String>,
    pub size: Option<u64>,
    pub filename: Option<String>,
    pub reason: Option<String>,
}

pub fn parse_url(input: &str) -> Result<FtpTarget, FtpError> {
    let url = Url::parse(input)?;
    if !matches!(url.scheme(), "ftp" | "ftps") {
        return Err(FtpError::Protocol("not an ftp URL".into()));
    }

    let host = url
        .host_str()
        .ok_or_else(|| FtpError::Protocol("FTP URL has no host".into()))?
        .to_string();
    let port = url.port().unwrap_or(21);
    let username = decode(url.username(), "anonymous");
    let password = decode(url.password().unwrap_or("anonymous@"), "");
    let path = percent_decode_str(url.path())
        .decode_utf8_lossy()
        .into_owned();

    if path.is_empty() || path == "/" {
        return Err(FtpError::Protocol("FTP URL has no file path".into()));
    }

    Ok(FtpTarget {
        host,
        port,
        username,
        password,
        path,
        secure: url.scheme() == "ftps",
    })
}

fn decode(value: &str, fallback: &str) -> String {
    let decoded = percent_decode_str(value).decode_utf8_lossy();
    if decoded.is_empty() {
        fallback.to_string()
    } else {
        decoded.into_owned()
    }
}

pub fn probe(input: &str, passive: bool) -> FtpProbe {
    let target = match parse_url(input) {
        Ok(target) => target,
        Err(error) => {
            return FtpProbe {
                supported: false,
                protocol: None,
                size: None,
                filename: None,
                reason: Some(error.to_string()),
            }
        }
    };

    let mut ftp = match connect(&target, passive) {
        Ok(ftp) => ftp,
        Err(error) => {
            return FtpProbe {
                supported: true,
                protocol: Some(if target.secure { "ftps" } else { "ftp" }.into()),
                size: None,
                filename: target.filename(),
                reason: Some(error.to_string()),
            }
        }
    };

    let size = ftp.size(&target.path).ok().map(|size| size as u64);
    let _ = ftp.quit();

    FtpProbe {
        supported: true,
        protocol: Some(if target.secure { "ftps" } else { "ftp" }.into()),
        size,
        filename: target.filename(),
        reason: None,
    }
}

pub fn run(
    sender: PluginSender,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
    passive: bool,
) {
    match run_inner(&sender, &task, &paths, &control, &progress, passive) {
        Ok(()) => {}
        Err(FtpError::Canceled) => {}
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
    passive: bool,
) -> Result<(), FtpError> {
    if control.is_canceled() {
        return Err(FtpError::Canceled);
    }

    let target = parse_url(&task.url)?;
    fs::create_dir_all(&paths.staging)?;
    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .or_else(|| target.filename())
        .unwrap_or_else(|| "download.bin".into());
    let file_path = paths.staging.join(&filename);

    let mut ftp = connect(&target, passive)?;
    let remote_size = ftp.size(&target.path).ok().map(|size| size as u64);
    let total = remote_size.or(task.expected_size);
    if let (Some(expected), Some(remote)) = (task.expected_size, remote_size) {
        if expected != remote {
            return Err(FtpError::Protocol(format!(
                "remote size {remote} does not match expected {expected}"
            )));
        }
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&file_path)?;

    let state_path = state_path(&paths.staging);
    let state = read_state(&state_path).ok().flatten();
    let reset = state
        .as_ref()
        .is_some_and(|state| state.url != task.url || Some(state.size) != total);
    let mut done = file.metadata()?.len();
    if reset || total.is_some_and(|total| done > total) {
        file.set_len(0)?;
        done = 0;
        let _ = fs::remove_file(&state_path);
    }

    write_state(
        &state_path,
        &FtpState {
            url: task.url.clone(),
            size: total.unwrap_or(0),
            filename: filename.clone(),
        },
    )?;

    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: task.url.clone(),
            size: total,
            etag: None,
            content_type: None,
            files: vec![],
        },
    });

    if total.is_some_and(|total| done == total && total > 0) {
        return finish(
            sender, task, paths, &file, &file_path, &filename, done, control,
        );
    }

    if done > 0 {
        ftp.resume_transfer(done as usize)?;
    }

    let mut stream = ftp.retr_as_stream(&target.path)?;
    let mut offset = done;
    let mut buffer = vec![0_u8; CHUNK];
    let mut last_progress = Instant::now();
    let mut snapshot = ProgressSnapshot {
        downloaded: done,
        total,
        connections: 1,
        chunks_done: 0,
        chunks_total: 1,
        ..Default::default()
    };

    loop {
        if !control.wait_until_running() {
            let _ = ftp.abort(stream);
            return Err(FtpError::Canceled);
        }

        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        write_all_at(&file, &buffer[..read], offset)?;
        offset += read as u64;

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            snapshot.downloaded = offset;
            snapshot.total = total;
            snapshot.speed_bps = 0;
            snapshot.seq = snapshot.seq.saturating_add(1);
            *progress.lock().unwrap() = snapshot.clone();
            let _ = sender.send(&PluginEvent::Progress {
                task_id: task.task_id.clone(),
                seq: snapshot.seq,
                downloaded: offset,
                total,
                speed_bps: 0,
                eta_secs: None,
                connections: 1,
                chunks_done: None,
                chunks_total: Some(1),
            });
            last_progress = Instant::now();
        }
    }

    ftp.finalize_retr_stream(stream)?;
    let _ = ftp.quit();

    if let Some(total) = total {
        if offset != total {
            return Err(FtpError::Protocol(format!(
                "size mismatch: expected {total}, wrote {offset}"
            )));
        }
    }

    finish(
        sender, task, paths, &file, &file_path, &filename, offset, control,
    )
}

fn finish(
    sender: &PluginSender,
    task: &TaskSpec,
    paths: &TaskPaths,
    file: &File,
    file_path: &Path,
    filename: &str,
    size: u64,
    control: &TaskControl,
) -> Result<(), FtpError> {
    if control.is_canceled() {
        return Err(FtpError::Canceled);
    }

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(FtpError::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(file_path);
            let _ = fs::remove_file(state_path(&paths.staging));
            return Err(FtpError::ChecksumMismatch);
        }
    }

    let _ = fs::remove_file(state_path(&paths.staging));
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![FileInfo {
            relative_path: filename.to_string(),
            size,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: Some(serde_json::json!({ "remote_size": size })),
    })?;

    Ok(())
}

fn connect(target: &FtpTarget, passive: bool) -> Result<FtpConnection, FtpError> {
    // Passive (PASV/EPSV) is what nearly every server and NAT expects; the
    // active mode switch exists for servers that require the client to listen.
    let mode = if passive {
        suppaftp::Mode::Passive
    } else {
        suppaftp::Mode::Active
    };
    if target.secure {
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = RustlsConnector::from(Arc::new(config));
        let mut ftp = RustlsFtpStream::connect((target.host.as_str(), target.port))?
            .into_secure(connector, &target.host)?;
        ftp.login(&target.username, &target.password)?;
        ftp.set_mode(mode);
        ftp.transfer_type(FileType::Binary)?;
        Ok(FtpConnection::Secure(ftp))
    } else {
        let mut ftp = FtpStream::connect((target.host.as_str(), target.port))?;
        ftp.login(&target.username, &target.password)?;
        ftp.set_mode(mode);
        ftp.transfer_type(FileType::Binary)?;
        Ok(FtpConnection::Plain(ftp))
    }
}

enum FtpConnection {
    Plain(FtpStream),
    Secure(RustlsFtpStream),
}

impl FtpConnection {
    fn size(&mut self, path: &str) -> Result<usize, suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => ftp.size(path),
            Self::Secure(ftp) => ftp.size(path),
        }
    }

    fn resume_transfer(&mut self, offset: usize) -> Result<(), suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => ftp.resume_transfer(offset),
            Self::Secure(ftp) => ftp.resume_transfer(offset),
        }
    }

    fn retr_as_stream(&mut self, path: &str) -> Result<Box<dyn Read>, suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => Ok(Box::new(ftp.retr_as_stream(path)?)),
            Self::Secure(ftp) => Ok(Box::new(ftp.retr_as_stream(path)?)),
        }
    }

    fn finalize_retr_stream(&mut self, stream: Box<dyn Read>) -> Result<(), suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => ftp.finalize_retr_stream(stream),
            Self::Secure(ftp) => ftp.finalize_retr_stream(stream),
        }
    }

    fn abort(&mut self, stream: Box<dyn Read>) -> Result<(), suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => ftp.abort(stream),
            Self::Secure(ftp) => ftp.abort(stream),
        }
    }

    fn quit(&mut self) -> Result<(), suppaftp::FtpError> {
        match self {
            Self::Plain(ftp) => ftp.quit(),
            Self::Secure(ftp) => ftp.quit(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FtpState {
    url: String,
    size: u64,
    filename: String,
}

fn state_path(staging: &Path) -> PathBuf {
    staging.join(STATE_FILE)
}

fn read_state(path: &Path) -> Result<Option<FtpState>, FtpError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(FtpError::Io(error)),
    }
}

fn write_state(path: &Path, state: &FtpState) -> Result<(), FtpError> {
    fs::write(path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum FtpError {
    #[error("ftp error: {0}")]
    Ftp(#[from] suppaftp::FtpError),

    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("state json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("canceled")]
    Canceled,

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl FtpError {
    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::ChecksumMismatch => PluginError::new(ErrorKind::Checksum, "SHA-256 mismatch"),
            Self::Ftp(suppaftp::FtpError::ConnectionError(error)) => {
                PluginError::new(ErrorKind::Connect, error.to_string()).retryable(true)
            }
            Self::Ftp(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Url(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Io(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Json(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}
