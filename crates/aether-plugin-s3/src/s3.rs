//! S3-compatible object download via SigV4 GET/HEAD.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_plugin_sdk::fsutil::{sanitize_filename, write_all_at};
use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, Metadata, PluginError, PluginEvent,
    PluginSender, TaskPaths, TaskSpec,
};
use percent_encoding::percent_decode_str;
use reqwest::blocking::Client;
use reqwest::header::RANGE;
use thiserror::Error;
use url::Url;

use crate::sigv4::{self, Credentials};

const CHUNK: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

pub struct S3Probe {
    pub supported: bool,
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub reason: Option<String>,
}

pub fn is_s3_url(input: &str) -> bool {
    input.starts_with("s3://")
}

pub fn probe(client: &Client, input: &str, settings: &serde_json::Value) -> S3Probe {
    let target = match Target::parse(input) {
        Ok(target) => target,
        Err(error) => {
            return S3Probe {
                supported: false,
                filename: None,
                size: None,
                reason: Some(error.to_string()),
            }
        }
    };
    let config = match Config::from_settings(settings) {
        Ok(config) => config,
        Err(error) => {
            return S3Probe {
                supported: true,
                filename: target.filename(),
                size: None,
                reason: Some(error.to_string()),
            }
        }
    };
    let url = match target.url(&config) {
        Ok(url) => url,
        Err(error) => {
            return S3Probe {
                supported: true,
                filename: target.filename(),
                size: None,
                reason: Some(error.to_string()),
            }
        }
    };

    match send(client, "HEAD", &url, &config, None) {
        Ok(response) => {
            let status = response.status();
            let size = response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            S3Probe {
                supported: true,
                filename: target.filename(),
                size,
                reason: (!status.is_success()).then(|| format!("HTTP {status}")),
            }
        }
        Err(error) => S3Probe {
            supported: true,
            filename: target.filename(),
            size: None,
            reason: Some(error.to_string()),
        },
    }
}

pub fn run(
    sender: PluginSender,
    client: Client,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
) {
    match run_inner(&sender, &client, &task, &paths, &control, &progress) {
        Ok(()) => {}
        Err(S3Error::Canceled) => {}
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
    client: &Client,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
) -> Result<(), S3Error> {
    if control.is_canceled() {
        return Err(S3Error::Canceled);
    }
    let target = Target::parse(&task.url)?;
    let config = Config::from_settings(&task.settings)?;
    let url = target.url(&config)?;
    fs::create_dir_all(&paths.staging)?;

    let filename = task
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .or_else(|| target.filename())
        .unwrap_or_else(|| "download.bin".into());
    let file_path = paths.staging.join(&filename);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&file_path)?;
    let mut done = file.metadata()?.len();

    let range = (done > 0).then(|| format!("bytes={done}-"));
    let mut response = send(client, "GET", &url, &config, range.as_deref())?;
    let status = response.status();
    if !status.is_success() {
        return Err(S3Error::HttpStatus(status.as_u16()));
    }
    if done > 0 && status == reqwest::StatusCode::OK {
        file.set_len(0)?;
        done = 0;
    }

    let total = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|length| length + done);

    let _ = sender.send(&PluginEvent::Metadata {
        task_id: task.task_id.clone(),
        metadata: Metadata {
            final_url: url.to_string(),
            size: total,
            etag: None,
            content_type: None,
            files: vec![],
        },
    });

    let mut snapshot = ProgressSnapshot {
        downloaded: done,
        total,
        connections: 1,
        ..Default::default()
    };
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut offset = done;
    let mut buffer = vec![0_u8; CHUNK];

    loop {
        if !control.wait_until_running() {
            return Err(S3Error::Canceled);
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        write_all_at(&file, &buffer[..read], offset)?;
        offset += read as u64;

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            snapshot.downloaded = offset;
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
                chunks_total: None,
            });
            last_progress = Instant::now();
        }
    }

    file.sync_all()?;
    let checksum = aether_plugin_sdk::checksum::sha256_file(&file_path)?;
    if let Some(expected) = &task.checksum {
        if expected.algorithm != ChecksumAlgorithm::Sha256 {
            return Err(S3Error::Protocol("only SHA-256 is supported".into()));
        }
        if !checksum.eq_ignore_ascii_case(&expected.value) {
            let _ = fs::remove_file(&file_path);
            return Err(S3Error::ChecksumMismatch);
        }
    }

    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files: vec![FileInfo {
            relative_path: filename,
            size: offset,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: None,
    })?;

    Ok(())
}

