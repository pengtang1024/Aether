use crate::server::*;

pub struct DaemonConfig {
    pub download_dir: PathBuf,
    pub plugins_dir: PathBuf,
    pub socket: PathBuf,
    pub host_version: String,
    pub update_url: Option<String>,
    pub update_public_key: Option<String>,
    pub update_appimage: Option<PathBuf>,
    /// Maximum number of simultaneously running tasks; the rest wait queued.
    pub max_concurrent: usize,
    /// Optional total download speed limit shared across running tasks.
    pub max_speed_bps: Option<u64>,
    /// Automatic retry attempts for retryable task failures.
    pub retry_max: u32,
    /// Base delay in seconds for the exponential retry backoff.
    pub retry_base_secs: u64,
    /// RPC over TCP listen address (`"ip:port"`), overriding the persisted
    /// `rpc_listen` setting for this run. Off when both are unset.
    pub rpc_listen: Option<String>,
    /// Proxy spec from `AETHER_PROXY`; overrides the persisted setting.
    pub proxy: Option<String>,
    /// PAC URL from `AETHER_PAC_URL`; overrides the persisted setting.
    pub pac_url: Option<String>,
}

/// Signed release manifest. Overridable with `AETHER_UPDATE_URL` or
/// `--update-url`; an empty value turns update checks off.
const DEFAULT_UPDATE_URL: &str =
    "https://github.com/pengtang1024/Aether/releases/latest/download/update.json";
/// Official Aether release key. Public by design: it only verifies manifests.
const RELEASE_PUBLIC_KEY: &str = "gmTgGxsoxi9dyYiWQ+mlqeqx4gDNnMwQ+P0jwNbsa+w=";

/// `AETHER_UPDATE_*` when set (empty means "disabled"), else the built-in
/// default so a daemon started by the desktop app checks updates too.
fn update_setting(variable: &str, default: &str) -> Option<String> {
    match std::env::var(variable) {
        Ok(value) if value.is_empty() => None,
        Ok(value) => Some(value),
        Err(_) => Some(default.to_string()),
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("downloads"),
            plugins_dir: default_plugins_dir(),
            socket: crate::client::default_socket(),
            host_version: env!("CARGO_PKG_VERSION").into(),
            update_url: update_setting("AETHER_UPDATE_URL", DEFAULT_UPDATE_URL),
            update_public_key: update_setting("AETHER_UPDATE_PUBKEY", RELEASE_PUBLIC_KEY),
            update_appimage: std::env::var("AETHER_UPDATE_APPIMAGE")
                .ok()
                .map(PathBuf::from),
            max_concurrent: std::env::var("AETHER_MAX_CONCURRENT")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(3),
            max_speed_bps: std::env::var("AETHER_MAX_SPEED_BPS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0),
            retry_max: std::env::var("AETHER_RETRY_MAX")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5),
            retry_base_secs: std::env::var("AETHER_RETRY_BASE_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(30),
            rpc_listen: std::env::var("AETHER_RPC_LISTEN")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            proxy: std::env::var("AETHER_PROXY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            pac_url: std::env::var("AETHER_PAC_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}
