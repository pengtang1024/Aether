//! S3-compatible object probing and URL/credential plumbing.

use aether_plugin_sdk::fsutil::sanitize_filename;
use aether_plugin_sdk::{ErrorKind, PluginError};
use percent_encoding::percent_decode_str;
use reqwest::blocking::Client;
use reqwest::header::RANGE;
use thiserror::Error;
use url::Url;

use crate::sigv4::{self, Credentials};

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

pub(crate) fn send(
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
    // Range is intentionally not part of the signed headers: S3 accepts
    // unsigned Range, and sigv4.rs only signs host/x-amz-*.
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

pub(crate) struct Target {
    pub(crate) bucket: String,
    pub(crate) key: String,
}

impl Target {
    pub(crate) fn parse(input: &str) -> Result<Self, S3Error> {
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

    pub(crate) fn filename(&self) -> Option<String> {
        self.key
            .rsplit('/')
            .find(|part| !part.is_empty())
            .and_then(sanitize_filename)
    }

    pub(crate) fn url(&self, config: &Config) -> Result<Url, S3Error> {
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

pub(crate) struct Config {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    region: String,
    endpoint: Option<String>,
    path_style: bool,
}

impl Config {
    pub(crate) fn from_settings(settings: &serde_json::Value) -> Result<Self, S3Error> {
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

    #[error("server ignored a Range request")]
    RangeIgnored,

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
            Self::RangeIgnored => {
                PluginError::new(ErrorKind::Protocol, "server ignored a Range request")
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
