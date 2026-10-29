//! Daemon bridge for the Slint UI.
//!
//! The window never touches SQLite, plugins or worker threads: a background
//! thread keeps a long-lived subscription to `aether-daemon`, mirrors the task
//! list into [`crate::model::Task`] values, and accepts commands over a
//! channel. The UI polls a cheap snapshot at its own pace.

use std::collections::VecDeque;

use aether_daemon::{PluginInfo, Stats};
use serde_json::Value;

use crate::model::Task;

/// Split a pasted download field into individual URLs: whitespace
/// separates them (newline-separated clipboard content included), so one
/// dialog submit can queue a batch or several magnets at once. Returns
/// tokens in order; empty input yields nothing (the dialog already guards
/// an empty field).
pub fn split_urls(input: &str) -> Vec<&str> {
    input.split_whitespace().collect()
}

/// Immutable view of the daemon state, cloned per UI refresh.
#[derive(Default)]
pub struct Snapshot {
    pub tasks: Vec<Task>,
    /// Aggregate download speed samples (one per second).
    pub history: VecDeque<f64>,
    /// False while the daemon is unreachable (the UI can show a hint).
    pub connected: bool,
    /// Default download root, used as the new-download dialog's path.
    pub download_dir: String,
    /// Dashboard counters, refreshed about once a second.
    pub stats: Option<Stats>,
    /// Installed plugins, sorted by id (stable order doubles as row ids).
    pub plugins: Vec<PluginInfo>,
    /// Current daemon settings (`get_settings`), `None` while disconnected.
    pub settings: Option<Value>,
}

pub use imp::Backend;

/// A task that reached a terminal state, with everything the UI needs to
/// notify or open it.
#[derive(Debug, Clone)]
pub struct Finished {
    pub done: bool,
    pub name: String,
    /// Download directory (present for completions).
    pub dir: Option<String>,
    /// File name inside `dir` (present for completions).
    pub file: Option<String>,
    pub error: Option<String>,
}

mod imp {
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::process::{Command as Process, Stdio};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use aether_daemon::client::Client;
    use aether_daemon::client::Subscription;
    use aether_daemon::{DaemonError, PluginInfo, Stats, StoredTask, TaskState};
    use serde_json::{json, Value};

    use super::{Finished, Snapshot};
    use crate::model::{Status, Task, CHUNK_COUNT, HISTORY_LEN};

    enum Command {
        Add {
            url: String,
            dir: String,
            speed_limit_kb: f32,
        },
        Action {
            id: i32,
            action: i32,
        },
        Proxy {
            mode: String,
            url: String,
        },
        /// One `set_settings` patch, e.g. `{"dns": "223.5.5.5"}`.
        Setting {
            patch: Value,
        },
        Plugin {
            row: i32,
            enabled: bool,
        },
        /// Install a plugin package directory (daemon verifies its manifest
        /// signature when a release key is configured).
        InstallPlugin {
            path: String,
        },
        /// Re-scan the plugins directory without restarting the daemon.
        RescanPlugins,
        /// Check the update manifest and install every outdated plugin.
        PluginUpdateAll,
    }

    struct Shared {
        snapshot: Snapshot,
        notice: Option<String>,
        completed: Vec<Finished>,
    }

    pub struct Backend {
        commands: Sender<Command>,
        shared: Arc<Mutex<Shared>>,
        started: Instant,
    }

    impl Backend {
        pub fn spawn() -> Self {
            let (commands, receiver) = mpsc::channel();
            let shared = Arc::new(Mutex::new(Shared {
                snapshot: Snapshot::default(),
                notice: None,
                completed: Vec::new(),
            }));
            let worker_shared = shared.clone();
            thread::spawn(move || worker(receiver, worker_shared));
            Self {
                commands,
                shared,
                started: Instant::now(),
            }
        }

