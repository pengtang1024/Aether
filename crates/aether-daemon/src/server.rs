//! Daemon event loop and Unix-socket server.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_host::pac::PacResolver;
use aether_host::system_proxy::{self, SystemProxy, SYSTEM_PROXY_TTL};
use aether_host::{
    default_plugins_dir, ensure_space, finalize_task_with_checksum, human_bytes, NewTask,
    PluginEventEnvelope, PluginManager, PluginRegistry, ProxySetting, StoredTask, TaskState,
    TaskStore,
};
use aether_plugin_sdk::{
    Budget, Checksum, ChecksumAlgorithm, HostCommand, PluginEvent, PluginKind, ProxyConfig,
    TaskPaths, TaskSpec,
};
use aether_update::UpdateClient;
use fs2::FileExt;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::proto::{Request, Response};
use crate::service::{ServiceBridge, ServiceStatus, ServiceSupervisor};
use crate::settings::{
    parse_hhmm, Limits, Settings, CONNECTIONS_DEFAULT, KEYS, RPC_LISTEN_DEFAULT,
};
use crate::transport::{Conn, Listener};

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

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("downloads"),
            plugins_dir: default_plugins_dir(),
            socket: crate::client::default_socket(),
            host_version: env!("CARGO_PKG_VERSION").into(),
            update_url: std::env::var("AETHER_UPDATE_URL").ok(),
            update_public_key: std::env::var("AETHER_UPDATE_PUBKEY").ok(),
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

pub(crate) struct Command {
    pub(crate) request: Request,
    pub(crate) respond: Sender<Response>,
    pub(crate) subscriber: Option<Sender<serde_json::Value>>,
    /// Fired after the response bytes are written, so `shutdown` can wait for
    /// the client to receive its ack before the process exits.
    pub(crate) written: Option<mpsc::Receiver<()>>,
}

/// Subscribe a compatibility transport (aria2 WebSocket) to the same event
/// stream the native `subscribe` method uses.
pub(crate) fn subscribe_events(
    commands: &Sender<Command>,
) -> Option<mpsc::Receiver<serde_json::Value>> {
    let (respond_tx, respond_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let request = Request {
        id: 0,
        method: "subscribe".into(),
        url: None,
        plugin: None,
        dir: None,
        task_id: None,
        priority: None,
        settings: serde_json::Value::Null,
    };
    commands
        .send(Command {
            request,
            respond: respond_tx,
            subscriber: Some(event_tx),
            written: None,
        })
        .ok()?;
    match respond_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(response) if response.ok => Some(event_rx),
        _ => None,
    }
}

struct State {
    manager: PluginManager,
    store: TaskStore,
    download_dir: PathBuf,
    plugins_dir: PathBuf,
    data_dir: PathBuf,
    /// Shared lifecycle for `kind: "service"` plugins (aria2 compatibility).
    services: Arc<crate::service::ServiceSupervisor>,
    /// Loopback `ip:port` of the bound RPC listener, used by the aria2 plugin
    /// to publish `addTorrent`/`addMetalink` blobs. `None` while RPC is off.
    rpc_base: Option<String>,
    /// Latest plugin detail payload per task (BT peers/seeding, HTTP chunks).
    details: HashMap<String, serde_json::Value>,
    /// Last time a task reported progress; drives the stall watchdog.
    last_progress: HashMap<String, Instant>,
    active: HashSet<String>,
    update: Option<Arc<Mutex<serde_json::Value>>>,
    subscribers: Vec<Sender<serde_json::Value>>,
    max_concurrent: usize,
    max_speed_bps: Option<u64>,
    retry_max: u32,
    retry_base_secs: u64,
    /// Tasks whose plugin execution thread is alive in this daemon session
    /// (running or paused). A task outside this set must be (re)started from
    /// its stored record instead of receiving an in-process Resume.
    live: HashSet<String>,
    /// Tasks whose cancel command is still being executed by their plugin.
    canceling: HashSet<String>,
    /// Tasks the schedule paused (not the user); resumed when the window
    /// opens again. In-memory on purpose: after a daemon restart they simply
    /// stay paused until the user or the next window acts.
    schedule_paused: HashSet<String>,
    /// Automatic retry schedule per failed task: attempt count and next start.
    retries: HashMap<String, RetrySchedule>,
    /// Persisted daemon settings: proxy, network behaviour (user agent, IP
    /// mode, DNS servers) and the scheduler limits below. The limits are
    /// resolved at startup, so this always reports what is in force.
    settings: Settings,
    settings_path: PathBuf,
    /// PAC evaluator, present only while the proxy setting is `pac`.
    pac: Option<PacResolver>,
    /// Cached desktop proxy detection for `mode: "system"`.
    system_proxy: SystemProxyCache,
    /// PAC resolver for a desktop-configured PAC URL (GNOME/KDE "auto").
    system_pac: Option<(String, PacResolver)>,
    /// RPC token shared with the TCP accept loop: `set_settings` updates it
    /// in place, so a rotation applies to the next connection.
    rpc_secret: Arc<Mutex<Option<String>>>,
    /// Host version for plugin compatibility checks at install time
    /// (mirrors what the manager was built with).
    host_version: String,
    /// Release Ed25519 public key that signs plugin manifests; the trust
    /// root for `install_plugin`. `None` = unsigned installs need the
    /// explicit `AETHER_ALLOW_UNSIGNED_PLUGINS=1` escape hatch.
    plugin_public_key: Option<String>,
    /// Update manifest URL; when set together with the key, the plugin
    /// update channel (`plugin_updates` / `plugin_update_all`) is live.
    update_url: Option<String>,
    /// AppImage path the updater replaces when a release is applied.
    update_appimage: Option<PathBuf>,
}

impl Default for SystemProxyCache {
    fn default() -> Self {
        Self {
            checked: None,
            value: None,
        }
    }
}

/// Detection shells out, so it is sampled once a minute instead of per task.
struct SystemProxyCache {
    checked: Option<Instant>,
    value: Option<SystemProxy>,
}

