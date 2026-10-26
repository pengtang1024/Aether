//! Aether first-party plugin SDK.
//!
//! This crate is shared by the Host and by every first-party plugin. It defines
//! the manifest format, the length-prefixed MessagePack IPC framing, the typed
//! command/event protocol, a small [`Plugin`] runner and a host-side
//! [`PluginProcess`] supervisor.
//!
//! The wire protocol is versioned with [`protocol::API_VERSION`]. A Host only
//! starts plugins whose manifest `api` matches.

pub mod checksum;
pub mod error;
pub mod fsutil;
pub mod ipc;
pub mod manifest;
pub mod plugin;
pub mod protocol;
pub mod supervisor;
pub mod task;

pub use error::SdkError;
pub use ipc::{read_frame, write_frame, MAX_FRAME_SIZE};
pub use manifest::{current_platform_key, ManifestError, PluginManifest};
pub use plugin::{run_stdio, Plugin, PluginContext, PluginSender};
pub use protocol::*;
pub use supervisor::PluginProcess;
