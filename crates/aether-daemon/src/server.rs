//! Daemon event loop and Unix-socket server.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_host::{
    default_plugins_dir, finalize_task, NewTask, PluginEventEnvelope, PluginManager,
    PluginRegistry, StoredTask, TaskState, TaskStore,
};
use aether_plugin_sdk::{Budget, HostCommand, PluginEvent, TaskPaths, TaskSpec};
use aether_update::UpdateClient;
use fs2::FileExt;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::proto::{Request, Response};

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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("downloads"),
            plugins_dir: default_plugins_dir(),
            socket: crate::client::default_socket(),
            host_version: "0.2.0".into(),
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
        }
    }
}

struct Command {
    request: Request,
    respond: Sender<Response>,
    subscriber: Option<Sender<serde_json::Value>>,
}

struct State {
    manager: PluginManager,
    store: TaskStore,
    download_dir: PathBuf,
    active: HashSet<String>,
    update: Option<Arc<Mutex<serde_json::Value>>>,
    subscribers: Vec<Sender<serde_json::Value>>,
    max_concurrent: usize,
    max_speed_bps: Option<u64>,
    /// Tasks whose plugin execution thread is alive in this daemon session
    /// (running or paused). A task outside this set must be (re)started from
    /// its stored record instead of receiving an in-process Resume.
    live: HashSet<String>,
    /// Tasks whose cancel command is still being executed by their plugin.
    canceling: HashSet<String>,
}

pub fn run(config: DaemonConfig) -> Result<(), DaemonError> {
    let data_dir = config.download_dir.join(".aether");
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(config.socket.parent().unwrap_or(Path::new(".")))?;
    let lock_path = config.socket.with_extension("lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| DaemonError::AlreadyRunning)?;

    if config.socket.exists() {
        if UnixStream::connect(&config.socket).is_ok() {
            return Err(DaemonError::AlreadyRunning);
        }
        let _ = fs::remove_file(&config.socket);
    }

    let listener = UnixListener::bind(&config.socket)?;
    fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;

    let registry = PluginRegistry::discover(&[config.plugins_dir.clone()])?;
    let manager = PluginManager::new(registry, &data_dir, config.host_version.clone());
    let store = TaskStore::open(data_dir.join("aether.db"))?;
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
                status.clone(),
            );
            Some(status)
        }
        _ => None,
    };

    let mut state = State {
        manager,
        store,
        download_dir: config.download_dir.clone(),
        active: HashSet::new(),
        update: update_status,
        subscribers: Vec::new(),
        max_concurrent: config.max_concurrent,
        max_speed_bps: config.max_speed_bps,
        live: HashSet::new(),
        canceling: HashSet::new(),
    };

    recover_tasks(&mut state);

    let (command_tx, command_rx) = mpsc::channel::<Command>();
    let accept_tx = command_tx.clone();
    let accept_thread = thread::spawn(move || accept_loop(listener, accept_tx));

    eprintln!("aether-daemon: listening on {}", config.socket.display());
    let mut shutdown = false;
    let mut last_idle_cleanup = std::time::Instant::now();
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
            continue;
        }

        // Active downloads: drain commands, then poll plugin events.
        while let Ok(command) = command_rx.try_recv() {
            shutdown |= dispatch(&mut state, command);
        }
        match state.manager.recv_event(Duration::from_millis(100)) {
            Ok(Some(envelope)) => handle_event(&mut state, envelope),
            Ok(None) => {}
            Err(error) => eprintln!("aether-daemon: plugin event error: {error}"),
        }
    }

    state.manager.shutdown();
    drop(accept_thread);
    let _ = fs::remove_file(&config.socket);
    Ok(())
}