fn send(
    client: &Client,
    method: &str,
    url: &Url,
    config: &Config,
    range: Option<&str>,
) -> Result<reqwest::blocking::Response, S3Error> {
    let credentials = Credentials {
        access_key: &config.access_key,
        secret_key: &config.secret_key,
        session_token: config.session_token.as_deref(),
        region: &config.region,
    };
    let headers = sigv4::sign_headers(method, url, &credentials).map_err(S3Error::Protocol)?;
    let mut request = client.request(
        method
            .parse()
            .map_err(|_| S3Error::Protocol("invalid method".into()))?,
        url.clone(),
    );
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(range) = range {
        request = request.header(RANGE, range);
    }
    Ok(request.send()?)
}

struct Target {
    bucket: String,
    key: String,
}

impl Target {
    fn parse(input: &str) -> Result<Self, S3Error> {
        let url = Url::parse(input)?;
        if url.scheme() != "s3" {
            return Err(S3Error::Protocol("not an s3:// URL".into()));
        }
        let bucket = url
            .host_str()
            .ok_or_else(|| S3Error::Protocol("S3 URL has no bucket".into()))?
            .to_string();
        let key = percent_decode_str(url.path().trim_start_matches('/'))
            .decode_utf8_lossy()
            .into_owned();
        if key.is_empty() {
            return Err(S3Error::Protocol("S3 URL has no object key".into()));
        }
        Ok(Self { bucket, key })
    }

    fn filename(&self) -> Option<String> {
        self.key
            .rsplit('/')
            .find(|part| !part.is_empty())
            .and_then(sanitize_filename)
    }

    fn url(&self, config: &Config) -> Result<Url, S3Error> {
        let key = encode_key(&self.key);
        let value = if let Some(endpoint) = &config.endpoint {
            if config.path_style {
                format!("{}/{}/{key}", endpoint.trim_end_matches('/'), self.bucket)
            } else {
                let parsed = Url::parse(endpoint)?;
                format!(
                    "{}://{}.{}/{key}",
                    parsed.scheme(),
                    self.bucket,
                    parsed.authority()
                )
            }
        } else {
            format!(
                "https://{}.s3.{}.amazonaws.com/{key}",
                self.bucket, config.region
            )
        };
        Ok(Url::parse(&value)?)
    }
}

fn encode_key(key: &str) -> String {
    let mut encoded = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

struct Config {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    region: String,
    endpoint: Option<String>,
    path_style: bool,
}

impl Config {
    fn from_settings(settings: &serde_json::Value) -> Result<Self, S3Error> {
        let access_key = setting(settings, "access_key")
            .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
            .ok_or_else(|| S3Error::Protocol("S3 access_key is not configured".into()))?;
        let secret_key = setting(settings, "secret_key")
            .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
            .ok_or_else(|| S3Error::Protocol("S3 secret_key is not configured".into()))?;
        let session_token =
            setting(settings, "session_token").or_else(|| std::env::var("AWS_SESSION_TOKEN").ok());
        let region = setting(settings, "region")
            .or_else(|| std::env::var("AWS_REGION").ok())
            .unwrap_or_else(|| "us-east-1".into());
        let endpoint =
            setting(settings, "endpoint").or_else(|| std::env::var("AWS_ENDPOINT_URL").ok());
        let path_style = settings
            .get("path_style")
            .and_then(|value| value.as_bool())
            .unwrap_or(endpoint.is_some());

        Ok(Self {
            access_key,
            secret_key,
            session_token,
            region,
            endpoint,
            path_style,
        })
    }
}

fn setting(settings: &serde_json::Value, key: &str) -> Option<String> {
    settings.get(key)?.as_str().map(ToOwned::to_owned)
}

#[derive(Debug, Error)]
pub enum S3Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("http status {0}")]
    HttpStatus(u16),

    #[error("canceled")]
    Canceled,

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl S3Error {
    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::ChecksumMismatch => PluginError::new(ErrorKind::Checksum, "SHA-256 mismatch"),
            Self::Http(error) if error.is_timeout() => {
                PluginError::new(ErrorKind::Timeout, error.to_string()).retryable(true)
            }
            Self::Http(error) if error.is_connect() => {
                PluginError::new(ErrorKind::Connect, error.to_string()).retryable(true)
            }
            Self::Http(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Url(error) => PluginError::new(ErrorKind::Protocol, error.to_string()),
            Self::Io(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::HttpStatus(status) => {
                PluginError::new(ErrorKind::HttpStatus, format!("HTTP {status}"))
                    .with_status(*status)
            }
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_s3_keys() {
        assert_eq!(encode_key("a b/c.txt"), "a%20b/c.txt");
        assert_eq!(encode_key("naïve.txt"), "na%C3%AFve.txt");
    }

    #[test]
    fn parses_s3_url() {
        let target = Target::parse("s3://bucket/path/file.bin").unwrap();
        assert_eq!(target.bucket, "bucket");
        assert_eq!(target.key, "path/file.bin");
    }
}
