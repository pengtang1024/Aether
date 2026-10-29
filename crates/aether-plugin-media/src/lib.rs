//! HLS/DASH media plugin for Aether.

pub mod dash;
pub mod error;
pub mod hls;
pub mod http;
pub mod plugin;
pub mod transcode;

pub use plugin::MediaPlugin;
