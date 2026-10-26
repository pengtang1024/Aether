//! Host-side plugin runtime shared by the Slint host and the CLI.

pub mod error;
pub mod finalize;
pub mod manager;
pub mod registry;
pub mod store;

pub use error::HostError;
pub use finalize::finalize_task;
pub use manager::{PluginEventEnvelope, PluginManager, ProbeOutcome};
pub use registry::{PluginEntry, PluginRegistry};
pub use store::{NewTask, StoredFile, StoredTask, TaskState, TaskStore};

/// First existing plugin directory in the usual locations.
pub fn default_plugins_dir() -> std::path::PathBuf {
    if let Ok(value) = std::env::var("AETHER_PLUGINS_DIR") {
        return std::path::PathBuf::from(value);
    }
    for candidate in ["/usr/lib/aether/plugins", "build/plugins", "plugins"] {
        let path = std::path::PathBuf::from(candidate);
        if path.is_dir() {
            return path;
        }
    }
    std::path::PathBuf::from("plugins")
}