impl SystemProxyCache {
    fn lookup(&mut self) -> Option<&SystemProxy> {
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
struct RetrySchedule {
    next: Instant,
}

pub fn run(config: DaemonConfig) -> Result<(), DaemonError> {
    let data_dir = config.download_dir.join(".aether");
    fs::create_dir_all(&data_dir)?;
    // The endpoint is a socket file on Unix (its parent must exist) and a
    // pipe name on Windows (no filesystem presence at all).
    #[cfg(unix)]
    fs::create_dir_all(config.socket.parent().unwrap_or(Path::new(".")))?;
    // Single-instance guard. Unix keys it off the socket path; a Windows pipe
    // name has no directory, so the lock lives in the data dir and only
    // serializes daemons that share a download root (the pipe name itself is
    // per user, so a second daemon on the same user still gets rejected by
    // the connect probe below).
    #[cfg(unix)]
    let lock_path = config.socket.with_extension("lock");
    #[cfg(windows)]
    let lock_path = data_dir.join("daemon.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| DaemonError::AlreadyRunning)?;

    #[cfg(unix)]
    {
        if config.socket.exists() {
            if Conn::connect(&config.socket).is_ok() {
                return Err(DaemonError::AlreadyRunning);
            }
            let _ = fs::remove_file(&config.socket);
        }
    }
    // A live pipe refuses a second FILE_FLAG_FIRST_PIPE_INSTANCE below, so
    // the same probe is inherent on Windows.
    #[cfg(windows)]
    if Conn::connect(&config.socket).is_ok() {
        return Err(DaemonError::AlreadyRunning);
    }

    let listener = Listener::bind(&config.socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;
    }

    let registry = PluginRegistry::discover(&[config.plugins_dir.clone()])?;
    let mut manager = PluginManager::new(registry, &data_dir, config.host_version.clone());
    let store = TaskStore::open(data_dir.join("aether.db"))?;
    // Plugins the user switched off last time stay off.
    for plugin_id in store.disabled_plugins()? {
        let _ = manager.registry_mut().set_enabled(&plugin_id, false);
    }

    // Persisted settings, with AETHER_PROXY winning over the file so ops and
    // tests can override a user's choice for one run.
    let settings_path = Settings::path(&config.download_dir);
    let mut settings = Settings::load(&settings_path);
    // RPC is useful out of the box (CLI, second device): seed a loopback
    // address and a token on first boot, then persist so the UI and the CLI
    // read the same values. Explicit empty strings are preserved.
    if settings.seed_rpc_defaults() {
        settings.save(&settings_path)?;
    }
    if let Some(pac) = &config.pac_url {
        settings.proxy = Some(ProxySetting::Pac {
            url: Some(pac.clone()),
            script: None,
        });
    } else if let Some(spec) = &config.proxy {
        settings.proxy = Some(ProxySetting::parse_spec(spec).map_err(DaemonError::Protocol)?);
    }
    // Scheduler limits: the file wins, `AETHER_MAX_*` fills what it leaves
    // out, and the resolved values are written back so `get_settings` reports
    // the truth instead of `null`.
    let limits = settings.resolve_limits(Limits {
        max_concurrent: config.max_concurrent,
        max_speed_bps: config.max_speed_bps,
        retry_max: config.retry_max,
    });
    manager.set_proxy(init_proxy(settings.proxy.as_ref()));
    manager.set_network(settings.network());
    manager.set_allow_external_process(settings.plugin_external_process.unwrap_or(true));
    let update_status = match (&config.update_url, &config.update_public_key) {
        (Some(url), Some(key)) => {
            let status = Arc::new(Mutex::new(serde_json::json!({
                "status": "idle",
                "latest": null,
                "artifact": null,
                "error": null,
            })));
            spawn_update_loop(
                url.clone(),
                key.clone(),
                config.update_appimage.clone(),
                data_dir.clone(),
                settings_path.clone(),
                status.clone(),
                config.socket.clone(),
            );
            Some(status)
        }
        _ => None,
    };

    // Captured before `settings` moves into `State`; the accept loop shares
    // this handle so a rotation via set_settings applies live.
    let rpc_secret = Arc::new(Mutex::new(
        settings
            .rpc_secret
            .clone()
            .filter(|secret| !secret.trim().is_empty()),
    ));
    let mut state = State {
        manager,
        store,
        download_dir: config.download_dir.clone(),
        plugins_dir: config.plugins_dir.clone(),
        data_dir: data_dir.clone(),
        services: ServiceSupervisor::new(
            config.socket.clone(),
            data_dir.clone(),
            config.host_version.clone(),
        ),
        rpc_base: None,
        details: HashMap::new(),
        last_progress: HashMap::new(),
        active: HashSet::new(),
        update: update_status,
        subscribers: Vec::new(),
        max_concurrent: limits.max_concurrent,
        max_speed_bps: limits.max_speed_bps,
        retry_max: limits.retry_max,
        retry_base_secs: config.retry_base_secs,
        live: HashSet::new(),
        canceling: HashSet::new(),
        schedule_paused: HashSet::new(),
        retries: HashMap::new(),
        pac: settings.proxy.as_ref().and_then(ProxySetting::resolver),
        settings,
        settings_path,
        system_proxy: SystemProxyCache::default(),
        system_pac: None,
        rpc_secret,
        host_version: config.host_version.clone(),
        plugin_public_key: config.update_public_key.clone(),
        update_url: config.update_url.clone(),
        update_appimage: config.update_appimage.clone(),
    };

    recover_tasks(&mut state);
    restore_retries(&mut state);
    cleanup_stale_staging(&state);
    let (command_tx, command_rx) = mpsc::channel::<Command>();

    // RPC over TCP: the CLI/env override wins over the persisted setting.
    // Anything wider than loopback must carry a token — checked here as well
    // because the boot path bypasses `set_settings` and its validation.
    let rpc_listen = config
        .rpc_listen
        .clone()
        .filter(|addr| !addr.trim().is_empty())
        .or_else(|| {
            state
                .settings
                .rpc_listen
                .clone()
                .filter(|addr| !addr.trim().is_empty())
        });
    if let Some(addr) = rpc_listen.as_deref() {
        // The effective pair is checked here too: the CLI/env override is
        // not part of `set_settings`, so its validation would miss it.
        let parsed: std::net::SocketAddr = addr.trim().parse().map_err(|error| {
            DaemonError::Protocol(format!("rpc_listen must be ip:port: {error}"))
        })?;
        let has_secret = state
            .rpc_secret
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|secret| !secret.is_empty());
        if !parsed.ip().is_loopback() && !has_secret {
            return Err(DaemonError::Protocol(
                "rpc_listen on a non-loopback address requires rpc_secret".into(),
            ));
        }
        // The default listener is a convenience, not a requirement: a busy
        // port must not keep downloads from starting. Explicit addresses
        // still fail loudly so a misconfiguration is visible.
        let listener = match Listener::bind_tcp(addr.trim()) {
            Ok(listener) => Some(listener),
            Err(error) if addr.trim() == RPC_LISTEN_DEFAULT => {
                eprintln!(
                    "aether-daemon: cannot listen on default RPC address {addr}: {error}; \
                     continuing without TCP RPC"
                );
                None
            }
            Err(error) => {
                return Err(DaemonError::Protocol(format!(
                    "cannot listen on {addr}: {error}"
                )))
            }
        };
        if let Some(listener) = listener {
            let blob_listen = if parsed.ip().is_unspecified() {
                format!("127.0.0.1:{}", parsed.port())
            } else {
                addr.trim().to_string()
            };
            state.rpc_base = Some(blob_listen.clone());
            state.services.set_listen(Some(blob_listen));
            state.services.sync(state.manager.registry());
            if has_secret {
                eprintln!("aether-daemon: rpc listening on {addr} (token required)");
            } else {
                eprintln!("aether-daemon: rpc listening on {addr} (no token; loopback only)");
            }
            let rpc_tx = command_tx.clone();
            let rpc_secret = state.rpc_secret.clone();
            let bridge = Arc::new(ServiceBridge::new(state.services.clone(), "aria2"));
            let blob_dir = state.data_dir.join("aria2-blobs");
            thread::spawn(move || rpc_accept_loop(listener, rpc_tx, rpc_secret, bridge, blob_dir));
        }
    }

    let accept_tx = command_tx.clone();
    let accept_thread = thread::spawn(move || accept_loop(listener, accept_tx));

    eprintln!("aether-daemon: listening on {}", config.socket.display());
    let mut shutdown = false;
    let mut last_idle_cleanup = std::time::Instant::now();
    let mut last_schedule_tick = std::time::Instant::now();
    let mut last_health = Instant::now();
    let mut last_stall = Instant::now();
    while !shutdown {
        // Idle: block on the command channel instead of polling plugins.
        if state.active.is_empty() {
            match command_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(command) => {
                    shutdown |= dispatch(&mut state, command);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_idle_cleanup.elapsed() >= Duration::from_secs(300) {
                        state.manager.stop_all();
                        state.live.clear();
                        last_idle_cleanup = std::time::Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Retry bookkeeping must run even when commands keep arriving.
            check_retries(&mut state);
            if last_health.elapsed() >= Duration::from_secs(5) {
                last_health = Instant::now();
                recover_dead_plugins(&mut state);
                check_stalled_tasks(&mut state);
            }
            if last_schedule_tick.elapsed() >= Duration::from_secs(15) {
                last_schedule_tick = std::time::Instant::now();
                tick_schedule(&mut state);
            }
            continue;
        }

        // Active downloads: drain commands, then poll plugin events.
        while let Ok(command) = command_rx.try_recv() {
            shutdown |= dispatch(&mut state, command);
        }
        match state.manager.recv_event(Duration::from_millis(100)) {
            Ok(Some(envelope)) => handle_event(&mut state, envelope),
            Ok(None) => {}
            Err(error) => {
                eprintln!("aether-daemon: plugin event error: {error}");
                recover_dead_plugins(&mut state);
            }
        }
        check_retries(&mut state);
        if last_health.elapsed() >= Duration::from_secs(5) {
            last_health = Instant::now();
            recover_dead_plugins(&mut state);
        }
        if last_stall.elapsed() >= Duration::from_secs(30) {
            last_stall = Instant::now();
            check_stalled_tasks(&mut state);
        }
        if last_schedule_tick.elapsed() >= Duration::from_secs(15) {
            last_schedule_tick = std::time::Instant::now();
            tick_schedule(&mut state);
        }
    }

    state.manager.shutdown();
    state.services.stop_all();
    drop(accept_thread);
    #[cfg(unix)]
    let _ = fs::remove_file(&config.socket);
    Ok(())
}

/// Category bucket for a finalized file, by extension. Order matches the
/// `stats` JSON: video, archive, app, doc, other.
fn file_category(relative_path: &str) -> usize {
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase());
    match ext.as_deref() {
        Some(
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "ts" | "m4v" | "mp3" | "flac"
            | "wav" | "ogg",
        ) => 0,
        Some("zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "lz4") => 1,
        Some("exe" | "msi" | "dmg" | "deb" | "rpm" | "appimage" | "pkg" | "iso" | "apk") => 2,
        Some(
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "epub"
            | "mobi",
        ) => 3,
        _ => 4,
    }
}

/// The periodic plugin-update pass, run as a *client* of the daemon (the
/// update loop owns no `State`). Skips silently unless the daemon setting
/// `plugin_auto_update` is on; returns what the daemon reported.
pub fn plugin_auto_update_pass(
    socket: &std::path::Path,
) -> Result<Option<serde_json::Value>, DaemonError> {
    let mut client = crate::client::Client::connect(socket)?;
    let settings = client.settings()?;
    if settings["plugin_auto_update"].as_bool() != Some(true) {
        return Ok(None);
    }
    let result = client.call("plugin_update_all", None, None, None, None, json!({}))?;
    Ok(Some(result))
}

/// One app-update pass: fetch and verify the manifest, then — unless the
/// user turned background downloads off — download and, for AppImage,
/// apply the release. Blocking; the loop and the on-demand RPC run it on a
/// worker thread. Returns the status it wrote.
fn update_pass(
    manifest_url: &str,
    public_key: &str,
    appimage: Option<&Path>,
    data_dir: &Path,
    settings_path: &Path,
    status: &Arc<Mutex<serde_json::Value>>,
) -> serde_json::Value {
    // Re-read each pass so the settings switch applies without a restart.
    let auto = Settings::load(settings_path)
        .app_auto_update
        .unwrap_or(true);
    let result = (|| -> Result<serde_json::Value, aether_update::UpdateError> {
        let client = UpdateClient::new(
            manifest_url,
            public_key,
            env!("CARGO_PKG_VERSION"),
            aether_plugin_sdk::current_platform_key(),
            data_dir,
        )?;
        let Some(manifest) = client.check()? else {
            return Ok(json!({"status": "up_to_date"}));
        };
        if !auto {
            // Background downloads are off: report the version and let the
            // user decide from the About panel.
            return Ok(json!({"status": "available", "latest": manifest.version}));
        }
        let asset = client.asset_for_platform(&manifest)?;
        let artifact = client.download(&manifest, asset)?;
        if asset.kind == "appimage" {
            if let Some(target) = appimage {
                client.apply_appimage(&artifact, target)?;
            }
        }
        Ok(json!({
            "status": "ready",
            "latest": manifest.version,
            "artifact": artifact.to_string_lossy(),
        }))
    })();
    let value = result.unwrap_or_else(|error| {
        json!({
            "status": "error",
            "error": error.to_string(),
        })
    });
    *status.lock().unwrap() = value.clone();
    value
}

fn spawn_update_loop(
    manifest_url: String,
    public_key: String,
    appimage: Option<PathBuf>,
    data_dir: PathBuf,
    settings_path: PathBuf,
    status: Arc<Mutex<serde_json::Value>>,
    socket: std::path::PathBuf,
) {
    thread::spawn(move || {
        // Do not delay daemon startup for a network check.
        thread::sleep(Duration::from_secs(30));
        loop {
            // Plugin updates ride the same cadence; failures are logged, not
            // surfaced through the app-update status.
            match plugin_auto_update_pass(&socket) {
                Ok(Some(result)) => {
                    let updated = result["updated"].as_array().map_or(0, Vec::len);
                    if updated > 0 {
                        eprintln!("aether-daemon: auto-updated {updated} plugin(s)");
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("aether-daemon: plugin auto-update: {error}"),
            }
            update_pass(
                &manifest_url,
                &public_key,
                appimage.as_deref(),
                &data_dir,
                &settings_path,
                &status,
            );
            thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

fn dispatch(state: &mut State, mut command: Command) -> bool {
    let id = command.request.id;
    let method = command.request.method.clone();
    if method == "subscribe" {
        if let Some(tx) = command.subscriber {
            state.subscribers.push(tx);
        }
        let _ = command.respond.send(Response::ok(id, json!({"ok": true})));
        return false;
    }
    let shutdown = method == "shutdown";
    let written = command.written.take();
    let response = match handle_request(state, command.request) {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::err(id, error.to_string()),
    };
    let _ = command.respond.send(response);
    if shutdown {
        if let Some(written) = written {
            let _ = written.recv_timeout(Duration::from_secs(2));
        }
    }
    shutdown
}

fn broadcast(state: &mut State, event: Value) {
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

fn accept_loop(mut listener: Listener, commands: Sender<Command>) {
    loop {
        let Ok(conn) = listener.accept() else { break };
        let commands = commands.clone();
        thread::spawn(move || client_loop(conn, commands));
    }
}

/// RPC over TCP: every connection must open with an `auth` request carrying
/// the token before it may use any other method. The secret is read live, so
/// a `set_settings` rotation applies to new connections immediately.
/// Keep-alive means a connection can sit idle for a minute; cap the number
/// of compat threads so a stuck client cannot exhaust the process.
const MAX_RPC_CONNECTIONS: usize = 128;

struct RpcConnectionGuard(Arc<AtomicUsize>);

impl Drop for RpcConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn rpc_accept_loop(
    mut listener: Listener,
    commands: Sender<Command>,
    secret: std::sync::Arc<Mutex<Option<String>>>,
    bridge: Arc<ServiceBridge>,
    blob_dir: PathBuf,
) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok(conn) = listener.accept() else { break };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_RPC_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            conn.shutdown();
            continue;
        }
        let guard = RpcConnectionGuard(active.clone());
        let commands = commands.clone();
        let secret = secret.clone();
        let bridge = bridge.clone();
        let blob_dir = blob_dir.clone();
        thread::spawn(move || {
            let _guard = guard;
            rpc_client_loop(conn, commands, secret, bridge, blob_dir)
        });
    }
}

fn rpc_client_loop(
    stream: Conn,
    commands: Sender<Command>,
    secret: std::sync::Arc<Mutex<Option<String>>>,
    bridge: Arc<ServiceBridge>,
    blob_dir: PathBuf,
) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        writer.shutdown();
        return;
    }
    // One port, two protocols: HTTP goes to the aria2 compatibility bridge,
    // the native JSON-lines protocol keeps its auth handshake below.
    if crate::aria2::is_http_request(&line) {
        crate::aria2::serve_http(
            reader,
            &mut writer,
            &line,
            Some(bridge.as_ref()),
            &secret,
            &commands,
            &blob_dir,
        );
        writer.shutdown();
        return;
    }
    let request = serde_json::from_str::<Request>(&line);
    let authed = match request {
        Ok(request) if request.method == "auth" => {
            let token = request
                .settings
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or("");
            let expected = secret.lock().unwrap();
            let ok = constant_time_eq(
                token.as_bytes(),
                expected.as_deref().unwrap_or("").as_bytes(),
            );
            if write_response(
                &mut writer,
                &if ok {
                    Response::ok(request.id, json!({"ok": true}))
                } else {
                    Response::err(request.id, "invalid rpc token")
                },
            )
            .is_err()
            {
                writer.shutdown();
                return;
            }
            ok
        }
        other => {
            let id = other.as_ref().map_or(0, |request| request.id);
            let _ = write_response(
                &mut writer,
                &Response::err(id, "rpc needs an auth request first"),
            );
            false
        }
    };
    if authed {
        serve(&mut reader, &mut writer, commands);
    }
    writer.shutdown();
}

/// Constant-time byte comparison for secrets (no early exit on the first
/// differing byte).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn client_loop(stream: Conn, commands: Sender<Command>) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    serve(&mut reader, &mut writer, commands);
    writer.shutdown();
}

fn serve(reader: &mut BufReader<Conn>, writer: &mut Conn, commands: Sender<Command>) {
    let mut line = String::new();

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = Response::err(0, format!("invalid request: {error}"));
                if write_response(&mut *writer, &response).is_err() {
                    break;
                }
                line.clear();
                continue;
            }
        };
        let subscribe = request.method == "subscribe";
        let (respond_tx, respond_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        if commands
            .send(Command {
                request,
                respond: respond_tx,
                subscriber: subscribe.then_some(event_tx),
                written: Some(written_rx),
            })
            .is_err()
        {
            break;
        }
        let response = match respond_rx.recv_timeout(Duration::from_secs(300)) {
            Ok(response) => response,
            Err(_) => Response::err(0, "daemon request timed out"),
        };
        let accepted = response.ok;
        if write_response(&mut *writer, &response).is_err() {
            break;
        }
        let _ = written_tx.send(());
        if subscribe {
            if !accepted {
                break;
            }
            loop {
                match event_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(event) => {
                        if serde_json::to_writer(&mut *writer, &event).is_err()
                            || writer.write_all(b"\n").is_err()
                            || writer.flush().is_err()
                        {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            break;
        }
        line.clear();
    }
}

fn write_response(writer: &mut Conn, response: &Response) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, response).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn handle_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    match request.method.as_str() {
        "ping" => Ok(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")})),
        "probe" => {
            let url = request
                .url
                .ok_or_else(|| DaemonError::Protocol("probe needs url".into()))?;
            let outcomes = state
                .manager
                .probe(&url)
                .into_iter()
                .map(|outcome| {
                    json!({
                        "plugin_id": outcome.plugin_id,
                        "supported": outcome.result.as_ref().map(|result| result.supported),
                        "priority": outcome.result.as_ref().map(|result| result.priority),
                        "filename": outcome.result.as_ref().and_then(|result| result.suggested_filename.clone()),
                        "size": outcome.result.as_ref().and_then(|result| result.metadata.as_ref().and_then(|metadata| metadata.size)),
                        "reason": outcome.result.as_ref().and_then(|result| result.reason.clone()),
                        "error": outcome.error,
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "outcomes": outcomes }))
        }
        "add" => add_task(state, request),
        "list" => {
            let limit = request
                .settings
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(1, 10_000) as usize;
            let include_files = request
                .settings
                .get("include_files")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let tasks = state.store.list_tasks_with_files(limit, include_files)?;
            // The UI needs the default root to show a path for tasks that use
            // it (records only carry non-default destinations).
            Ok(json!({
                "tasks": tasks,
                "download_dir": state.download_dir.to_string_lossy(),
            }))
        }
        "get" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            Ok(json!({ "task": task }))
        }
        "logs" => {
            let task_id = task_id(&request)?;
            // 404-style error keeps the UI honest when a task was deleted.
            state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            let limit = request
                .settings
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(200)
                .clamp(1, 1000) as usize;
            Ok(json!({ "task_id": task_id, "logs": state.store.logs(&task_id, limit)? }))
        }
        "pause" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            match task.state {
                TaskState::Running => {
                    let _ = state.manager.send(
                        &task.plugin_id,
                        &HostCommand::Pause {
                            task_id: task_id.clone(),
                        },
                    );
                    state.store.set_state(&task_id, TaskState::Paused)?;
                    state.active.remove(&task_id);
                    let _ = state
                        .store
                        .append_log(&task_id, "info", "paused by user", None);
                    broadcast(
                        state,
                        json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                    );
                    Ok(json!({"task_id": task_id, "state": "paused"}))
                }
                // A queued task is not executing anything; holding it just
                // keeps it out of the promotion path.
                TaskState::Queued => {
                    state.store.set_state(&task_id, TaskState::Paused)?;
                    let _ = state
                        .store
                        .append_log(&task_id, "info", "paused by user", None);
                    broadcast(
                        state,
                        json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                    );
                    Ok(json!({"task_id": task_id, "state": "paused"}))
                }
                other => Err(DaemonError::Protocol(format!(
                    "cannot pause a {} task",
                    other.as_str()
                ))),
            }
        }
        "resume" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            match task.state {
                TaskState::Running => Ok(json!({"task_id": task_id, "state": "running"})),
                TaskState::Completed => Err(DaemonError::Protocol("task already completed".into())),
                _ if state.canceling.contains(&task_id) => Err(DaemonError::Protocol(
                    "cancel is still in progress; retry shortly".into(),
                )),
                _ => {
                    // A manual resume is an explicit "try again": reset the
                    // automatic retry bookkeeping (in memory and in the store).
                    state.retries.remove(&task_id);
                    let _ = state.store.reset_retry(&task_id);
                    if state.live.contains(&task_id) {
                        // The plugin still owns a live (paused or stopping)
                        // execution; resume it in place.
                        let _ = state.manager.send(
                            &task.plugin_id,
                            &HostCommand::Resume {
                                task_id: task_id.clone(),
                            },
                        );
                        state.store.set_state(&task_id, TaskState::Running)?;
                        state.active.insert(task_id.clone());
                        let _ = state
                            .store
                            .append_log(&task_id, "info", "resumed by user", None);
                        broadcast(
                            state,
                            json!({"event": "task_state", "task_id": task_id, "state": "running"}),
                        );
                        return Ok(json!({"task_id": task_id, "state": "running"}));
                    }
                    match start_from_record(state, &task) {
                        Ok(started) => {
                            if !started {
                                let _ = state.store.set_state(&task_id, TaskState::Queued);
                                let _ = state.store.append_log(
                                    &task_id,
                                    "info",
                                    "resumed by user",
                                    None,
                                );
                                broadcast(
                                    state,
                                    json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
                                );
                            }
                            Ok(json!({
                                "task_id": task_id,
                                "state": if started { "running" } else { "queued" },
                            }))
                        }
                        Err(error) => Err(error),
                    }
                }
            }
        }
        "save_session" => save_session(state),
        "task_summary" => {
            let summary = state.store.summary()?;
            let mut counts = serde_json::Map::new();
            let mut download_speed_bps = 0_u64;
            for (task_state, (count, speed)) in summary {
                if task_state == "running" {
                    download_speed_bps = download_speed_bps.saturating_add(speed);
                }
                counts.insert(task_state, json!(count));
            }
            Ok(json!({
                "counts": counts,
                "download_speed_bps": download_speed_bps,
            }))
        }
        "set_task_limit" => set_task_limit(state, request),
        "queued_order" => Ok(json!({ "tasks": state.store.waiting_order()? })),
        "reorder_queue" => reorder_queue(state, request),
        "get_peers" => get_peers(state, request),
        "cancel" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            if task.state == TaskState::Completed {
                return Err(DaemonError::Protocol(format!(
                    "task {task_id} is already completed"
                )));
            }
            // Canceled is idempotent; failed tasks can be canceled to stop the
            // automatic retry cycle.
            if state.canceling.contains(&task_id) {
                return Ok(json!({"task_id": task_id, "state": "canceled"}));
            }
            if state.live.contains(&task_id) {
                let _ = state.manager.send(
                    &task.plugin_id,
                    &HostCommand::Cancel {
                        task_id: task_id.clone(),
                    },
                );
            }
            state.canceling.insert(task_id.clone());
            state.store.set_state(&task_id, TaskState::Canceled)?;
            let _ = state.store.reset_retry(&task_id);
            let _ = state
                .store
                .append_log(&task_id, "info", "canceled by user", None);
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.retries.remove(&task_id);
            state.details.remove(&task_id);
            state.last_progress.remove(&task_id);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "canceled"}),
            );
            promote_queued(state);
            Ok(json!({"task_id": task_id, "state": "canceled"}))
        }
        "delete" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            if !task.state.is_terminal() {
                return Err(DaemonError::Protocol(
                    "cancel the task before deleting it".into(),
                ));
            }
            // Staging/resume state is daemon-owned; finalized files live in the
            // download directory and survive the deletion.
            let centralized = state.download_dir.join(".aether").join(&task_id);
            let _ = fs::remove_dir_all(&centralized);
            cleanup_staging_parent(&centralized);
            let final_dir = destination_dir(state, task.target_dir.as_deref());
            let target_staging = final_dir.join(".aether-staging").join(&task_id);
            let _ = fs::remove_dir_all(&target_staging);
            cleanup_staging_parent(&target_staging);
            state.details.remove(&task_id);
            state.last_progress.remove(&task_id);
            state.store.delete_task(&task_id)?;
            broadcast(state, json!({"event": "task_deleted", "task_id": task_id}));
            Ok(json!({"task_id": task_id, "deleted": true}))
        }
        "resolve_proxy" => {
            let url = request
                .url
                .ok_or_else(|| DaemonError::Protocol("resolve_proxy needs url".into()))?;
            let pac = state.pac.is_some();
            let proxy = resolve_task_proxy(state, &url, true);
            let error = state.pac.as_ref().and_then(PacResolver::last_error);
            Ok(json!({ "url": url, "pac": pac, "proxy": proxy, "pac_error": error }))
        }
        "plugins" => {
            let allow_external = state.manager.allow_external_process();
            let backup_root = state.data_dir.join("plugin-backups");
            let service_status: HashMap<String, ServiceStatus> = state
                .services
                .statuses()
                .into_iter()
                .map(|status| (status.service.clone(), status))
                .collect();
            let plugins = state
                .manager
                .registry()
                .iter()
                .map(|entry| {
                    let service = entry
                        .manifest
                        .service
                        .as_deref()
                        .and_then(|service| service_status.get(service));
                    let running = service.map_or_else(
                        || state.manager.running_pid(&entry.manifest.id).is_some(),
                        |status| status.running,
                    );
                    // A policy-blocked plugin is still listed (and can still
                    // be disabled/uninstalled) but the UI must say why it
                    // cannot start instead of failing at task time.
                    let blocked = !allow_external
                        && entry
                            .manifest
                            .capabilities
                            .contains(&aether_plugin_sdk::Capability::ExternalProcess);
                    json!({
                        "id": entry.manifest.id,
                        "name": entry.manifest.name,
                        "version": entry.manifest.version,
                        "protocols": entry.manifest.protocols,
                        "capabilities": entry.manifest.capabilities,
                        "enabled": entry.enabled,
                        "blocked": blocked,
                        "service": entry.manifest.kind == PluginKind::Service,
                        "running": running,
                        "last_error": service.and_then(|status| status.last_error.clone()),
                        "has_backup": backup_root
                            .join(&entry.manifest.id)
                            .join("plugin.json")
                            .is_file(),
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "plugins": plugins,
                "external_process_allowed": allow_external,
            }))
        }
        "set_plugin_enabled" => {
            let plugin_id = request
                .plugin
                .clone()
                .ok_or_else(|| DaemonError::Protocol("set_plugin_enabled needs plugin".into()))?;
            let enabled = request
                .settings
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| {
                    DaemonError::Protocol("set_plugin_enabled needs settings.enabled".into())
                })?;
            if !enabled {
                // Refuse to pull the rug from under running work.
                let busy = state.active.iter().any(|task_id| {
                    state
                        .store
                        .task(task_id)
                        .ok()
                        .flatten()
                        .is_some_and(|task| task.plugin_id == plugin_id)
                });
                if busy {
                    return Err(DaemonError::Protocol(format!(
                        "plugin {plugin_id} still has running tasks"
                    )));
                }
            }
            state
                .manager
                .registry_mut()
                .set_enabled(&plugin_id, enabled)?;
            let is_service = state
                .manager
                .registry()
                .get(&plugin_id)
                .is_some_and(|entry| entry.manifest.kind == PluginKind::Service);
            if !enabled {
                state.manager.stop(&plugin_id);
                state.live.retain(|task_id| {
                    state
                        .store
                        .task(task_id)
                        .ok()
                        .flatten()
                        .is_none_or(|task| task.plugin_id != plugin_id)
                });
            }
            if is_service {
                state.services.sync(state.manager.registry());
            }
            state.store.set_plugin_disabled(&plugin_id, !enabled)?;
            let entry = state
                .manager
                .registry()
                .get(&plugin_id)
                .map(|entry| {
                    json!({
                        "id": entry.manifest.id,
                        "enabled": entry.enabled,
                    })
                })
                .unwrap_or(serde_json::Value::Null);
            Ok(json!({ "plugin": entry }))
        }
        "stats" => {
            let now = chrono::Local::now();
            let today_start = now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .map(|naive| {
                    naive
                        .and_local_timezone(chrono::Local)
                        .single()
                        .map(|local| local.timestamp_millis())
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let week_start = today_start - 7 * 24 * 3600 * 1000;
            let (today_bytes, today_files) = state.store.completed_since(today_start)?;
            let (week_bytes, week_files) = state.store.completed_since(week_start)?;
            // Per-day bars and per-type split come from one file-level query.
            let files = state.store.completed_files_since(week_start)?;
            let tz_offset = chrono::Local::now().offset().local_minus_utc() as i64 * 1000;
            let day_ms = 24 * 3600 * 1000;
            let today_index = ((today_start + tz_offset) / day_ms) as usize;
            let first_index = today_index - 6; // slots: 6 days ago … today
            let mut daily = [0_u64; 7];
            let mut categories = [0_u64; 5];
            for (completed_at, size, relative_path) in &files {
                let day = ((*completed_at + tz_offset) / day_ms) as usize;
                if day >= first_index {
                    let slot = (day - first_index).min(6);
                    daily[slot] += size;
                }
                categories[file_category(relative_path)] += size;
            }
            // Weekday glyphs for the daily slots, today last.
            let day_labels: Vec<String> = {
                let glyphs = ["一", "二", "三", "四", "五", "六", "日"];
                let today =
                    chrono::Datelike::weekday(&chrono::Local::now()).num_days_from_monday() as i64;
                (0..7)
                    .map(|back| {
                        let weekday = (today - (6 - back)).rem_euclid(7) as usize;
                        glyphs[weekday].to_string()
                    })
                    .collect()
            };
            let (completed, failed) = state.store.outcome_counts()?;
            let (disk_total, disk_free) = match (
                fs2::total_space(&state.download_dir),
                fs2::available_space(&state.download_dir),
            ) {
                (Ok(total), Ok(free)) => (total, free),
                _ => (0, 0),
            };
            let queued = state
                .store
                .list_tasks(1000)?
                .into_iter()
                .filter(|task| task.state == TaskState::Queued)
                .count() as u64;
            let registry = state.manager.registry();
            Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "download_dir": state.download_dir.to_string_lossy(),
                "plugins_dir": state.plugins_dir.to_string_lossy(),
                "data_dir": state.data_dir.join("aether.db").to_string_lossy(),
                "disk": { "total": disk_total, "free": disk_free },
                "today": { "bytes": today_bytes, "files": today_files },
                "week": { "bytes": week_bytes, "files": week_files },
                "daily": daily,
                "daily_labels": day_labels,
                "categories": {
                    "video": categories[0],
                    "archive": categories[1],
                    "app": categories[2],
                    "doc": categories[3],
                    "other": categories[4],
                },
                "tasks": {
                    "completed": completed,
                    "failed": failed,
                    "active": state.active.len(),
                    "queued": queued,
                },
                "plugins": {
                    "total": registry.len(),
                    "enabled": registry.iter().filter(|entry| entry.enabled).count(),
                },
                // App-update status, so the About panel shows the same state
                // the background loop is in. `null` = no update source.
                "update": state.update.as_ref().map(|status| {
                    status.lock().unwrap().clone()
                }),
            }))
        }
        "db_check" => Ok(json!({ "result": state.store.integrity_check()? })),
        "backup" => {
            let path = request
                .settings
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| DaemonError::Protocol("backup needs a path".into()))?;
            state.store.backup_to(path)?;
            Ok(json!({ "path": path }))
        }
        "get_settings" => Ok(json!({ "settings": state.settings })),
        "set_settings" => {
            // Partial update: only keys that are present change. `proxy: null`
            // clears the proxy and falls back to the plugin default; blank
            // user_agent/dns input clears those knobs the same way. The limits
            // need a concrete number instead — `null` is rejected below.
            let patch = request
                .settings
                .as_object()
                .ok_or_else(|| DaemonError::Protocol("set_settings needs an object".into()))?;
            // The file is parsed with `deny_unknown_fields`, so the RPC
            // rejects the same typos instead of storing nothing quietly.
            for key in patch.keys() {
                if !KEYS.contains(&key.as_str()) {
                    return Err(DaemonError::Protocol(format!("unknown setting {key}")));
                }
            }

            // Parse and validate the whole patch before committing a single
            // field: a rejected patch leaves the running settings untouched.
            let mut next = state.settings.clone();
            if let Some(proxy) = patch.get("proxy") {
                let proxy: Option<ProxySetting> =
                    serde_json::from_value(proxy.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid proxy setting: {error}"))
                    })?;
                if let Some(proxy) = &proxy {
                    proxy.validate().map_err(DaemonError::Protocol)?;
                }
                next.proxy = proxy;
            }
            if let Some(user_agent) = patch.get("user_agent") {
                let user_agent: Option<String> = serde_json::from_value(user_agent.clone())
                    .map_err(|error| {
                        DaemonError::Protocol(format!("invalid user_agent: {error}"))
                    })?;
                next.user_agent = user_agent.filter(|agent| !agent.trim().is_empty());
            }
            if let Some(ip_mode) = patch.get("ip_mode") {
                next.ip_mode = serde_json::from_value(ip_mode.clone())
                    .map_err(|error| DaemonError::Protocol(format!("invalid ip_mode: {error}")))?;
            }
            if let Some(dns) = patch.get("dns") {
                let dns: Option<String> = serde_json::from_value(dns.clone())
                    .map_err(|error| DaemonError::Protocol(format!("invalid dns: {error}")))?;
                let dns = dns.filter(|servers| !servers.trim().is_empty());
                if let Some(servers) = &dns {
                    aether_plugin_sdk::NetworkConfig::parse_dns_servers(servers)
                        .map_err(DaemonError::Protocol)?;
                }
                next.dns = dns;
            }
            if let Some(value) = patch.get("max_concurrent") {
                next.max_concurrent = Some(parse_limit(value, "max_concurrent")?);
            }
            if let Some(value) = patch.get("max_speed_bps") {
                next.max_speed_bps = Some(parse_limit(value, "max_speed_bps")?);
            }
            if let Some(value) = patch.get("connections") {
                next.connections = Some(parse_limit(value, "connections")?);
            }
            if let Some(value) = patch.get("retry_max") {
                next.retry_max = Some(parse_limit(value, "retry_max")?);
            }
            if let Some(value) = patch.get("default_headers") {
                let headers: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid default_headers: {error}"))
                    })?;
                next.default_headers = headers.filter(|list| !list.trim().is_empty());
            }
            if let Some(value) = patch.get("ftp_passive") {
                next.ftp_passive = serde_json::from_value(value.clone()).map_err(|error| {
                    DaemonError::Protocol(format!("invalid ftp_passive: {error}"))
                })?;
            }
            if let Some(value) = patch.get("bt_listen_port") {
                next.bt_listen_port = serde_json::from_value(value.clone()).map_err(|error| {
                    DaemonError::Protocol(format!("invalid bt_listen_port: {error}"))
                })?;
            }
            if let Some(value) = patch.get("bt_dht") {
                next.bt_dht = serde_json::from_value(value.clone())
                    .map_err(|error| DaemonError::Protocol(format!("invalid bt_dht: {error}")))?;
            }
            if let Some(value) = patch.get("bt_upload_bps") {
                next.bt_upload_bps = serde_json::from_value(value.clone()).map_err(|error| {
                    DaemonError::Protocol(format!("invalid bt_upload_bps: {error}"))
                })?;
            }
            if let Some(value) = patch.get("bt_blocklist") {
                let blocklist: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid bt_blocklist: {error}"))
                    })?;
                next.bt_blocklist = blocklist.filter(|path| !path.trim().is_empty());
            }
            if let Some(value) = patch.get("bt_trackers") {
                let trackers: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid bt_trackers: {error}"))
                    })?;
                next.bt_trackers = trackers.filter(|list| !list.trim().is_empty());
            }
            if let Some(value) = patch.get("schedule_on") {
                next.schedule_on = serde_json::from_value(value.clone()).map_err(|error| {
                    DaemonError::Protocol(format!("invalid schedule_on: {error}"))
                })?;
            }
            if let Some(value) = patch.get("schedule_start") {
                let text: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid schedule_start: {error}"))
                    })?;
                next.schedule_start = text.filter(|text| !text.trim().is_empty());
            }
            if let Some(value) = patch.get("schedule_end") {
                let text: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid schedule_end: {error}"))
                    })?;
                next.schedule_end = text.filter(|text| !text.trim().is_empty());
            }
            if let Some(value) = patch.get("rpc_listen") {
                let text: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid rpc_listen: {error}"))
                    })?;
                // `Some("")` is an explicit "off"; `None` (null) returns to
                // the built-in default on the next boot.
                next.rpc_listen = text.map(|text| text.trim().to_string());
            }
            if let Some(value) = patch.get("rpc_secret") {
                let text: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid rpc_secret: {error}"))
                    })?;
                // Same distinction: `Some("")` explicitly disables the token,
                // `None` regenerates the default on the next boot.
                next.rpc_secret = text.map(|text| text.trim().to_string());
            }
            if let Some(value) = patch.get("ffmpeg_path") {
                let text: Option<String> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid ffmpeg_path: {error}"))
                    })?;
                next.ffmpeg_path = text.filter(|text| !text.trim().is_empty());
            }
            if let Some(value) = patch.get("plugin_auto_update") {
                next.plugin_auto_update =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid plugin_auto_update: {error}"))
                    })?;
            }
            if let Some(value) = patch.get("plugin_pins") {
                let pins: Option<std::collections::BTreeMap<String, String>> =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid plugin_pins: {error}"))
                    })?;
                next.plugin_pins = pins.filter(|pins| !pins.is_empty());
            }
            if let Some(value) = patch.get("plugin_external_process") {
                next.plugin_external_process =
                    serde_json::from_value(value.clone()).map_err(|error| {
                        DaemonError::Protocol(format!("invalid plugin_external_process: {error}"))
                    })?;
            }
            if let Some(value) = patch.get("app_auto_update") {
                next.app_auto_update = serde_json::from_value(value.clone()).map_err(|error| {
                    DaemonError::Protocol(format!("invalid app_auto_update: {error}"))
                })?;
            }
            next.validate()?;

            let network_before = state.settings.network();
            let proxy_changed = patch.contains_key("proxy");
            let external_process_before = state.settings.plugin_external_process.unwrap_or(true);
            state.settings = next;
            let external_process_after = state.settings.plugin_external_process.unwrap_or(true);
            state
                .manager
                .set_allow_external_process(external_process_after);
            if external_process_before && !external_process_after {
                // The policy only gates the next spawn; retire idle plugins
                // that need an external process so the setting takes effect
                // without a daemon restart. Busy plugins finish their task.
                stop_idle_external_plugins(state);
            }
            if proxy_changed {
                // Applies to plugins spawned from now on; running downloads
                // keep the proxy they started with. PAC is evaluated per task,
                // so a fresh resolver is enough.
                state.pac = state
                    .settings
                    .proxy
                    .as_ref()
                    .and_then(ProxySetting::resolver);
                state
                    .manager
                    .set_proxy(init_proxy(state.settings.proxy.as_ref()));
                // Re-detect the desktop proxy for the new mode on next use.
                state.system_proxy = SystemProxyCache::default();
                state.system_pac = None;
            }
            apply_limits(state);
            // A raised cap starts waiting tasks immediately; a lowered one
            // lets the running ones finish — nothing is killed.
            promote_queued(state);
            // The RPC token is read live by the accept loop, so a rotation
            // is effective for the next connection (no listener restart).
            if patch.contains_key("rpc_secret") {
                *state.rpc_secret.lock().unwrap() = state
                    .settings
                    .rpc_secret
                    .clone()
                    .filter(|secret| !secret.trim().is_empty());
            }
            // A schedule change takes effect now, not at the next periodic
            // tick: closing the window pauses, opening resumes.
            if patch.keys().any(|key| key.starts_with("schedule_")) {
                tick_schedule(state);
            }
            // New network values reach plugins on their next spawn; running
            // downloads keep the clients they were started with. Idle plugin
            // processes are stopped right away so the very next task uses the
            // new configuration instead of waiting for a daemon restart.
            let network_after = state.settings.network();
            if network_before != network_after {
                restart_idle_plugins(state);
            }
            state.manager.set_network(network_after);
            state.settings.save(&state.settings_path)?;
            broadcast(
                state,
                json!({"event": "settings_changed", "settings": state.settings}),
            );
            Ok(json!({ "settings": state.settings }))
        }
        "install_plugin" => install_plugin(state, request),
        "uninstall_plugin" => uninstall_plugin(state, request),
        "rollback_plugin" => rollback_plugin(state, request),
        "rescan_plugins" => {
            let total = rescan_registry(state)?;
            state.services.sync(state.manager.registry());
            Ok(json!({ "plugins": total }))
        }
        "plugin_updates" => plugin_updates(state),
        "plugin_update_all" => plugin_update_all(state),
        "update_status" => Ok(state.update.as_ref().map_or_else(
            || json!({"status": "disabled"}),
            |status| status.lock().unwrap().clone(),
        )),
        // Run one check (and, unless the user turned background downloads
        // off, download/apply the release) off the main loop: fetching the
        // manifest must not stall downloads. The result lands in `stats`.
        "update_check" => {
            let (Some(status), Some(url), Some(key)) = (
                state.update.clone(),
                state.update_url.clone(),
                state.plugin_public_key.clone(),
            ) else {
                return Ok(json!({"status": "disabled"}));
            };
            *status.lock().unwrap() = json!({"status": "checking"});
            let appimage = state.update_appimage.clone();
            let data_dir = state.data_dir.clone();
            let settings_path = state.settings_path.clone();
            thread::spawn(move || {
                update_pass(
                    &url,
                    &key,
                    appimage.as_deref(),
                    &data_dir,
                    &settings_path,
                    &status,
                );
            });
            Ok(json!({"status": "checking"}))
        }
        "shutdown" => Ok(json!({"ok": true})),
        other => Err(DaemonError::Protocol(format!("unknown method {other}"))),
    }
}