        pub fn snapshot(&self) -> Snapshot {
            let guard = self.shared.lock().unwrap();
            Snapshot {
                tasks: guard.snapshot.tasks.clone(),
                history: guard.snapshot.history.clone(),
                connected: guard.snapshot.connected,
                download_dir: guard.snapshot.download_dir.clone(),
                stats: guard.snapshot.stats.clone(),
                plugins: guard.snapshot.plugins.clone(),
                settings: guard.snapshot.settings.clone(),
            }
        }

        /// Slow sine used by the “live” indicator, independent of the daemon.
        pub fn drift(&self) -> f32 {
            (self.started.elapsed().as_secs_f32() * 0.22).sin()
        }

        pub fn apply_action(&self, id: i32, action: i32) {
            let _ = self.commands.send(Command::Action { id, action });
        }

        pub fn add_task(&self, url: &str, path: &str, speed_limit_kb: f32) {
            let _ = self.commands.send(Command::Add {
                url: url.to_string(),
                dir: path.to_string(),
                speed_limit_kb,
            });
        }

        pub fn set_proxy(&self, mode: &str, url: &str) {
            let _ = self.commands.send(Command::Proxy {
                mode: mode.to_string(),
                url: url.to_string(),
            });
        }

        /// Send one `set_settings` patch (key → value pairs) to the daemon.
        pub fn set_setting(&self, patch: Value) {
            let _ = self.commands.send(Command::Setting { patch });
        }

        /// Enable/disable the plugin displayed at row `row` (index into the
        /// snapshot's plugin list).
        pub fn set_plugin_enabled(&self, row: i32, enabled: bool) {
            let _ = self.commands.send(Command::Plugin { row, enabled });
        }

        /// Install a plugin package (directory containing plugin.json).
        /// Success and failure both surface through `take_notice`.
        pub fn install_plugin(&self, path: &str) {
            let _ = self.commands.send(Command::InstallPlugin {
                path: path.to_string(),
            });
        }

        /// Ask the daemon to re-scan the plugins directory (count in the
        /// resulting notice).
        pub fn rescan_plugins(&self) {
            let _ = self.commands.send(Command::RescanPlugins);
        }

        /// Check the update manifest and install every outdated plugin;
        /// the summary arrives through `take_notice`.
        pub fn plugin_update_all(&self) {
            let _ = self.commands.send(Command::PluginUpdateAll);
        }

        /// Pop the most recent error surfaced by the daemon.
        pub fn take_notice(&self) -> Option<String> {
            self.shared.lock().unwrap().notice.take()
        }

        /// Drain finished tasks (completed or failed) since the last call;
        /// the UI uses them for the after-done action and notifications.
        pub fn take_finished(&self, out: &mut Vec<Finished>) {
            let mut guard = self.shared.lock().unwrap();
            out.append(&mut guard.completed);
        }

