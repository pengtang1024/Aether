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
pub use error::DaemonError;
#[cfg(unix)]
pub use server::{run, DaemonConfig};