/// Re-scan `plugins_dir` and replace the registry, re-applying the
/// persisted disable list. Running plugin processes are left alone: they
/// keep serving their running tasks, and the next spawn picks up whatever
/// the rescan found.
fn rescan_registry(state: &mut State) -> Result<usize, DaemonError> {
    let registry = PluginRegistry::discover(&[state.plugins_dir.clone()])?;
    *state.manager.registry_mut() = registry;
    for plugin_id in state.store.disabled_plugins()? {
        let _ = state.manager.registry_mut().set_enabled(&plugin_id, false);
    }
    Ok(state.manager.registry().iter().count())
}

/// Install a plugin package from a local directory into `plugins_dir`.
///
/// Order matters: validate everything (identity, API, host compatibility,
/// executable path safety, signature, file hashes) *before* touching the
/// installed copy, then swap in via a staging directory so a failed copy
/// cannot leave a half-installed package behind.
fn install_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let path = request
        .settings
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| DaemonError::Protocol("install_plugin needs settings.path".into()))?;
    install_package(state, Path::new(path))
}

/// True while a task owned by `plugin_id` is still running. Disabling,
/// replacing, rolling back or uninstalling a plugin must never pull the
/// binary out from under a live download.
fn plugin_is_busy(state: &State, plugin_id: &str) -> bool {
    state.active.iter().any(|task_id| {
        state
            .store
            .task(task_id)
            .ok()
            .flatten()
            .is_some_and(|task| task.plugin_id == plugin_id)
    })
}

