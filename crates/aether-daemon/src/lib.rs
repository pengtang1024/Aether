//! Aether background daemon: SQLite + plugin runtime over a local socket.

pub mod aria2;
pub mod client;
mod client_types;
pub mod error;
pub mod proto;
pub mod server;
pub mod service;
pub mod settings;
pub mod transport;

pub use client::Client;
pub use client_types::{PluginInfo, Stats};
// Task types a client needs, without depending on aether-host directly.
pub use aether_host::{StoredFile, StoredTask, TaskLog, TaskState};
pub use error::DaemonError;
pub use server::{run, DaemonConfig};