        /// Wall-clock epoch millis, used for the records' relative times.
        pub fn now_ms(&self) -> i64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0)
        }
    }

    #[derive(Default)]
    struct Worker {
        client: Option<Client>,
        subscription: Option<Subscription>,
        /// Success feedback from the last command, drained into `Shared`
        /// by the worker loop (errors travel through the `handle` result).
        notice: Option<String>,
        /// Daemon records keyed by ASCII id.
        records: HashMap<String, StoredTask>,
        /// Display order (newest first).
        order: Vec<String>,
        ids: HashMap<String, i32>,
        reverse: HashMap<i32, String>,
        next_id: i32,
        history: HashMap<String, VecDeque<f64>>,
        global: VecDeque<f64>,
        download_dir: String,
        sampled: Option<Instant>,
        refreshed: Option<Instant>,
        needs_refresh: bool,
        offline_notice: bool,
        stats: Option<Stats>,
        stats_sampled: Option<Instant>,
        plugins: Vec<PluginInfo>,
        /// Daemon settings, from `get_settings` and `settings_changed` events.
        settings: Option<Value>,
        /// Finished tasks (completed or failed), drained by the UI to run the
        /// "after done" action and the desktop notification.
        completed: VecDeque<Finished>,
    }

    fn worker(receiver: Receiver<Command>, shared: Arc<Mutex<Shared>>) {
        let socket = aether_daemon::client::default_socket();
        let mut worker = Worker {
            next_id: 1,
            ..Worker::default()
        };

        loop {
            if worker.client.is_none() {
                match worker.ensure_client(&socket) {
                    Ok(()) => worker.offline_notice = false,
                    Err(error) => {
                        // One notice per outage, not one per retry.
                        if !worker.offline_notice {
                            worker.offline_notice = true;
                            start_daemon();
                            let mut guard = shared.lock().unwrap();
                            guard.snapshot.connected = false;
                            guard.notice = Some(crate::i18n::tr(
                                format!("后台服务未就绪：{error}。已尝试自动启动 aether-daemon。"),
                                format!("Daemon not ready: {error}. Tried starting aether-daemon automatically."),
                            ));
                        }
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }

            if let Ok(command) = receiver.recv_timeout(Duration::from_millis(50)) {
                match worker.handle(command) {
                    Ok(()) => {
                        if let Some(notice) = worker.notice.take() {
                            shared.lock().unwrap().notice = Some(notice);
                        }
                    }
                    Err(error) => {
                        // Connection died mid-command: drop it and reconnect.
                        worker.disconnect();
                        let mut guard = shared.lock().unwrap();
                        guard.notice = Some(error.to_string());
                    }
                }
            }
            worker.pump_events();
            worker.sample();
            worker.maybe_refresh();
            worker.maybe_stats();
            worker.publish(&shared);
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Best effort: start the daemon the same way a packaged install would.
    fn start_daemon() {
        if std::env::var_os("AETHER_NO_AUTOSTART").is_some() {
            return;
        }
        let download_dir = default_download_dir();
        let daemon_name = format!("aether-daemon{}", std::env::consts::EXE_SUFFIX);
        let mut candidates = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join(&daemon_name));
            }
        }
        candidates.push(PathBuf::from(&daemon_name));
        for candidate in candidates {
            let spawned = Process::new(&candidate)
                .arg("--download-dir")
                .arg(&download_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if spawned.is_ok() {
                return;
            }
        }
    }

    /// `XDG_DOWNLOAD_DIR` from `user-dirs.dirs`, else `~/Downloads`. On
    /// Windows: the user profile's `Downloads`.
    fn default_download_dir() -> PathBuf {
        #[cfg(windows)]
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join("Downloads");
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("user-dirs.dirs");
            if let Ok(text) = std::fs::read_to_string(config) {
                for line in text.lines() {
                    let Some(value) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else {
                        continue;
                    };
                    let value = value.trim().trim_matches('"');
                    let value = value.replace("$HOME", &home.to_string_lossy());
                    if !value.is_empty() {
                        return PathBuf::from(value);
                    }
                }
            }
            return home.join("Downloads");
        }
        PathBuf::from("downloads")
    }

    impl Worker {
        fn disconnect(&mut self) {
            self.client = None;
            self.subscription = None;
            self.records.clear();
            self.order.clear();
            self.ids.clear();
            self.reverse.clear();
            self.stats = None;
            self.plugins.clear();
            self.settings = None;
            self.needs_refresh = true;
        }

        fn ensure_client(&mut self, socket: &Path) -> Result<(), DaemonError> {
            // Reconnected lazily after a failure.
            let _ = socket;
            if self.client.is_some() && self.subscription.is_some() {
                return Ok(());
            }
            let mut client = Client::connect(&aether_daemon::client::default_socket())?;
            client.ping()?;
            let subscription =
                Client::connect(&aether_daemon::client::default_socket())?.subscribe()?;
            self.client = Some(client);
            self.subscription = Some(subscription);
            self.refresh_all()?;
            // Seed the settings mirror so the page shows real daemon state.
            if let Some(client) = self.client.as_mut() {
                let value = client.call("get_settings", None, None, None, None, json!({}))?;
                self.settings = Some(value["settings"].clone());
            }
            Ok(())
        }

        fn handle(&mut self, command: Command) -> Result<(), DaemonError> {
            let socket = aether_daemon::client::default_socket();
            self.ensure_client(&socket)?;
            let Some(client) = self.client.as_mut() else {
                return Ok(());
            };
            match command {
                Command::Add {
                    url,
                    dir,
                    speed_limit_kb,
                } => {
                    let dir = dir.trim();
                    let dir = (!dir.is_empty()).then_some(dir);
                    let settings = if speed_limit_kb > 0.0 {
                        json!({"max_speed_bps": (speed_limit_kb as u64) * 1024})
                    } else {
                        json!({})
                    };
                    client.call_with_priority(
                        "add",
                        Some(&url),
                        None,
                        dir,
                        None,
                        None,
                        settings,
                    )?;
                    self.needs_refresh = true;
                }
                Command::Action { id, action } => {
                    let Some(daemon_id) = self.reverse.get(&id).cloned() else {
                        return Ok(());
                    };
                    let state = self
                        .records
                        .get(&daemon_id)
                        .map(|record| record.state)
                        .unwrap_or(TaskState::Queued);
                    match action {
                        // 0 = pause / resume toggle.
                        0 => {
                            if matches!(state, TaskState::Running | TaskState::Queued) {
                                client.call(
                                    "pause",
                                    None,
                                    None,
                                    None,
                                    Some(&daemon_id),
                                    json!({}),
                                )?;
                            } else if !matches!(state, TaskState::Completed) {
                                client.call(
                                    "resume",
                                    None,
                                    None,
                                    None,
                                    Some(&daemon_id),
                                    json!({}),
                                )?;
                            }
                        }
                        // 1 = retry a failed task.
                        1 => {
                            client.call("resume", None, None, None, Some(&daemon_id), json!({}))?;
                        }
                        // 2 = remove: cancel first, terminal tasks can be deleted.
                        2 => {
                            if !matches!(state, TaskState::Completed | TaskState::Canceled) {
                                let _ = client.call(
                                    "cancel",
                                    None,
                                    None,
                                    None,
                                    Some(&daemon_id),
                                    json!({}),
                                );
                            }
                            client.call("delete", None, None, None, Some(&daemon_id), json!({}))?;
                            self.forget(&daemon_id);
                        }
                        // 3 = reveal in the file manager: handled by the UI.
                        _ => {}
                    }
                    self.needs_refresh = true;
                }
                Command::Proxy { mode, url } => {
                    let proxy = match mode.as_str() {
                        "直连" | "none" | "direct" => json!({"mode": "none"}),
                        "手动" | "custom" => {
                            if url.trim().is_empty() {
                                return Ok(());
                            }
                            json!({"mode": "custom", "url": url.trim()})
                        }
                        _ => json!({"mode": "system"}),
                    };
                    client.call(
                        "set_settings",
                        None,
                        None,
                        None,
                        None,
                        json!({"proxy": proxy}),
                    )?;
                }
                Command::Setting { patch } => {
                    client.call("set_settings", None, None, None, None, patch)?;
                }
                Command::Plugin { row, enabled } => {
                    let Some(plugin) = self.plugins.get(row as usize) else {
                        return Ok(());
                    };
                    let plugin_id = plugin.id.clone();
                    client.set_plugin_enabled(&plugin_id, enabled)?;
                    self.needs_refresh = true;
                }
                Command::InstallPlugin { path } => {
                    let value = client.install_plugin(&path)?;
                    let id = value["plugin_id"].as_str().unwrap_or("plugin").to_string();
                    let version = value["version"].as_str().unwrap_or_default().to_string();
                    let replaced = value["replaced"].as_bool().unwrap_or(false);
                    self.needs_refresh = true;
                    self.notice = Some(if replaced {
                        crate::i18n::tr(
                            format!("已更新插件 {id} {version}"),
                            format!("Updated plugin {id} {version}"),
                        )
                    } else {
                        crate::i18n::tr(
                            format!("已安装插件 {id} {version}"),
                            format!("Installed plugin {id} {version}"),
                        )
                    });
                }
                Command::RescanPlugins => {
                    let value = client.rescan_plugins()?;
                    let total = value["plugins"].as_u64().unwrap_or(0);
                    self.needs_refresh = true;
                    self.notice = Some(crate::i18n::tr(
                        format!("已重新扫描插件目录：{total} 个插件"),
                        format!("Re-scanned the plugin directory: {total} plugins"),
                    ));
                }
                Command::PluginUpdateAll => {
                    let value = client.plugin_update_all()?;
                    let updated = value["updated"].as_array().cloned().unwrap_or_default();
                    let failed = value["failed"].as_array().cloned().unwrap_or_default();
                    self.needs_refresh = true;
                    self.notice = Some(if updated.is_empty() && failed.is_empty() {
                        crate::i18n::tr("插件已是最新", "Plugins are up to date")
                    } else {
                        let names = updated
                            .iter()
                            .filter_map(|item| item.as_str())
                            .collect::<Vec<_>>()
                            .join("、");
                        let failures = if failed.is_empty() {
                            String::new()
                        } else {
                            crate::i18n::tr(
                                format!("；{} 个失败", failed.len()),
                                format!("; {} failed", failed.len()),
                            )
                        };
                        crate::i18n::tr(
                            format!("已更新 {} 个插件：{names}{failures}", updated.len()),
                            format!("Updated {} plugins: {names}{failures}", updated.len()),
                        )
                    });
                }
            }
            Ok(())
        }

        fn refresh_all(&mut self) -> Result<(), DaemonError> {
            let Some(client) = self.client.as_mut() else {
                return Ok(());
            };
            let value = client.call("list", None, None, None, None, json!({}))?;
            self.download_dir = value["download_dir"].as_str().unwrap_or("").to_string();
            let tasks: Vec<StoredTask> =
                serde_json::from_value(value["tasks"].clone()).unwrap_or_default();
            let mut records = HashMap::new();
            let mut order = Vec::new();
            for task in tasks {
                order.push(task.id.clone());
                records.insert(task.id.clone(), task);
            }
            self.records = records;
            self.order = order;
            // Drop ids of tasks that disappeared.
            let known = self.order.clone();
            for (daemon_id, id) in self.ids.clone() {
                if !known.contains(&daemon_id) {
                    self.ids.remove(&daemon_id);
                    self.reverse.remove(&id);
                }
            }
            let plugins = client.plugins()?;
            self.plugins = plugins;
            self.needs_refresh = false;
            self.refreshed = Some(Instant::now());
            self.sample_due();
            Ok(())
        }

        /// Dashboard counters, refreshed about once a second (cheap SQL + one
        /// filesystem stat).
        fn maybe_stats(&mut self) {
            let due = self
                .stats_sampled
                .map(|at| at.elapsed() >= Duration::from_secs(1))
                .unwrap_or(true);
            if !due {
                return;
            }
            let result = self.client.as_mut().map(Client::stats);
            match result {
                Some(Ok(stats)) => {
                    self.stats = Some(stats);
                    self.stats_sampled = Some(Instant::now());
                }
                Some(Err(_)) => self.disconnect(),
                None => {}
            }
        }

        fn maybe_refresh(&mut self) {
            let due = self
                .refreshed
                .map(|at| at.elapsed() >= Duration::from_secs(5))
                .unwrap_or(true);
            if (self.needs_refresh || due) && self.refresh_all().is_err() {
                self.disconnect();
            }
        }

        fn pump_events(&mut self) {
            let mut events = Vec::new();
            if let Some(subscription) = self.subscription.as_ref() {
                while let Ok(event) = subscription.recv_timeout(Duration::from_millis(0)) {
                    events.push(event);
                }
            }
            for event in events {
                self.apply_event(event);
            }
        }

        fn apply_event(&mut self, event: Value) {
            let Some(kind) = event["event"].as_str() else {
                return;
            };
            // Settings events carry no task id; keep the mirror current.
            if kind == "settings_changed" {
                self.settings = Some(event["settings"].clone());
                return;
            }
            let Some(task_id) = event["task_id"].as_str().map(ToOwned::to_owned) else {
                return;
            };
            match kind {
                "task_added" => {
                    if let Ok(task) = serde_json::from_value::<StoredTask>(event["task"].clone()) {
                        if !self.records.contains_key(&task_id) {
                            self.order.insert(0, task_id.clone());
                        }
                        self.records.insert(task_id, task);
                    }
                }
                "task_state" => {
                    if let Some(state) = event["state"].as_str() {
                        if let Some(record) = self.records.get_mut(&task_id) {
                            if let Ok(state) = TaskState::parse(state) {
                                record.state = state;
                                if state != TaskState::Running {
                                    record.speed_bps = 0;
                                }
                            }
                        }
                    }
                }
                "progress" => {
                    if let Some(record) = self.records.get_mut(&task_id) {
                        if let Some(downloaded) = event["downloaded"].as_u64() {
                            record.downloaded = downloaded;
                        }
                        if let Some(total) = event["total"].as_u64() {
                            record.total = Some(total);
                        }
                        if let Some(speed) = event["speed_bps"].as_u64() {
                            record.speed_bps = speed;
                        }
                    }
                }
                "completed" => {
                    if let Some(record) = self.records.get_mut(&task_id) {
                        record.state = TaskState::Completed;
                        record.speed_bps = 0;
                        record.downloaded = record.total.unwrap_or(record.downloaded);
                        let dir = record.target_dir.clone().or_else(|| {
                            (!self.download_dir.is_empty()).then(|| self.download_dir.clone())
                        });
                        let file = record
                            .filename
                            .clone()
                            .or_else(|| Some(name_from_url(&record.url)));
                        let name = file.clone().unwrap_or_else(|| name_from_url(&record.url));
                        if self.completed.len() < 32 {
                            self.completed.push_back(Finished {
                                done: true,
                                name,
                                dir,
                                file,
                                error: None,
                            });
                        }
                    }
                    self.needs_refresh = true;
                }
                "failed" => {
                    if let Some(record) = self.records.get_mut(&task_id) {
                        let canceled = event["error"]["kind"]
                            .as_str()
                            .is_some_and(|kind| kind.eq_ignore_ascii_case("canceled"));
                        record.state = if canceled {
                            TaskState::Canceled
                        } else {
                            TaskState::Failed
                        };
                        record.speed_bps = 0;
                        record.error_message = event["error"]["message"]
                            .as_str()
                            .filter(|message| !message.is_empty())
                            .map(ToOwned::to_owned)
                            .or_else(|| Some(crate::i18n::tr("下载失败", "Download failed")));
                        // Cancellations are the user's own doing; only real
                        // failures earn a desktop notification.
                        if !canceled && self.completed.len() < 32 {
                            let name = record
                                .filename
                                .clone()
                                .unwrap_or_else(|| name_from_url(&record.url));
                            self.completed.push_back(Finished {
                                done: false,
                                name,
                                dir: None,
                                file: None,
                                error: record.error_message.clone(),
                            });
                        }
                    }
                }
                "retry_scheduled" => {
                    if let Some(record) = self.records.get_mut(&task_id) {
                        let delay = event["delay_secs"].as_u64().unwrap_or(0);
                        record.error_message = Some(crate::i18n::tr(
                            format!("{delay} 秒后自动重试"),
                            format!("Retry in {delay}s"),
                        ));
                    }
                }
                "task_deleted" => self.forget(&task_id),
                _ => {}
            }
        }

        fn forget(&mut self, task_id: &str) {
            self.records.remove(task_id);
            self.order.retain(|id| id != task_id);
            if let Some(id) = self.ids.remove(task_id) {
                self.reverse.remove(&id);
            }
            self.history.remove(task_id);
        }

        fn sample_due(&mut self) {
            if self.history.len() < self.records.len() {
                for task_id in self.records.keys() {
                    self.history
                        .entry(task_id.clone())
                        .or_insert_with(|| VecDeque::from(vec![0.0; HISTORY_LEN]));
                }
            }
        }

        fn sample(&mut self) {
            let due = self
                .sampled
                .map(|at| at.elapsed() >= Duration::from_secs(1))
                .unwrap_or(true);
            if !due {
                return;
            }
            self.sampled = Some(Instant::now());
            let mut total = 0.0;
            for (task_id, record) in &self.records {
                let running = matches!(record.state, TaskState::Running | TaskState::Queued)
                    && record.speed_bps > 0;
                let speed = if running {
                    record.speed_bps as f64
                } else {
                    0.0
                };
                total += speed;
                let history = self
                    .history
                    .entry(task_id.clone())
                    .or_insert_with(|| VecDeque::from(vec![0.0; HISTORY_LEN]));
                if history.len() == HISTORY_LEN {
                    history.pop_front();
                }
                history.push_back(speed);
            }
            if self.global.len() == HISTORY_LEN {
                self.global.pop_front();
            }
            self.global.push_back(total);
        }

        fn publish(&mut self, shared: &Arc<Mutex<Shared>>) {
            self.sample_due();
            let pairs: Vec<(String, StoredTask)> = self
                .order
                .iter()
                .filter_map(|task_id| {
                    self.records
                        .get(task_id)
                        .map(|record| (task_id.clone(), record.clone()))
                })
                .collect();
            let tasks = pairs
                .iter()
                .map(|(task_id, record)| self.materialise(task_id, record))
                .collect::<Vec<_>>();
            let mut guard = shared.lock().unwrap();
            while let Some(entry) = self.completed.pop_front() {
                if guard.completed.len() < 32 {
                    guard.completed.push(entry);
                }
            }
            guard.snapshot = Snapshot {
                tasks,
                history: self.global.clone(),
                connected: self.client.is_some(),
                download_dir: self.download_dir.clone(),
                stats: self.stats.clone(),
                plugins: self.plugins.clone(),
                settings: self.settings.clone(),
            };
        }

        fn materialise(&mut self, task_id: &str, record: &StoredTask) -> Task {
            let id = match self.ids.get(task_id) {
                Some(id) => *id,
                None => {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.ids.insert(task_id.to_string(), id);
                    self.reverse.insert(id, task_id.to_string());
                    id
                }
            };
            let history = self
                .history
                .entry(task_id.to_string())
                .or_insert_with(|| VecDeque::from(vec![0.0; HISTORY_LEN]))
                .clone();
            Task {
                id,
                name: record
                    .filename
                    .clone()
                    .unwrap_or_else(|| name_from_url(&record.url)),
                source: source_of(record),
                path: record
                    .target_dir
                    .clone()
                    .unwrap_or_else(|| self.download_dir.clone()),
                note: record.error_message.clone().unwrap_or_default(),
                total: record.total.unwrap_or(0),
                done: record.downloaded as f64,
                status: status_of(record.state),
                torrent: record.plugin_id == "bt" || record.url.starts_with("magnet:"),
                speed: if record.state == TaskState::Running {
                    record.speed_bps as f64
                } else {
                    0.0
                },
                history,
                order: chunk_order(task_id),
                time_ms: record.completed_at.unwrap_or(record.updated_at),
            }
        }
    }

    fn status_of(state: TaskState) -> Status {
        match state {
            TaskState::Running => Status::Downloading,
            TaskState::Queued => Status::Queued,
            TaskState::Paused => Status::Paused,
            TaskState::Completed => Status::Completed,
            TaskState::Failed | TaskState::Canceled => Status::Failed,
        }
    }

    fn name_from_url(url: &str) -> String {
        let without_query = url.split(['?', '#']).next().unwrap_or(url);
        let name = without_query
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(url);
        name.to_string()
    }

    fn source_of(record: &StoredTask) -> String {
        if record.plugin_id == "bt" || record.url.starts_with("magnet:") {
            return "BT · magnet".to_string();
        }
        let authority = record
            .url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(record.url.as_str())
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        let authority = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        let host = if authority.starts_with('[') {
            authority
        } else {
            authority.split(':').next().unwrap_or("")
        };
        if host.is_empty() {
            record.plugin_id.clone()
        } else {
            host.to_string()
        }
    }

    /// Deterministic chunk completion order so the drawer looks alive without
    /// pretending to know the server's real segment schedule.
    fn chunk_order(key: &str) -> [u8; CHUNK_COUNT] {
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        for byte in key.bytes() {
            seed ^= byte as u64;
            seed = seed.wrapping_mul(0x1000_0000_01B3);
        }
        let mut ranks: Vec<u8> = (0..CHUNK_COUNT as u8).collect();
        for index in (1..ranks.len()).rev() {
            let pick = (rand_u32(&mut seed) as usize) % (index + 1);
            ranks.swap(index, pick);
        }
        let mut order = [0_u8; CHUNK_COUNT];
        for (rank, chunk) in ranks.iter().enumerate() {
            order[*chunk as usize] = rank as u8;
        }
        order
    }

    fn rand_u32(state: &mut u64) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 32) as u32
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::backend::split_urls;

        #[test]
        fn chunk_order_is_a_permutation_and_stable() {
            let first = chunk_order("task-1");
            let again = chunk_order("task-1");
            assert_eq!(first, again);
            let mut seen = [false; CHUNK_COUNT];
            for rank in first {
                assert!(!seen[rank as usize], "rank {rank} repeated");
                seen[rank as usize] = true;
            }
            assert_ne!(first, chunk_order("task-2"));
        }

        #[test]
        fn names_and_sources_come_from_the_url() {
            assert_eq!(name_from_url("https://x.test/a/b.iso?token=1"), "b.iso");
            assert_eq!(
                source_of(&StoredTask {
                    id: "t".into(),
                    url: "https://user@host.test:8443/x".into(),
                    plugin_id: "http".into(),
                    filename: None,
                    state: TaskState::Queued,
                    priority: 0,
                    downloaded: 0,
                    total: None,
                    speed_bps: 0,
                    error_kind: None,
                    error_message: None,
                    error_retryable: false,
                    settings: json!({}),
                    plugin_state: None,
                    target_dir: None,
                    retry_attempts: 0,
                    retry_at: None,
                    created_at: 0,
                    updated_at: 0,
                    completed_at: None,
                    files: Vec::new(),
                }),
                "host.test"
            );
        }
        #[test]
        fn split_urls_handles_pasted_batches() {
            // One URL is the common case and must pass through unchanged.
            assert_eq!(
                split_urls("https://example.com/a.bin"),
                vec!["https://example.com/a.bin"]
            );
            // Whitespace-separated batches and multiple magnets queue in
            // order; newline-separated clipboard content counts too.
            assert_eq!(
                split_urls("https://a/x.bin  magnet:?xt=urn:btih:aa\nftp://b/y"),
                vec!["https://a/x.bin", "magnet:?xt=urn:btih:aa", "ftp://b/y"]
            );
            // Tabs and trailing newlines are separators too; blanks drop out.
            assert_eq!(split_urls("  a\t\t b \n"), vec!["a", "b"]);
            assert!(split_urls("   \n ").is_empty());
        }
    }
}
