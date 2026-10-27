//! Aether background daemon: SQLite + plugin runtime over a local socket.

#[cfg(unix)]
pub mod client;
pub mod error;
pub mod proto;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod settings;

#[cfg(unix)]
pub use client::Client;
// Task types a client needs, without depending on aether-host directly.
pub use aether_host::{StoredFile, StoredTask, TaskLog, TaskState};
pub use error::DaemonError;
#[cfg(unix)]
pub use server::{run, DaemonConfig};
