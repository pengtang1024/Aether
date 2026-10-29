//! Host-side plugin runtime shared by the Slint host and the CLI.

pub mod abi;
pub mod error;
pub mod finalize;
pub mod manager;
pub mod pac;
pub mod proxy;
pub mod registry;
pub mod space;
pub mod store;
pub mod system_proxy;

pub use abi::AbiPlugin;
pub use error::HostError;
pub use finalize::finalize_task;
pub use manager::{PluginEventEnvelope, PluginManager, ProbeOutcome};
pub use proxy::ProxySetting;
pub use registry::{PluginEntry, PluginRegistry};
pub use space::{ensure_space, human_bytes, SpaceError};
pub use store::{NewTask, StoreError, StoredFile, StoredTask, TaskLog, TaskState, TaskStore};
pub use system_proxy::{SystemProxy, SYSTEM_PROXY_TTL};

/// First existing plugin directory in the usual locations.
pub fn default_plugins_dir() -> std::path::PathBuf {
    if let Ok(value) = std::env::var("AETHER_PLUGINS_DIR") {
        return std::path::PathBuf::from(value);
    }
    for candidate in plugin_dir_candidates() {
        if candidate.is_dir() {
            return candidate;
        }
    }
    std::path::PathBuf::from("plugins")
}

/// The search order behind [`default_plugins_dir`], per platform. On
/// Windows the installer and the portable zip both put `plugins\` beside
/// the exe (`{app}\plugins` — the daemon resolves against its own exe,
/// which the installer keeps in `{app}`), so the exe's directory leads;
/// unix keeps the system location first. The dev-tree tail is shared.
fn plugin_dir_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("plugins"));
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // The .app bundle keeps executables in Contents/MacOS and
            // resources in Contents/Resources — plugins ship with resources.
            candidates.push(dir.join("../Resources/plugins"));
            candidates.push(dir.join("plugins"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    candidates.push(std::path::PathBuf::from("/usr/lib/aether/plugins"));
    candidates.push(std::path::PathBuf::from("build/plugins"));
    candidates.push(std::path::PathBuf::from("plugins"));
    candidates
}

#[cfg(test)]
mod tests {
    use super::plugin_dir_candidates;

    /// Platform system location leads; the dev-tree tail keeps its order on
    /// both platforms so a source checkout behaves the same everywhere.
    #[test]
    fn plugin_dir_candidates_lead_with_the_platform_location() {
        let candidates = plugin_dir_candidates();
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(
            candidates[0],
            std::path::PathBuf::from("/usr/lib/aether/plugins")
        );
        #[cfg(target_os = "macos")]
        {
            // Contents/MacOS/aether-daemon -> Contents/Resources/plugins.
            let first = &candidates[0];
            assert!(
                first.is_absolute(),
                "bundle path must be absolute: {first:?}"
            );
            assert!(
                first.ends_with("Resources/plugins"),
                "plugins ship with the bundle resources: {first:?}"
            );
        }
        #[cfg(windows)]
        {
            // exe-adjacent plugins\ — absolute, ending in the folder name.
            let first = &candidates[0];
            assert!(
                first.is_absolute(),
                "exe-derived path must be absolute: {first:?}"
            );
            assert_eq!(first.file_name().and_then(|n| n.to_str()), Some("plugins"));
        }
        let last = candidates.len() - 1;
        assert_eq!(
            candidates[last - 1],
            std::path::PathBuf::from("build/plugins")
        );
        assert_eq!(candidates[last], std::path::PathBuf::from("plugins"));
    }
}