/// Remove an installed plugin package. Refuses while its tasks are running
/// (the same rule as disable/install), deletes the package and the retained
/// rollback copy, and forgets the persisted disabled flag so a later reinstall
/// starts enabled.
fn uninstall_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("uninstall_plugin needs plugin".into()))?;
    let entry = state
        .manager
        .registry()
        .get(&plugin_id)
        .ok_or_else(|| DaemonError::Protocol(format!("plugin {plugin_id} is not installed")))?;
    // Canonicalize both sides: a symlinked package must not let the delete
    // escape the plugins directory. The registry only accepts direct child
    // packages, and `plugin_id` is validated by the manifest parser.
    let canonical_root = fs::canonicalize(&state.plugins_dir)?;
    let target = fs::canonicalize(&entry.root)?;
    if target.parent() != Some(canonical_root.as_path()) {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} does not live directly under the plugins directory"
        )));
    }
    if plugin_is_busy(state, &plugin_id) {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} still has running tasks"
        )));
    }
    state.manager.stop(&plugin_id);
    state.live.retain(|task_id| {
        state
            .store
            .task(task_id)
            .ok()
            .flatten()
            .is_none_or(|task| task.plugin_id != plugin_id)
    });
    fs::remove_dir_all(&target)?;
    let _ = fs::remove_dir_all(state.data_dir.join("plugin-backups").join(&plugin_id));
    state.store.set_plugin_disabled(&plugin_id, false)?;
    rescan_registry(state)?;
    state.services.sync(state.manager.registry());
    Ok(json!({ "plugin_id": plugin_id, "removed": true }))
}

/// Reinstall the retained previous version. The backup goes through the same
/// verified install path as a fresh package (manifest, host range, signature
/// and file hashes), and the currently installed copy becomes the new
/// backup — rollback is repeatable, not destructive.
fn rollback_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("rollback_plugin needs plugin".into()))?;
    let backup = state.data_dir.join("plugin-backups").join(&plugin_id);
    if !backup.join("plugin.json").is_file() {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} has no retained previous version"
        )));
    }
    let mut value = install_package(state, &backup)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("rolled_back".into(), Value::Bool(true));
    }
    Ok(value)
}

/// An update client for the plugin channel, or `None` when the daemon was
/// not configured with both the manifest URL and the release key (the same
/// trust root the app-update loop uses).
fn plugin_update_client(state: &State) -> Option<aether_update::UpdateClient> {
    let url = state.update_url.as_deref()?;
    let key = state.plugin_public_key.as_deref()?;
    aether_update::UpdateClient::new(
        url,
        key,
        &state.host_version,
        aether_plugin_sdk::current_platform_key(),
        &state.data_dir.join("plugin-updates"),
    )
    .ok()
}

