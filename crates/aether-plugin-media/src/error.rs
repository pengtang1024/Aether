//! Media plugin errors.

use aether_plugin_sdk::{ErrorKind, PluginError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

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

impl MediaError {
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
            Self::Json(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}