fn spawn_update_loop(
    manifest_url: String,
    public_key: String,
    appimage: Option<PathBuf>,
    data_dir: PathBuf,
    status: Arc<Mutex<serde_json::Value>>,
) {
    thread::spawn(move || {
        // Do not delay daemon startup for a network check.
        thread::sleep(Duration::from_secs(30));
        loop {
            let client = match UpdateClient::new(
                &manifest_url,
                &public_key,
                env!("CARGO_PKG_VERSION"),
                aether_plugin_sdk::current_platform_key(),
                &data_dir,
            ) {
                Ok(client) => client,
                Err(error) => {
                    *status.lock().unwrap() = json!({
                        "status": "error",
                        "error": error.to_string(),
                    });
                    thread::sleep(Duration::from_secs(6 * 3600));
                    continue;
                }
            };

            match client.check() {
                Ok(None) => {
                    *status.lock().unwrap() = json!({"status": "up_to_date"});
                }
                Ok(Some(manifest)) => {
                    let result = client.asset_for_platform(&manifest).and_then(|asset| {
                        let artifact = client.download(&manifest, asset)?;
                        if asset.kind == "appimage" {
                            if let Some(target) = &appimage {
                                client.apply_appimage(&artifact, target)?;
                            }
                        }
                        Ok((artifact, manifest.version.clone()))
                    });
                    *status.lock().unwrap() = match result {
                        Ok((artifact, version)) => json!({
                            "status": "ready",
                            "latest": version,
                            "artifact": artifact.to_string_lossy(),
                        }),
                        Err(error) => json!({
                            "status": "error",
                            "error": error.to_string(),
                        }),
                    };
                }
                Err(error) => {
                    *status.lock().unwrap() = json!({
                        "status": "error",
                        "error": error.to_string(),
                    });
                }
            }
            thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

fn dispatch(state: &mut State, command: Command) -> bool {
    let id = command.request.id;
    let method = command.request.method.clone();
    if method == "subscribe" {
        if let Some(tx) = command.subscriber {
            state.subscribers.push(tx);
        }
        let _ = command.respond.send(Response::ok(id, json!({"ok": true})));
        return false;
    }
    let response = match handle_request(state, command.request) {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::err(id, error.to_string()),
    };
    let _ = command.respond.send(response);
    method == "shutdown"
}

fn broadcast(state: &mut State, event: Value) {
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

fn accept_loop(listener: UnixListener, commands: Sender<Command>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let commands = commands.clone();
        thread::spawn(move || client_loop(stream, commands));
    }
}

fn client_loop(stream: UnixStream, commands: Sender<Command>) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut line = String::new();

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = Response::err(0, format!("invalid request: {error}"));
                if write_response(&mut writer, &response).is_err() {
                    break;
                }
                line.clear();
                continue;
            }
        };
        let subscribe = request.method == "subscribe";
        let (respond_tx, respond_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        if commands
            .send(Command {
                request,
                respond: respond_tx,
                subscriber: subscribe.then_some(event_tx),
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
        if write_response(&mut writer, &response).is_err() {
            break;
        }
        if subscribe {
            if !accepted {
                break;
            }
            loop {
                match event_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(event) => {
                        if serde_json::to_writer(&mut writer, &event).is_err()
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
    let _ = writer.shutdown(Shutdown::Both);
}

fn write_response(writer: &mut UnixStream, response: &Response) -> std::io::Result<()> {
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
            let tasks = state.store.list_tasks(100)?;
            Ok(json!({ "tasks": tasks }))
        }
        "get" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            Ok(json!({ "task": task }))
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
        "cancel" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            if task.state.is_terminal() && task.state != TaskState::Canceled {
                return Err(DaemonError::Protocol(format!(
                    "task {task_id} is already {}",
                    task.state.as_str()
                )));
            }
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
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "canceled"}),
            );
            promote_queued(state);
            Ok(json!({"task_id": task_id, "state": "canceled"}))
        }
        "update_status" => Ok(state.update.as_ref().map_or_else(
            || json!({"status": "disabled"}),
            |status| status.lock().unwrap().clone(),
        )),
        "shutdown" => Ok(json!({"ok": true})),
        other => Err(DaemonError::Protocol(format!("unknown method {other}"))),
    }
}

fn add_task(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let url = request
        .url
        .ok_or_else(|| DaemonError::Protocol("add needs url".into()))?;
    let outcomes = state.manager.probe(&url);
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
            .ok_or_else(|| DaemonError::Protocol("no plugin supports this URL".into()))?,
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
    let task_root = state.download_dir.join(".aether").join(&task_id);
    let staging = task_root.join("staging");
    let final_dir = task_root.join("final");
    fs::create_dir_all(&staging)?;

    let settings = request.settings;
    state.store.create_task(&NewTask {
        id: task_id.clone(),
        url: url.clone(),
        plugin_id: plugin_id.clone(),
        filename: probe.suggested_filename.clone(),
        settings: settings.clone(),
        state: TaskState::Queued,
    })?;

    let task = TaskSpec {
        task_id: task_id.clone(),
        url,
        filename: probe.suggested_filename,
        settings,
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum: None,
    };

    let started = match try_launch(state, &plugin_id, task, staging, final_dir) {
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

    Ok(json!({
        "task_id": task_id,
        "plugin_id": plugin_id,
        "queued": !started,
    }))
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
    state.active.insert(task_id.clone());
    state.live.insert(task_id.clone());
    broadcast(
        state,
        json!({"event": "task_state", "task_id": task_id, "state": "running"}),
    );
    Ok(true)
}

/// Promote the oldest queued tasks while there is capacity.
fn promote_queued(state: &mut State) {
    while state.active.len() < state.max_concurrent {
        let next = state.store.list_tasks(100).ok().and_then(|tasks| {
            tasks
                .into_iter()
                .filter(|task| task.state == TaskState::Queued)
                // list_tasks is created_at DESC; the queue runs FIFO.
                .last()
        });
        let Some(task) = next else { return };
        match start_from_record(state, &task) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                let _ = state
                    .store
                    .fail_task(&task.id, "internal", &error.to_string(), true);
                continue;
            }
        }
    }
}