/// Compare installed plugin versions against the update manifest.
fn plugin_updates(state: &State) -> Result<Value, DaemonError> {
    let Some(client) = plugin_update_client(state) else {
        // Honest about the precondition instead of reporting "no updates":
        // nothing was checked.
        return Ok(json!({ "configured": false, "updates": [] }));
    };
    let manifest = client
        .check_manifest()
        .map_err(|error| DaemonError::Protocol(format!("update manifest: {error}")))?;
    let installed: std::collections::HashMap<String, String> = state
        .manager
        .registry()
        .iter()
        .map(|entry| (entry.manifest.id.clone(), entry.manifest.version.clone()))
        .collect();
    let pins = state.settings.plugin_pins.clone().unwrap_or_default();
    let pinned = manifest
        .plugins
        .keys()
        .filter(|id| pins.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    let updates = manifest
        .plugins
        .iter()
        .filter(|(id, entry)| {
            !pins.contains_key(*id)
                && installed.get(*id).map(String::as_str) != Some(entry.version.as_str())
        })
        .map(|(id, entry)| {
            json!({
                "plugin_id": id,
                "installed": installed.get(id),
                "latest": entry.version,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "configured": true, "updates": updates, "pinned": pinned }))
}

/// Download, verify and install every outdated plugin from the update
/// manifest. Each plugin is independent: one failure (busy, bad archive,
/// signature) is reported and the rest continue.
fn plugin_update_all(state: &mut State) -> Result<Value, DaemonError> {
    let Some(client) = plugin_update_client(state) else {
        return Err(DaemonError::Protocol(
            "plugin updates need update_url and the release key configured".into(),
        ));
    };
    let manifest = client
        .check_manifest()
        .map_err(|error| DaemonError::Protocol(format!("update manifest: {error}")))?;
    let installed: std::collections::HashMap<String, String> = state
        .manager
        .registry()
        .iter()
        .map(|entry| (entry.manifest.id.clone(), entry.manifest.version.clone()))
        .collect();

    let pins = state.settings.plugin_pins.clone().unwrap_or_default();
    let mut updated = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    for (id, entry) in &manifest.plugins {
        if pins.contains_key(id) {
            skipped.push(id.clone());
            continue;
        }
        if installed.get(id).map(String::as_str) == Some(entry.version.as_str()) {
            continue;
        }
        match update_one_plugin(state, &client, id, entry) {
            Ok(()) => updated.push(id.clone()),
            Err(error) => failed.push(json!({ "plugin_id": id, "error": error.to_string() })),
        }
    }
    Ok(json!({
        "updated": updated,
        "failed": failed,
        "skipped_pinned": skipped,
        "up_to_date": updated.is_empty() && failed.is_empty(),
    }))
}

fn update_one_plugin(
    state: &mut State,
    client: &aether_update::UpdateClient,
    id: &str,
    entry: &aether_update::PluginUpdateEntry,
) -> Result<(), DaemonError> {
    let staging_root = state
        .data_dir
        .join("plugin-updates")
        .join(format!("staging-{id}"));
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root)?;
    }
    let archive = client
        .download_artifact(
            &entry.url,
            &entry.sha256,
            entry.size,
            &state.data_dir.join("plugin-updates"),
            &format!("{id}.tar.gz"),
        )
        .map_err(|error| DaemonError::Protocol(format!("download {id}: {error}")))?;
    let unpacked = staging_root.join("pkg");
    let unpack = aether_update::unpack_plugin_archive(&archive, &unpacked, 256 * 1024 * 1024);
    if let Err(error) = unpack {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(DaemonError::Protocol(error.to_string()));
    }
    let result = install_package(state, &unpacked);
    let _ = fs::remove_dir_all(&staging_root);
    result.map(|_| ())
}

/// Install one verified package directory into `plugins_dir` (shared by the
/// `install_plugin` RPC and the update channel's unpacked downloads).
fn install_package(state: &mut State, package: &Path) -> Result<Value, DaemonError> {
    let package = package;
    let manifest_path = package.join("plugin.json");
    let raw = fs::read_to_string(&manifest_path).map_err(|error| {
        DaemonError::Protocol(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let manifest_value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;

    // Structural validation: identity, executable map, host bounds, and the
    // current platform's executable existing inside the package with no
    // path traversal (resolve_executable canonicalizes and containment-checks).
    let manifest = aether_plugin_sdk::manifest::PluginManifest::from_json(&raw)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;
    manifest
        .validate()
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;
    if manifest.api != aether_plugin_sdk::API_VERSION {
        return Err(DaemonError::Protocol(format!(
            "plugin {} targets API {} but the host speaks API {}",
            manifest.id,
            manifest.api,
            aether_plugin_sdk::API_VERSION
        )));
    }
    if !manifest
        .is_compatible_with_host(&state.host_version)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?
    {
        return Err(DaemonError::Protocol(format!(
            "plugin {} is not compatible with host {}",
            manifest.id, state.host_version
        )));
    }
    manifest
        .resolve_executable(package)
        .map_err(|error| DaemonError::Protocol(format!("executable: {error}")))?;

    // Signature policy: a configured release key makes a valid signature
    // mandatory; without a key, unsigned installs need the explicit dev
    // escape hatch (PLUGIN_ARCHITECTURE: dev mode allows local unsigned
    // plugins, production refuses them).
    match state.plugin_public_key.as_deref() {
        Some(key) => {
            aether_update::verify_manifest_signature(&manifest_value, key)
                .map_err(|error| DaemonError::Protocol(format!("signature: {error}")))?;
            aether_update::verify_package_files(package, &manifest_value)
                .map_err(|error| DaemonError::Protocol(format!("integrity: {error}")))?;
        }
        None => {
            let allowed = std::env::var("AETHER_ALLOW_UNSIGNED_PLUGINS")
                .map(|value| value.trim() == "1")
                .unwrap_or(false);
            if !allowed {
                return Err(DaemonError::Protocol(
                    "this daemon has no plugin signing key configured; unsigned plugins need                      AETHER_ALLOW_UNSIGNED_PLUGINS=1 (development only)"
                        .into(),
                ));
            }
        }
    }

    // Refuse to replace a plugin whose tasks are still running (same rule
    // as disabling), so the old binary is never pulled from under work.
    let busy = state.active.iter().any(|task_id| {
        state
            .store
            .task(task_id)
            .ok()
            .flatten()
            .is_some_and(|task| task.plugin_id == manifest.id)
    });
    if busy {
        return Err(DaemonError::Protocol(format!(
            "plugin {} still has running tasks",
            manifest.id
        )));
    }

    let existed = state.manager.registry().get(&manifest.id).is_some();
    if existed {
        // Idle by the check above: retire the old process so the next spawn
        // picks up the freshly installed binary instead of the old one.
        state.manager.stop(&manifest.id);
    }

    // Stage, then swap: copy failure leaves the installed copy untouched.
    let target = state.plugins_dir.join(&manifest.id);
    let staging =
        state
            .plugins_dir
            .join(format!(".staging-{}-{}", manifest.id, std::process::id()));
    let retired = state.plugins_dir.join(format!(".retired-{}", manifest.id));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    copy_dir(package, &staging)?;
    // Keep exactly one previous version outside `plugins_dir` (a backup
    // directory inside the scan root would be discovered as a duplicate
    // plugin id). `rollback_plugin` installs it through this same verified
    // path, so a rollback cannot bypass signature/integrity checks.
    if target.is_dir() {
        let backup_root = state.data_dir.join("plugin-backups");
        fs::create_dir_all(&backup_root)?;
        let backup = backup_root.join(&manifest.id);
        let backup_staging =
            backup_root.join(format!(".tmp-{}-{}", manifest.id, std::process::id()));
        if backup_staging.exists() {
            fs::remove_dir_all(&backup_staging)?;
        }
        copy_dir(&target, &backup_staging)?;
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&backup_staging, &backup)?;
    }
    if let Err(error) = fs::rename(&target, &retired) {
        if target.exists() {
            fs::remove_dir_all(&staging)?;
            return Err(DaemonError::Io(error));
        }
    }
    if let Err(error) = fs::rename(&staging, &target) {
        // Roll the previous version back so a failed swap is not a delete.
        let _ = fs::rename(&retired, &target);
        let _ = fs::remove_dir_all(&staging);
        return Err(DaemonError::Io(error));
    }
    let _ = fs::remove_dir_all(&retired);

    rescan_registry(state)?;
    state.services.sync(state.manager.registry());
    Ok(json!({
        "plugin_id": manifest.id,
        "version": manifest.version,
        "replaced": existed,
    }))
}

/// Recursive copy for plugin packages; follows no symlinks (they would not
/// survive a signature check anyway) and preserves nothing but bytes.
fn copy_dir(from: &Path, to: &Path) -> Result<(), DaemonError> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &destination)?;
        }
        // Symlinks/sockets/fifos are skipped: the package must be plain files.
    }
    Ok(())
}

/// Change one task's download limit. Queued tasks pick it up on start;
/// running HTTP tasks receive a live `SetBudget`. BitTorrent's session-wide
/// rate limiter has no per-task switch, so BT is rejected instead of silently
/// ignoring the call.
fn set_task_limit(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let limit = request
        .settings
        .get("max_speed_bps")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    if task.plugin_id == "bt" {
        return Err(DaemonError::Protocol(
            "per-task speed limits are not supported for BitTorrent".into(),
        ));
    }
    let mut settings = task.settings.as_object().cloned().unwrap_or_default();
    if limit == 0 {
        settings.remove("max_speed_bps");
    } else {
        settings.insert("max_speed_bps".into(), json!(limit));
    }
    let settings = Value::Object(settings);
    state.store.update_task_settings(&task_id, &settings)?;
    if state.live.contains(&task_id) {
        let budget = Budget {
            connections: state.settings.connections.unwrap_or(CONNECTIONS_DEFAULT),
            download_bps: if limit > 0 {
                limit
            } else {
                state.max_speed_bps.unwrap_or(0)
            },
            ..Budget::default()
        };
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::SetBudget {
                task_id: task_id.clone(),
                budget,
            },
        );
    }
    Ok(json!({ "task_id": task_id, "settings": settings }))
}

/// Export the queued/running/paused URLs in aria2's session-line format.
/// SQLite remains the restore source; this is a portable snapshot for users
/// and clients that call `aria2.saveSession`.
fn save_session(state: &State) -> Result<Value, DaemonError> {
    let tasks = state.store.list_tasks_with_files(10_000, false)?;
    let active = tasks
        .into_iter()
        .filter(|task| {
            matches!(
                task.state,
                TaskState::Queued | TaskState::Running | TaskState::Paused
            )
        })
        .collect::<Vec<_>>();
    let mut text = String::new();
    for task in &active {
        text.push_str(&task.url);
        text.push('\n');
    }
    let path = state.data_dir.join("aria2-session.txt");
    let temporary = path.with_extension("txt.tmp");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, &path)?;
    Ok(json!({ "path": path, "count": active.len() }))
}

