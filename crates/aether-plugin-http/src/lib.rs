//! HTTP/HTTPS download plugin for Aether.
//!
//! Supports probe (HEAD with GET fallback), segmented Range downloads, resume
//! from the staging state file, pause/resume/cancel and SHA-256 verification.

pub mod client;
pub mod download;
pub mod metalink;
pub mod plugin;
pub mod state;

pub use plugin::HttpPlugin;