/// (Re)start a task from its stored record if capacity allows. Ok(true) when
/// the task is running afterwards, Ok(false) when it stayed queued.
fn start_from_record(state: &mut State, task: &StoredTask) -> Result<bool, DaemonError> {
    let task_root = state.download_dir.join(".aether").join(&task.id);
    let staging = task_root.join("staging");
    let final_dir = task_root.join("final");
    fs::create_dir_all(&staging)?;
    let spec = TaskSpec {
        task_id: task.id.clone(),
        url: task.url.clone(),
        filename: task.filename.clone(),
        settings: task.settings.clone(),
        plugin_state: task.plugin_state.clone(),
        expected_size: task.total,
        checksum: None,
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
        PluginEvent::Completed { task_id, files, .. } => {
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            let task_root = state.download_dir.join(".aether").join(&task_id);
            let staging = task_root.join("staging");
            let final_dir = task_root.join("final");
            match finalize_task(&staging, &final_dir, &files) {
                Ok(_) => {
                    let _ = state.store.complete_task(&task_id, &files);
                    broadcast(
                        state,
                        json!({"event": "completed", "task_id": task_id, "files": files}),
                    );
                }
                Err(error) => {
                    let _ = state
                        .store
                        .fail_task(&task_id, "finalize", &error.to_string(), false);
                    broadcast(
                        state,
                        json!({"event": "failed", "task_id": task_id, "error": error.to_string()}),
                    );
                }
            }
            promote_queued(state);
        }
        PluginEvent::Failed { task_id, error } => {
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.canceling.remove(&task_id);
            if error.kind == aether_plugin_sdk::ErrorKind::Canceled {
                let _ = state.store.set_state(&task_id, TaskState::Canceled);
            } else {
                let _ = state.store.fail_task(
                    &task_id,
                    &format!("{:?}", error.kind).to_ascii_lowercase(),
                    &error.message,
                    error.retryable,
                );
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

fn task_id(request: &Request) -> Result<String, DaemonError> {
    request
        .task_id
        .clone()
        .ok_or_else(|| DaemonError::Protocol("task_id is required".into()))
}
