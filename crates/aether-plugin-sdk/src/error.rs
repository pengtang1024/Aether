//! Errors shared by the SDK and the host-side supervisor.

use std::io;

use thiserror::Error;

use crate::ipc::IpcError;
use crate::manifest::ManifestError;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("ipc error: {0}")]
    Ipc(#[from] IpcError),

    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("timed out waiting for {0}")]
    Timeout(&'static str),

    #[error("plugin exited before completing the request ({0})")]
    Exited(String),
}
