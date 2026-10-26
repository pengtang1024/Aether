use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("host error: {0}")]
    Host(#[from] aether_host::HostError),

    #[error("store error: {0}")]
    Store(#[from] aether_host::StoreError),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("daemon is already running")]
    AlreadyRunning,
}
