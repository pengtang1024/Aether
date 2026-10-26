//! S3-compatible object download plugin for Aether.

pub mod plugin;
pub mod s3;
pub mod sigv4;

pub use plugin::S3Plugin;
