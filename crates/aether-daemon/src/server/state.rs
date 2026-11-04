use crate::server::*;

pub(crate) struct State {
    pub(crate) manager: PluginManager,
    pub(crate) store: TaskStore,
    pub(crate) download_dir: PathBuf,
    pub(crate) plugins_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    /// Shared lifecycle for `kind: "service"` plugins (aria2 compatibility).
    pub(crate) services: Arc<crate::service::ServiceSupervisor>,
    /// Loopback `ip:port` of the bound RPC listener, used by the aria2 plugin
    /// to publish `addTorrent`/`addMetalink` blobs. `None` while RPC is off.
    pub(crate) rpc_base: Option<String>,
    /// Latest plugin detail payload per task (BT peers/seeding, HTTP chunks).
    pub(crate) details: HashMap<String, serde_json::Value>,
    /// Last time a task reported progress; drives the stall watchdog.
    pub(crate) last_progress: HashMap<String, Instant>,
    pub(crate) active: HashSet<String>,
    pub(crate) update: Option<Arc<Mutex<serde_json::Value>>>,
    /// Progress of `plugin_update_all` / `install_plugin`, read by `stats`
    /// so the UI can draw a bar instead of waiting for the RPC to return.
    pub(crate) plugin_progress: Arc<Mutex<serde_json::Value>>,
    pub(crate) subscribers: Vec<Sender<serde_json::Value>>,
    pub(crate) max_concurrent: usize,
    pub(crate) max_speed_bps: Option<u64>,
    pub(crate) retry_max: u32,
    pub(crate) retry_base_secs: u64,
    /// Tasks whose plugin execution thread is alive in this daemon session
    /// (running or paused). A task outside this set must be (re)started from
    /// its stored record instead of receiving an in-process Resume.
    pub(crate) live: HashSet<String>,
    /// Tasks whose cancel command is still being executed by their plugin.
    pub(crate) canceling: HashSet<String>,
    /// Tasks the schedule paused (not the user); resumed when the window
    /// opens again. In-memory on purpose: after a daemon restart they simply
    /// stay paused until the user or the next window acts.
    pub(crate) schedule_paused: HashSet<String>,
    /// Automatic retry schedule per failed task: attempt count and next start.
    pub(crate) retries: HashMap<String, RetrySchedule>,
    /// Persisted daemon settings: proxy, network behaviour (user agent, IP
    /// mode, DNS servers) and the scheduler limits below. The limits are
    /// resolved at startup, so this always reports what is in force.
    pub(crate) settings: Settings,
    pub(crate) settings_path: PathBuf,
    /// PAC evaluator, present only while the proxy setting is `pac`.
    pub(crate) pac: Option<PacResolver>,
    /// Cached desktop proxy detection for `mode: "system"`.
    pub(crate) system_proxy: SystemProxyCache,
    /// PAC resolver for a desktop-configured PAC URL (GNOME/KDE "auto").
    pub(crate) system_pac: Option<(String, PacResolver)>,
    /// RPC token shared with the TCP accept loop: `set_settings` updates it
    /// in place, so a rotation applies to the next connection.
    pub(crate) rpc_secret: Arc<Mutex<Option<String>>>,
    /// Host version for plugin compatibility checks at install time
    /// (mirrors what the manager was built with).
    pub(crate) host_version: String,
    /// Release Ed25519 public key that signs plugin manifests; the trust
    /// root for `install_plugin`. `None` = unsigned installs need the
    /// explicit `AETHER_ALLOW_UNSIGNED_PLUGINS=1` escape hatch.
    pub(crate) plugin_public_key: Option<String>,
    /// Update manifest URL; when set together with the key, the plugin
    /// update channel (`plugin_updates` / `plugin_update_all`) is live.
    pub(crate) update_url: Option<String>,
    /// AppImage path the updater replaces when a release is applied.
    pub(crate) update_appimage: Option<PathBuf>,
    /// Finalize jobs run on their own threads (hashing + moving a large file
    /// must not freeze the RPC loop); results come back through this channel.
    pub(crate) finalize_tx: Sender<FinalizeOutcome>,
    pub(crate) finalize_rx: mpsc::Receiver<FinalizeOutcome>,
    /// Number of finalize jobs still in flight; the idle loop polls faster
    /// while this is non-zero so completion is not delayed by a second.
    pub(crate) finalize_pending: usize,
}

/// Result of a background finalize pass, applied on the daemon's main loop.
pub(crate) struct FinalizeOutcome {
    pub(crate) task_id: String,
    pub(crate) files: Vec<FileInfo>,
    pub(crate) staging_root: PathBuf,
    pub(crate) result: Result<Vec<PathBuf>, String>,
}

/// Detection shells out, so it is sampled once a minute instead of per task.
#[derive(Default)]
pub(crate) struct SystemProxyCache {
    pub(crate) checked: Option<Instant>,
    pub(crate) value: Option<SystemProxy>,
}

impl SystemProxyCache {
    pub(crate) fn lookup(&mut self) -> Option<&SystemProxy> {
        let stale = self
            .checked
            .map(|at| at.elapsed() >= SYSTEM_PROXY_TTL)
            .unwrap_or(true);
        if stale {
            self.value = system_proxy::detect();
            self.checked = Some(Instant::now());
        }
        self.value.as_ref()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetrySchedule {
    pub(crate) next: Instant,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