/// Live peer list for the aria2 compatibility layer. The BT plugin pushes
/// peer snapshots through `Detail`; reading the cache keeps this off the
/// daemon's main loop (no plugin round-trip).
fn get_peers(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    let peers = state
        .details
        .get(&task_id)
        .and_then(|detail| detail.get("peers"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    Ok(json!({ "peers": peers }))
}

/// Apply an explicit waiting-queue order. Priorities are spaced by 200 and
/// pre-compensated for the aging bonus (capped at 100), so the requested
/// order survives future aging until the queue drains.
fn reorder_queue(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let order = request
        .settings
        .get("order")
        .and_then(Value::as_array)
        .ok_or_else(|| DaemonError::Protocol("reorder_queue needs settings.order".into()))?;
    let now = now_ms();
    let mut changes = Vec::with_capacity(order.len());
    for (index, id) in order.iter().enumerate() {
        let id = id
            .as_str()
            .ok_or_else(|| DaemonError::Protocol("reorder_queue entries must be strings".into()))?;
        let task = state
            .store
            .task(id)?
            .ok_or_else(|| DaemonError::Protocol(format!("task {id} not found")))?;
        if !matches!(task.state, TaskState::Queued | TaskState::Paused) {
            return Err(DaemonError::Protocol(format!("task {id} is not waiting")));
        }
        let age_minutes = ((now - task.created_at).max(0) / 1000 / 60) as i64;
        let age = (age_minutes / 5).min(100);
        let priority = 1_000_000 - (index as i64 * 200) - age;
        changes.push((id.to_string(), priority as i32));
    }
    state.store.set_priorities(&changes)?;
    promote_queued(state);
    Ok(json!({ "reordered": changes.len() }))
}

fn add_task(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let url = request
        .url
        .ok_or_else(|| DaemonError::Protocol("add needs url".into()))?;
    let task_proxy = resolve_task_proxy(state, &url, true);
    let outcomes = state.manager.probe_with_proxy(&url, task_proxy.clone());
    let chosen = match request.plugin.as_deref() {
        Some(id) => outcomes
            .iter()
            .find(|outcome| outcome.plugin_id == id)
            .and_then(|outcome| outcome.result.as_ref())
            .map(|result| (id.to_string(), result.clone()))
            .ok_or_else(|| DaemonError::Protocol(format!("plugin {id} did not answer probe")))?,
        None => outcomes
            .iter()
            .filter_map(|outcome| {
                outcome
                    .result
                    .as_ref()
                    .filter(|result| result.supported)
                    .map(|result| (outcome.plugin_id.clone(), result.clone()))
            })
            .max_by_key(|(_, result)| result.priority)
            .ok_or_else(|| {
                // Without the per-plugin reasons this error is undiagnosable
                // ("is my URL malformed, or did every plugin error out?").
                let reasons = outcomes
                    .iter()
                    .map(|outcome| {
                        let reason = outcome
                            .error
                            .clone()
                            .or_else(|| outcome.result.as_ref().and_then(|r| r.reason.clone()))
                            .unwrap_or_else(|| "no reason reported".into());
                        format!("{}: {reason}", outcome.plugin_id)
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                DaemonError::Protocol(format!("no plugin supports this URL ({reasons})"))
            })?,
    };

    let (plugin_id, probe) = chosen;
    if !probe.supported {
        return Err(DaemonError::Protocol(format!(
            "{plugin_id} does not support this URL"
        )));
    }

    let task_id = format!(
        "task-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    // Staging lives under the daemon-owned `.aether` tree; finalized files go
    // straight into the destination the client asked for.
    let target_dir = resolve_target_dir(state, request.dir.as_deref())?;
    let final_dir = target_dir.clone();
    fs::create_dir_all(&final_dir)?;
    let staging = prepare_staging(state, &task_id, &final_dir)?;

    // Only non-default destinations are persisted; `None` keeps following the
    // daemon's `--download-dir` if the user moves it.
    let stored_dir =
        (target_dir != state.download_dir).then(|| target_dir.to_string_lossy().into_owned());
    // Fail fast instead of dying halfway through: the probe usually knows the
    // size, and downloads already running into the same directory will keep
    // consuming space, so count their remaining bytes as reserved.
    if let Some(size) = probe.metadata.as_ref().and_then(|metadata| metadata.size) {
        let reserved = reserved_in(state, &target_dir);
        match ensure_space(&target_dir, size.saturating_add(reserved)) {
            Ok(_) => {}
            Err(error) if error.is_shortage() => {
                return Err(DaemonError::Protocol(format!(
                    "{error} (task needs {}, other downloads reserve {})",
                    human_bytes(size),
                    human_bytes(reserved)
                )))
            }
            // statvfs can fail on exotic mounts; not a reason to block.
            Err(error) => eprintln!("aether-daemon: {error}; continuing"),
        }
        // Staging always lives under the daemon's download root, which may be
        // a different filesystem than a "save as" target. Check both.
        let staging_root = staging
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| state.download_dir.join(".aether"));
        let staging_reserved = reserved_staging(state);
        match ensure_space(&staging_root, size.saturating_add(staging_reserved)) {
            Ok(_) => {}
            Err(error) if error.is_shortage() => {
                return Err(DaemonError::Protocol(format!(
                    "{error} (staging needs {}, other downloads reserve {})",
                    human_bytes(size),
                    human_bytes(staging_reserved)
                )))
            }
            Err(error) => eprintln!("aether-daemon: {error}; continuing"),
        }
    }

    // -100..=100 keeps a single task from starving the queue forever.
    let priority = request.priority.unwrap_or(0).clamp(-100, 100);
    // aria2 `out` (and any native caller that asks) overrides the probe's
    // suggested name; the http plugin already prefers task.filename.
    let filename = request
        .settings
        .get("filename")
        .and_then(Value::as_str)
        .and_then(aether_plugin_sdk::fsutil::sanitize_filename)
        .or_else(|| probe.suggested_filename.clone());
    let mut settings = request.settings;
    let checksum = parse_checksum(&settings)?;
    // The daemon-wide ffmpeg path is a default, not an override: a task that
    // names its own binary (or none at all) keeps/loses to its own choice,
    // and the value is persisted so the record shows what will actually run.
    if let Some(ffmpeg) = state.settings.ffmpeg_path.as_deref() {
        if let Some(object) = settings.as_object_mut() {
            object
                .entry("ffmpeg_path")
                .or_insert_with(|| serde_json::Value::String(ffmpeg.to_string()));
        }
    }
    state.store.create_task(&NewTask {
        id: task_id.clone(),
        url: url.clone(),
        plugin_id: plugin_id.clone(),
        filename: filename.clone(),
        settings: settings.clone(),
        state: TaskState::Queued,
        priority,
        target_dir: stored_dir,
    })?;

    let task = TaskSpec {
        task_id: task_id.clone(),
        url,
        filename,
        settings,
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum,
        proxy: task_proxy,
    };

    let started = match try_launch(state, &plugin_id, task, staging, final_dir.clone()) {
        Ok(started) => started,
        Err(error) => {
            let message = error.to_string();
            state
                .store
                .fail_task(&task_id, "internal", &message, true)?;
            return Err(error);
        }
    };
    if let Ok(Some(task)) = state.store.task(&task_id) {
        broadcast(state, json!({"event": "task_added", "task": task}));
    }
    if !started {
        let _ = state.store.append_log(
            &task_id,
            "info",
            &format!(
                "queued: {} task(s) already running (max {})",
                state.active.len(),
                state.max_concurrent
            ),
            None,
        );
    }

    Ok(json!({
        "task_id": task_id,
        "plugin_id": plugin_id,
        "queued": !started,
        "dir": final_dir.to_string_lossy(),
        "priority": priority,
    }))
}

/// Parse a task-level SHA-256 from settings. Accepts both aria2's
/// `sha-256=<hex>` and the JSON object form `{algorithm, value}`.
fn parse_checksum(settings: &Value) -> Result<Option<Checksum>, DaemonError> {
    let Some(value) = settings.get("checksum") else {
        return Ok(None);
    };
    let (algorithm, value) = match value {
        Value::String(text) => {
            let (algorithm, value) = text
                .split_once(':')
                .or_else(|| text.split_once('='))
                .ok_or_else(|| {
                    DaemonError::Protocol("checksum must be sha-256:<hex> or sha-256=<hex>".into())
                })?;
            (algorithm.to_string(), value.to_string())
        }
        Value::Object(map) => {
            let algorithm = map
                .get("algorithm")
                .and_then(Value::as_str)
                .ok_or_else(|| DaemonError::Protocol("checksum.algorithm is required".into()))?;
            let value = map
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| DaemonError::Protocol("checksum.value is required".into()))?;
            (algorithm.to_string(), value.to_string())
        }
        _ => {
            return Err(DaemonError::Protocol(
                "checksum must be a string or object".into(),
            ))
        }
    };
    let algorithm = algorithm.trim().replace('_', "-").to_ascii_lowercase();
    if algorithm != "sha256" && algorithm != "sha-256" {
        return Err(DaemonError::Protocol(
            "only SHA-256 checksums are supported".into(),
        ));
    }
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DaemonError::Protocol(
            "SHA-256 checksum must be 64 hex characters".into(),
        ));
    }
    Ok(Some(Checksum {
        algorithm: ChecksumAlgorithm::Sha256,
        value: value.to_ascii_lowercase(),
    }))
}

/// Parse one numeric limit out of a patch. `null` is rejected: the scheduler
/// needs a concrete value, and `0` already means what the caller wants (no
/// limit, no retry).
fn parse_limit<T>(value: &Value, key: &str) -> Result<T, DaemonError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<Option<T>>(value.clone())
        .map_err(|error| DaemonError::Protocol(format!("invalid {key}: {error}")))?
        .ok_or_else(|| DaemonError::Protocol(format!("{key} cannot be cleared; send a number")))
}

/// Copy the persisted limits onto the fields the scheduler reads. `validate`
/// has run by then, and startup resolution keeps the limits present, so every
/// value here is one the engine can honour.
fn apply_limits(state: &mut State) {
    if let Some(value) = state.settings.max_concurrent {
        state.max_concurrent = value as usize;
    }
    // `0` is the stored form of "no limit".
    if let Some(value) = state.settings.max_speed_bps {
        state.max_speed_bps = (value > 0).then_some(value);
    }
    if let Some(value) = state.settings.retry_max {
        state.retry_max = value;
    }
}

/// Start a task unless the concurrency limit is reached. Returns Ok(true) when
/// the task is now running, Ok(false) when it stayed queued.
fn try_launch(
    state: &mut State,
    plugin_id: &str,
    task: TaskSpec,
    staging: PathBuf,
    final_dir: PathBuf,
) -> Result<bool, DaemonError> {
    // Outside the scheduled window nothing starts; the periodic tick pauses
    // whatever is already running.
    if !schedule_in_window(state) {
        return Ok(false);
    }
    if state.active.len() >= state.max_concurrent {
        return Ok(false);
    }

    let task_id = task.task_id.clone();
    let settings_limit = task
        .settings
        .get("max_speed_bps")
        .and_then(serde_json::Value::as_u64)
        .filter(|limit| *limit > 0);
    let budget = Budget {
        // The settings page caps the connections one task may open; the http
        // plugin clamps this to 16 when it starts the range connections.
        connections: state.settings.connections.unwrap_or(CONNECTIONS_DEFAULT),
        download_bps: settings_limit.unwrap_or_else(|| {
            // Spread the global limit evenly across the tasks that will be
            // running once this one starts.
            match state.max_speed_bps {
                Some(total) => total / (state.active.len() as u64 + 1),
                None => 0,
            }
        }),
        ..Budget::default()
    };

    state
        .manager
        .start(plugin_id, task, TaskPaths { staging, final_dir }, budget)
        .map_err(DaemonError::Host)?;
    state.store.set_state(&task_id, TaskState::Running)?;
    let _ = state.store.append_log(
        &task_id,
        "info",
        &format!("running with plugin {plugin_id}"),
        None,
    );
    state.active.insert(task_id.clone());
    state.live.insert(task_id.clone());
    state.last_progress.insert(task_id.clone(), Instant::now());
    broadcast(
        state,
        json!({"event": "task_state", "task_id": task_id, "state": "running"}),
    );
    Ok(true)
}

/// Promote the oldest queued tasks while there is capacity.
fn promote_queued(state: &mut State) {
    while state.active.len() < state.max_concurrent {
        // Highest priority first, then FIFO; the store does the ordering so
        // the daemon never re-implements it.
        let next = state.store.next_queued().ok().flatten();
        let Some(task) = next else { return };
        match start_from_record(state, &task) {
            Ok(true) => {}
            Ok(false) => {
                break;
            }
            Err(error) => {
                let _ = state
                    .store
                    .fail_task(&task.id, "internal", &error.to_string(), true);
                continue;
            }
        }
    }
}

/// Whether the schedule lets tasks run at `now_minutes` (minutes since
/// midnight). A window whose end is before its start wraps midnight. A
/// disabled, incomplete or unparseable schedule always allows running: a
/// hand-edited file must not stall the queue silently.
pub(crate) fn schedule_active(
    on: Option<bool>,
    start: Option<&str>,
    end: Option<&str>,
    now_minutes: u32,
) -> bool {
    if on != Some(true) {
        return true;
    }
    let parse = |text: Option<&str>| {
        text.map(str::trim)
            .filter(|text| !text.is_empty())
            .and_then(parse_hhmm)
    };
    let (Some(start), Some(end)) = (parse(start), parse(end)) else {
        return true;
    };
    if start <= end {
        (start..end).contains(&now_minutes)
    } else {
        now_minutes >= start || now_minutes < end
    }
}

fn schedule_in_window(state: &State) -> bool {
    use chrono::Timelike;

    let now = chrono::Local::now();
    let now_minutes = now.hour() * 60 + now.minute();
    schedule_active(
        state.settings.schedule_on,
        state.settings.schedule_start.as_deref(),
        state.settings.schedule_end.as_deref(),
        now_minutes,
    )
}

/// Enforce the scheduled window: everything pauses when the window closes
/// (and stays out of the launcher while it is closed); the tasks the
/// scheduler paused resume when it opens again. User-paused tasks are never
/// touched because only `schedule_paused` ids are resumed.
fn tick_schedule(state: &mut State) {
    if schedule_in_window(state) {
        let paused: Vec<String> = state.schedule_paused.drain().collect();
        for task_id in paused {
            resume_scheduled(state, &task_id);
        }
        return;
    }
    // Snapshot first: pausing mutates `active` and the store.
    let mut candidates = state.active.clone();
    if let Ok(tasks) = state.store.list_tasks(500) {
        candidates.extend(
            tasks
                .into_iter()
                .filter(|task| task.state == TaskState::Queued)
                .map(|task| task.id),
        );
    }
    for task_id in candidates {
        if !state.schedule_paused.insert(task_id.clone()) {
            continue;
        }
        let Ok(Some(task)) = state.store.task(&task_id) else {
            state.schedule_paused.remove(&task_id);
            continue;
        };
        match task.state {
            TaskState::Running => {
                let _ = state.manager.send(
                    &task.plugin_id,
                    &HostCommand::Pause {
                        task_id: task_id.clone(),
                    },
                );
                let _ = state.store.set_state(&task_id, TaskState::Paused);
                state.active.remove(&task_id);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "paused by schedule", None);
                broadcast(
                    state,
                    json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                );
            }
            TaskState::Queued => {
                let _ = state.store.set_state(&task_id, TaskState::Paused);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "paused by schedule", None);
                broadcast(
                    state,
                    json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                );
            }
            // Anything else raced us (finished / failed / user pause).
            _ => {
                state.schedule_paused.remove(&task_id);
            }
        }
    }
}

