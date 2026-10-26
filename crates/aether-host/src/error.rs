use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("store error: {0}")]
    Store(#[from] crate::store::StoreError),

    #[error("manifest error: {0}")]
    Manifest(#[from] aether_plugin_sdk::ManifestError),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("ABI plugin error: {0}")]
    Abi(String),

    #[error("plugin not found: {0}")]
    PluginNotFound(String),

    #[error("plugin is disabled: {0}")]
    PluginDisabled(String),

    #[error("path escapes the task staging directory: {0}")]
    UnsafePath(String),

    #[error("size mismatch for {path}: expected {expected}, got {actual}")]
    SizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },

    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(String),
}