/// Resume one task the schedule paused: live plugin executions resume in
/// place, cold ones go through the launcher (which the open window allows).
fn resume_scheduled(state: &mut State, task_id: &str) {
    let Ok(Some(task)) = state.store.task(task_id) else {
        return;
    };
    if task.state != TaskState::Paused {
        return;
    }
    if state.live.contains(task_id) {
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::Resume {
                task_id: task_id.to_string(),
            },
        );
        let _ = state.store.set_state(task_id, TaskState::Running);
        state.active.insert(task_id.to_string());
        state
            .last_progress
            .insert(task_id.to_string(), Instant::now());
        let _ = state
            .store
            .append_log(task_id, "info", "resumed by schedule", None);
        broadcast(
            state,
            json!({"event": "task_state", "task_id": task_id, "state": "running"}),
        );
        return;
    }
    match start_from_record(state, &task) {
        Ok(true) => {
            let _ = state
                .store
                .append_log(task_id, "info", "resumed by schedule", None);
        }
        // At capacity: back to the queue, the next promotion picks it up.
        Ok(false) => {
            let _ = state.store.set_state(task_id, TaskState::Queued);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
            );
        }
        Err(error) => {
            let _ = state
                .store
                .fail_task(task_id, "internal", &error.to_string(), true);
        }
    }
}

/// Fail tasks whose plugin process died. Paused tasks are left paused: their
/// resume path restarts the plugin from the stored record.
fn recover_dead_plugins(state: &mut State) {
    let dead = state.manager.prune_dead();
    if dead.is_empty() {
        return;
    }
    for plugin_id in dead {
        let affected: Vec<aether_host::StoredTask> = state
            .live
            .iter()
            .filter_map(|task_id| state.store.task(task_id).ok().flatten())
            .filter(|task| task.plugin_id == plugin_id)
            .collect();
        let mut failed_any = false;
        for task in affected {
            state.active.remove(&task.id);
            state.live.remove(&task.id);
            state.canceling.remove(&task.id);
            state.last_progress.remove(&task.id);
            if task.state == TaskState::Paused {
                let _ = state.store.append_log(
                    &task.id,
                    "warn",
                    &format!("plugin {plugin_id} exited while paused; resume will restart it"),
                    None,
                );
                continue;
            }
            let message = format!("plugin {plugin_id} exited while the task was running");
            let _ = state.store.fail_task(&task.id, "plugin", &message, true);
            let _ = state.store.append_log(&task.id, "error", &message, None);
            broadcast(
                state,
                json!({"event": "failed", "task_id": task.id, "error": message}),
            );
            schedule_retry(state, &task.id, &message);
            failed_any = true;
        }
        if failed_any {
            promote_queued(state);
        }
    }
}

/// A plugin that is alive but never reports progress again must not pin a
/// concurrency slot forever. Five minutes is deliberately generous: the HTTP
/// client has a 30 s read timeout, so a live task normally reports far sooner.
fn check_stalled_tasks(state: &mut State) {
    const STALL: Duration = Duration::from_secs(900);
    let stalled: Vec<String> = state
        .active
        .iter()
        .filter(|task_id| {
            state
                .last_progress
                .get(*task_id)
                .is_none_or(|last| last.elapsed() > STALL)
        })
        .cloned()
        .collect();
    let mut failed_any = false;
    for task_id in stalled {
        let Ok(Some(task)) = state.store.task(&task_id) else {
            continue;
        };
        if task.state != TaskState::Running {
            continue;
        }
        // A task that already downloaded its full size may be checksumming a
        // very large file; that is not a stalled download.
        if task.total.is_some_and(|total| task.downloaded >= total) {
            continue;
        }
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::Cancel {
                task_id: task_id.clone(),
            },
        );
        state.active.remove(&task_id);
        state.live.remove(&task_id);
        state.canceling.remove(&task_id);
        state.last_progress.remove(&task_id);
        let message = format!("no progress for {} s", STALL.as_secs());
        let _ = state.store.fail_task(&task_id, "timeout", &message, true);
        let _ = state.store.append_log(&task_id, "error", &message, None);
        broadcast(
            state,
            json!({"event": "failed", "task_id": task_id, "error": message}),
        );
        schedule_retry(state, &task_id, &message);
        failed_any = true;
    }
    if failed_any {
        promote_queued(state);
    }
}

/// Schedule an automatic retry for a retryable failure, honoring the attempt
/// cap. Exhausted tasks stay failed until the user resumes them manually.
fn schedule_retry(state: &mut State, task_id: &str, message: &str) {
    if state.retry_max == 0 {
        return;
    }
    // The attempt counter lives in the database so it survives restarts and so
    // a retry cycle cannot reset itself to "first attempt" every time.
    let previous = state
        .store
        .task(task_id)
        .ok()
        .flatten()
        .map(|task| task.retry_attempts)
        .unwrap_or(0);
    let attempts = previous.saturating_add(1);
    if attempts > state.retry_max {
        state.retries.remove(task_id);
        // Keep the counter: "failed after N retries" is information worth
        // showing; `retry_at = NULL` already means "nothing scheduled".
        let _ = state.store.clear_pending_retry(task_id);
        let _ = state.store.append_log(
            task_id,
            "warn",
            &format!("auto-retry exhausted after {previous} retries: {message}"),
            None,
        );
        return;
    }
    let delay = Duration::from_secs(
        state
            .retry_base_secs
            .saturating_mul(1_u64 << (attempts - 1).min(6)),
    );
    let next = Instant::now() + delay;
    let _ = state
        .store
        .set_retry(task_id, attempts, now_ms() + delay.as_millis() as i64);
    state
        .retries
        .insert(task_id.to_string(), RetrySchedule { next });
    let _ = state.store.append_log(
        task_id,
        "info",
        &format!(
            "auto-retry {attempts}/{} in {} s",
            state.retry_max,
            delay.as_secs()
        ),
        None,
    );
    broadcast(
        state,
        json!({
            "event": "retry_scheduled",
            "task_id": task_id,
            "attempt": attempts,
            "delay_secs": delay.as_secs(),
        }),
    );
}

/// Start every retry whose backoff elapsed; retries re-enter the normal queue
/// so they compete FIFO with other work.
fn check_retries(state: &mut State) {
    if state.retries.is_empty() {
        return;
    }
    let now = Instant::now();
    let due: Vec<String> = state
        .retries
        .iter()
        .filter(|(_, schedule)| schedule.next <= now)
        .map(|(task_id, _)| task_id.clone())
        .collect();
    if due.is_empty() {
        return;
    }
    for task_id in due {
        state.retries.remove(&task_id);
        let _ = state.store.clear_pending_retry(&task_id);
        let result = state.store.set_state(&task_id, TaskState::Queued);
        if result.is_ok() {
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
            );
        }
    }
    promote_queued(state);
}

/// (Re)start a task from its stored record if capacity allows. Ok(true) when
/// the task is running afterwards, Ok(false) when it stayed queued.
fn start_from_record(state: &mut State, task: &StoredTask) -> Result<bool, DaemonError> {
    let final_dir = destination_dir(state, task.target_dir.as_deref());
    fs::create_dir_all(&final_dir)?;
    let staging = prepare_staging(state, &task.id, &final_dir)?;
    let checksum = parse_checksum(&task.settings).ok().flatten();
    let spec = TaskSpec {
        task_id: task.id.clone(),
        url: task.url.clone(),
        filename: task.filename.clone(),
        settings: task.settings.clone(),
        plugin_state: task.plugin_state.clone(),
        expected_size: task.total,
        checksum,
        proxy: None,
    };
    // PAC is resolved again for every (re)start: the script may have changed
    // between attempts. No network I/O here — promotions must not block.
    let spec = TaskSpec {
        proxy: resolve_task_proxy(state, &spec.url, false),
        ..spec
    };
    try_launch(state, &task.plugin_id, spec, staging, final_dir)
}

fn handle_event(state: &mut State, envelope: PluginEventEnvelope) {
    match envelope.event {
        PluginEvent::Progress {
            task_id,
            downloaded,
            total,
            speed_bps,
            ..
        } => {
            state.last_progress.insert(task_id.clone(), Instant::now());
            let _ = state
                .store
                .update_progress(&task_id, downloaded, total, speed_bps);
            broadcast(
                state,
                json!({
                    "event": "progress",
                    "task_id": task_id,
                    "downloaded": downloaded,
                    "total": total,
                    "speed_bps": speed_bps,
                }),
            );
        }
        PluginEvent::Metadata { task_id, metadata } => {
            if let Some(total) = metadata.size {
                let _ = state.store.update_total(&task_id, total);
            }
            broadcast(
                state,
                json!({"event": "task_metadata", "task_id": task_id, "metadata": metadata}),
            );
        }
        PluginEvent::Log {
            level,
            message,
            fields,
        } => {
            if let Some(task_id) = fields
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let fields = Value::Object(fields.into_iter().collect());
                let _ = state.store.append_log(
                    &task_id,
                    &format!("{level:?}").to_ascii_lowercase(),
                    &message,
                    Some(&fields),
                );
            } else {
                eprintln!("aether-daemon: plugin log: {message}");
            }
        }
        PluginEvent::Detail { task_id, data } => {
            state.details.insert(task_id.clone(), data.clone());
            broadcast(
                state,
                json!({"event": "task_detail", "task_id": task_id, "detail": data}),
            );
        }
        PluginEvent::Completed { task_id, files, .. } => {
            // A late completion after a watchdog failure/cancel must not
            // resurrect the task.
            if !state.active.contains(&task_id) && !state.live.contains(&task_id) {
                return;
            }
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.last_progress.remove(&task_id);
            state.details.remove(&task_id);
            // The record is authoritative: a "save as" task must finalize into
            // the directory it was created with, even after a restart.
            let record = state.store.task(&task_id).ok().flatten();
            let target_dir = record.as_ref().and_then(|task| task.target_dir.clone());
            let expected = record
                .as_ref()
                .and_then(|task| parse_checksum(&task.settings).ok().flatten());
            let final_dir = destination_dir(state, target_dir.as_deref());
            let staging_root = existing_staging_root(state, &task_id, &final_dir);
            let staging = staging_root.join("staging");
            match finalize_task_with_checksum(&staging, &final_dir, &files, expected.as_ref()) {
                Ok(moved) => {
                    let _ = fs::remove_dir_all(&staging_root);
                    cleanup_staging_parent(&staging_root);
                    let _ = state.store.complete_task(&task_id, &files);
                    let names = moved
                        .iter()
                        .filter_map(|path| path.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    let _ = state.store.append_log(
                        &task_id,
                        "info",
                        &format!("completed: {}", names.join(", ")),
                        Some(&json!({"files": names})),
                    );
                    broadcast(
                        state,
                        json!({"event": "completed", "task_id": task_id, "files": files}),
                    );
                }
                Err(error) => {
                    let _ = state
                        .store
                        .fail_task(&task_id, "finalize", &error.to_string(), false);
                    let _ = state.store.append_log(
                        &task_id,
                        "error",
                        &format!("finalize failed: {error}"),
                        None,
                    );
                    broadcast(
                        state,
                        json!({"event": "failed", "task_id": task_id, "error": error.to_string()}),
                    );
                }
            }
            promote_queued(state);
        }
        PluginEvent::Failed { task_id, error } => {
            if !state.active.contains(&task_id) && !state.live.contains(&task_id) {
                return;
            }
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.canceling.remove(&task_id);
            state.last_progress.remove(&task_id);
            state.details.remove(&task_id);
            if error.kind == aether_plugin_sdk::ErrorKind::Canceled {
                let _ = state.store.set_state(&task_id, TaskState::Canceled);
                let _ = state.store.reset_retry(&task_id);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "canceled by plugin", None);
            } else {
                let _ = state.store.fail_task(
                    &task_id,
                    &format!("{:?}", error.kind).to_ascii_lowercase(),
                    &error.message,
                    error.retryable,
                );
                let _ = state.store.append_log(
                    &task_id,
                    "error",
                    &error.message,
                    Some(&json!({
                        "kind": format!("{:?}", error.kind).to_ascii_lowercase(),
                        "retryable": error.retryable,
                    })),
                );
                schedule_retry(state, &task_id, &error.message);
            }
            broadcast(
                state,
                json!({"event": "failed", "task_id": task_id, "error": error}),
            );
            promote_queued(state);
        }
        _ => {}
    }
}

fn recover_tasks(state: &mut State) {
    let tasks = match state.store.recoverable_tasks() {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!("aether-daemon: cannot load recoverable tasks: {error}");
            return;
        }
    };
    for task in tasks {
        // Interrupted running/queued tasks restart while capacity lasts and
        // the rest are demoted to queued. Paused tasks stay paused until the
        // user resumes them explicitly.
        if !matches!(task.state, TaskState::Running | TaskState::Queued) {
            continue;
        }
        match start_from_record(state, &task) {
            Ok(true) => {}
            Ok(false) => {
                let _ = state.store.set_state(&task.id, TaskState::Queued);
            }
            Err(error) => eprintln!("aether-daemon: cannot resume task {}: {error}", task.id),
        }
    }
}

/// The `Init::proxy` for plugin processes: PAC hands plugins the environment
/// default, because the real proxy is resolved per task.
fn init_proxy(proxy: Option<&ProxySetting>) -> Option<ProxyConfig> {
    proxy.and_then(ProxySetting::init_config)
}

/// Network settings ride into plugin processes at spawn, so after a change
/// every idle plugin process is stopped; its next use respawns it with the
/// new configuration. Plugins with running or paused work keep their process
/// (and therefore their old settings) until that work finishes.
/// Stop idle plugins that declare `external-process`; used when the policy
/// flips to "blocked". Busy plugins keep running until their task ends.
fn stop_idle_external_plugins(state: &mut State) {
    let busy: HashSet<String> = state
        .live
        .iter()
        .filter_map(|task_id| {
            state
                .store
                .task(task_id)
                .ok()
                .flatten()
                .map(|task| task.plugin_id)
        })
        .collect();
    let ids = state
        .manager
        .registry()
        .iter()
        .filter(|entry| {
            entry
                .manifest
                .capabilities
                .contains(&aether_plugin_sdk::Capability::ExternalProcess)
        })
        .map(|entry| entry.manifest.id.clone())
        .filter(|id| !busy.contains(id))
        .collect::<Vec<_>>();
    for id in ids {
        state.manager.stop(&id);
    }
}

/// Start the aria2 compatibility service when an enabled `kind: "service"`
/// plugin declares the `aria2` protocol. Failure is logged, not fatal:
/// downloads keep working and HTTP requests answer 503.
fn restart_idle_plugins(state: &mut State) {
    let busy: HashSet<String> = state
        .live
        .iter()
        .filter_map(|task_id| {
            state
                .store
                .task(task_id)
                .ok()
                .flatten()
                .map(|task| task.plugin_id)
        })
        .collect();
    for plugin_id in state
        .manager
        .registry()
        .iter()
        .map(|entry| entry.manifest.id.clone())
        .collect::<Vec<_>>()
    {
        if !busy.contains(&plugin_id) {
            state.manager.stop(&plugin_id);
        }
    }
}

/// Resolve the proxy for one task. Errors are logged and swallowed: a broken
/// PAC (or an unreadable desktop setting) must not stop downloads, it just
/// falls back to the process default.
fn resolve_task_proxy(state: &mut State, url: &str, allow_fetch: bool) -> Option<ProxyConfig> {
    if state.pac.is_some() {
        let pac = state.pac.as_mut()?;
        return match pac.resolve(url, allow_fetch) {
            Ok(proxy) => Some(proxy),
            Err(error) => {
                eprintln!("aether-daemon: pac for {url}: {error}");
                None
            }
        };
    }
    // `system` (and the default, unset) means "follow the desktop"; explicit
    // custom/direct modes are handled by the plugins themselves.
    match state.settings.proxy.as_ref() {
        None | Some(ProxySetting::System) => {}
        Some(_) => return None,
    }
    match state.system_proxy.lookup().cloned() {
        Some(SystemProxy::Manual(config)) => Some(config),
        Some(SystemProxy::Pac(pac_url)) => resolve_system_pac(state, &pac_url, url, allow_fetch),
        None => None,
    }
}

/// A desktop-configured PAC URL (GNOME/KDE automatic mode) is evaluated like an
/// explicit PAC setting, but with its own cache slot.
fn resolve_system_pac(
    state: &mut State,
    pac_url: &str,
    url: &str,
    allow_fetch: bool,
) -> Option<ProxyConfig> {
    let needs_resolver = state
        .system_pac
        .as_ref()
        .map(|(current, _)| current != pac_url)
        .unwrap_or(true);
    if needs_resolver {
        state.system_pac = Some((
            pac_url.to_string(),
            PacResolver::new(Some(pac_url.to_string()), None),
        ));
    }
    let (_, resolver) = state.system_pac.as_mut()?;
    match resolver.resolve(url, allow_fetch) {
        Ok(proxy) => Some(proxy),
        Err(error) => {
            eprintln!("aether-daemon: system pac {pac_url} for {url}: {error}");
            None
        }
    }
}

/// Whether two directories live on the same filesystem. Unknown metadata
/// keeps the centralized staging root (the safe default).
fn same_filesystem(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev(),
            _ => true,
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => {
                match (left.volume_serial_number(), right.volume_serial_number()) {
                    (Some(left), Some(right)) => left == right,
                    // Unknown volume metadata keeps the centralized staging
                    // root: copying at finalize is safer than writing into a
                    // directory whose mount we could not identify.
                    _ => true,
                }
            }
            _ => true,
        }
    }
}

/// Centralized staging lives under the download root; a target on another
/// filesystem gets its own hidden staging directory so finalization is a
/// rename instead of a cross-device copy.
fn task_staging_root(state: &State, task_id: &str, final_dir: &Path) -> PathBuf {
    if same_filesystem(&state.download_dir, final_dir) {
        state.download_dir.join(".aether").join(task_id)
    } else {
        final_dir.join(".aether-staging").join(task_id)
    }
}

/// Drop staging directories left behind by a crash for tasks that are no
/// longer runnable. The DB is the source of truth.
fn cleanup_stale_staging(state: &State) {
    let Ok(entries) = fs::read_dir(&state.data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("task-") {
            continue;
        }
        let keep = state.store.task(name).ok().flatten().is_some_and(|task| {
            matches!(
                task.state,
                TaskState::Running | TaskState::Queued | TaskState::Paused
            )
        });
        if !keep {
            let _ = fs::remove_dir_all(&path);
        }
    }
    if let Ok(tasks) = state.store.list_tasks_with_files(10_000, false) {
        for task in tasks {
            if task.state != TaskState::Completed {
                continue;
            }
            let Some(target) = task.target_dir.as_deref() else {
                continue;
            };
            let root = Path::new(target).join(".aether-staging").join(&task.id);
            if root.is_dir() {
                let _ = fs::remove_dir_all(&root);
                cleanup_staging_parent(&root);
            }
        }
    }
}

/// Remove the cross-device `.aether-staging` parent when the last task under
/// it is gone. The centralized `.aether` parent is shared with the DB/settings
/// and is intentionally left alone.
fn cleanup_staging_parent(root: &Path) {
    let Some(parent) = root.parent() else {
        return;
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some(".aether-staging") {
        let _ = fs::remove_dir(parent);
    }
}

/// Prefer an existing staging directory over the computed one: a mount can
/// change between runs, and a partial download must not be abandoned.
fn existing_staging_root(state: &State, task_id: &str, final_dir: &Path) -> PathBuf {
    let centralized = state.download_dir.join(".aether").join(task_id);
    let target = final_dir.join(".aether-staging").join(task_id);
    if centralized.join("staging").is_dir() {
        centralized
    } else if target.join("staging").is_dir() {
        target
    } else {
        task_staging_root(state, task_id, final_dir)
    }
}

/// Prepare the staging directory, falling back to the centralized root when
/// volume detection was wrong or the target mount refuses the hidden folder.
fn prepare_staging(state: &State, task_id: &str, final_dir: &Path) -> std::io::Result<PathBuf> {
    let preferred = existing_staging_root(state, task_id, final_dir).join("staging");
    let fallback = state
        .download_dir
        .join(".aether")
        .join(task_id)
        .join("staging");
    create_dir_with_fallback(&preferred, &fallback)
}

/// Create `preferred`, falling back to `fallback` when the target mount is
/// read-only, full enough to refuse a directory, or not a directory at all.
fn create_dir_with_fallback(preferred: &Path, fallback: &Path) -> std::io::Result<PathBuf> {
    match fs::create_dir_all(preferred) {
        Ok(()) => Ok(preferred.to_path_buf()),
        Err(error) => {
            eprintln!(
                "aether-daemon: cannot create staging under {}: {error}; using the download root",
                preferred.display()
            );
            fs::create_dir_all(fallback)?;
            Ok(fallback.to_path_buf())
        }
    }
}

/// Bytes still to be written into `dir` by running plus queued/paused tasks.
/// The queue is included because a burst of adds must not each pass the check
/// against the same free space.
fn reserved_in(state: &State, dir: &Path) -> u64 {
    let mut reserved: u64 = state
        .active
        .iter()
        .filter_map(|task_id| state.store.task(task_id).ok().flatten())
        .filter(|task| destination_dir(state, task.target_dir.as_deref()) == dir)
        .map(|task| task.total.unwrap_or(0).saturating_sub(task.downloaded))
        .sum();
    if let Ok(tasks) = state.store.list_tasks_with_files(10_000, false) {
        reserved = reserved.saturating_add(
            tasks
                .into_iter()
                .filter(|task| {
                    matches!(task.state, TaskState::Queued | TaskState::Paused)
                        && destination_dir(state, task.target_dir.as_deref()) == dir
                })
                .map(|task| task.total.unwrap_or(0))
                .sum::<u64>(),
        );
    }
    reserved
}

/// Bytes that will eventually pass through the daemon's staging root.
fn reserved_staging(state: &State) -> u64 {
    let mut reserved: u64 = state
        .active
        .iter()
        .filter_map(|task_id| state.store.task(task_id).ok().flatten())
        .map(|task| task.total.unwrap_or(0).saturating_sub(task.downloaded))
        .sum();
    if let Ok(tasks) = state.store.list_tasks_with_files(10_000, false) {
        reserved = reserved.saturating_add(
            tasks
                .into_iter()
                .filter(|task| matches!(task.state, TaskState::Queued | TaskState::Paused))
                .map(|task| task.total.unwrap_or(0))
                .sum::<u64>(),
        );
    }
    reserved
}

/// Where a task's files land: the recorded directory or the daemon default.
fn destination_dir(state: &State, target_dir: Option<&str>) -> PathBuf {
    target_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| state.download_dir.clone())
}

/// Resolve the `dir` field of `add`. Absolute paths are honored (same-user
/// daemon: a client may legitimately "save as" elsewhere); relative paths stay
/// inside the download root and may not climb out of it.
fn resolve_target_dir(state: &State, requested: Option<&str>) -> Result<PathBuf, DaemonError> {
    let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(state.download_dir.clone());
    };
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(DaemonError::Protocol(format!(
            "invalid destination {raw:?}: relative paths may not contain '..'"
        )));
    }
    Ok(state.download_dir.join(path))
}

/// Reload automatic retries that were pending when the daemon last stopped.
/// Due entries go straight back to the queue; future ones keep their remaining
/// delay so a restart does not restart the backoff from scratch.
fn restore_retries(state: &mut State) {
    let pending = match state.store.pending_retries() {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("aether-daemon: cannot load pending retries: {error}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    let now = now_ms();
    for (task_id, attempts, at) in pending {
        let remaining = at.saturating_sub(now);
        if remaining <= 0 {
            let _ = state.store.clear_pending_retry(&task_id);
            if state.store.set_state(&task_id, TaskState::Queued).is_ok() {
                let _ = state.store.append_log(
                    &task_id,
                    "info",
                    "auto-retry resumed after restart",
                    None,
                );
            }
        } else {
            state.retries.insert(
                task_id.clone(),
                RetrySchedule {
                    next: Instant::now() + Duration::from_millis(remaining as u64),
                },
            );
            let _ = state.store.append_log(
                &task_id,
                "info",
                &format!(
                    "auto-retry {attempts}/{} resumes in {} s after restart",
                    state.retry_max,
                    remaining / 1000
                ),
                None,
            );
        }
    }
    promote_queued(state);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn task_id(request: &Request) -> Result<String, DaemonError> {
    request
        .task_id
        .clone()
        .ok_or_else(|| DaemonError::Protocol("task_id is required".into()))
}

#[cfg(test)]
mod tests {
    use super::{parse_checksum, schedule_active};
    use serde_json::json;
    use std::fs;

    #[test]
    fn same_filesystem_is_reflexive() {
        let dir = std::env::temp_dir();
        assert!(super::same_filesystem(&dir, &dir));
    }

    #[test]
    fn same_filesystem_missing_paths_keep_centralized_staging() {
        let dir = std::env::temp_dir();
        let missing = dir.join("aether-missing-volume-target");
        assert!(
            super::same_filesystem(&dir, &missing),
            "unknown metadata must keep the centralized staging root"
        );
    }

    #[test]
    fn staging_falls_back_when_the_preferred_path_is_blocked() {
        let root = std::env::temp_dir().join(format!(
            "aether-staging-fallback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let blocker = root.join("not-a-directory");
        fs::write(&blocker, b"x").unwrap();
        let preferred = blocker.join("staging");
        let fallback = root.join("fallback").join("staging");
        let chosen = super::create_dir_with_fallback(&preferred, &fallback).unwrap();
        assert_eq!(chosen, fallback);
        assert!(fallback.is_dir());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_task_checksums() {
        let hex = "a".repeat(64);
        let checksum = parse_checksum(&json!({ "checksum": format!("sha-256={hex}") }))
            .unwrap()
            .unwrap();
        assert_eq!(checksum.value, hex);

        let object = json!({ "checksum": { "algorithm": "SHA256", "value": hex } });
        assert!(parse_checksum(&object).unwrap().is_some());
        assert!(parse_checksum(&json!({ "checksum": "md5=abc" })).is_err());
        assert!(parse_checksum(&json!({ "checksum": "sha256=short" })).is_err());
        assert!(parse_checksum(&json!({})).unwrap().is_none());
    }

    #[test]
    fn schedule_window_decides_by_clock_time() {
        let window = (Some(true), Some("01:00"), Some("07:00"));

        // Inside the window.
        assert!(schedule_active(window.0, window.1, window.2, 60));
        assert!(schedule_active(window.0, window.1, window.2, 6 * 60 + 59));
        // Outside the window.
        assert!(!schedule_active(window.0, window.1, window.2, 0));
        assert!(!schedule_active(window.0, window.1, window.2, 7 * 60));
        assert!(!schedule_active(window.0, window.1, window.2, 12 * 60));

        // Overnight windows (end < start) wrap midnight.
        assert!(schedule_active(
            Some(true),
            Some("22:00"),
            Some("07:00"),
            23 * 60
        ));
        assert!(schedule_active(
            Some(true),
            Some("22:00"),
            Some("07:00"),
            2 * 60
        ));
        assert!(!schedule_active(
            Some(true),
            Some("22:00"),
            Some("07:00"),
            12 * 60
        ));

        // Disabled or incomplete schedules never stall the queue.
        assert!(schedule_active(None, Some("01:00"), Some("07:00"), 12 * 60));
        assert!(schedule_active(
            Some(false),
            Some("01:00"),
            Some("07:00"),
            12 * 60
        ));
        assert!(schedule_active(Some(true), Some("01:00"), None, 12 * 60));
        assert!(schedule_active(
            Some(true),
            Some("bad"),
            Some("07:00"),
            12 * 60
        ));
    }
}
