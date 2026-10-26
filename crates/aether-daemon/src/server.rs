//! Daemon event loop and Unix-socket server.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_host::{
    default_plugins_dir, finalize_task, NewTask, PluginEventEnvelope, PluginManager,
    PluginRegistry, StoredTask, TaskState, TaskStore,
};
use aether_plugin_sdk::{Budget, HostCommand, PluginEvent, TaskPaths, TaskSpec};
use fs2::FileExt;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::proto::{Request, Response};

pub struct DaemonConfig {
    pub download_dir: PathBuf,
    pub plugins_dir: PathBuf,
    pub socket: PathBuf,
    pub host_version: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("downloads"),
            plugins_dir: default_plugins_dir(),
            socket: crate::client::default_socket(),
            host_version: "0.2.0".into(),
        }
    }
}

struct Command {
    request: Request,
    respond: Sender<Response>,
}

struct State {
    manager: PluginManager,
    store: TaskStore,
    download_dir: PathBuf,
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
    let mut state = State {
        manager,
        store,
        download_dir: config.download_dir.clone(),
    };

    recover_tasks(&mut state);

    let (command_tx, command_rx) = mpsc::channel::<Command>();
    let accept_tx = command_tx.clone();
    let accept_thread = thread::spawn(move || accept_loop(listener, accept_tx));

    eprintln!("aether-daemon: listening on {}", config.socket.display());
    let mut shutdown = false;
    while !shutdown {
        while let Ok(command) = command_rx.try_recv() {
            let id = command.request.id;
            let method = command.request.method.clone();
            let response = match handle_request(&mut state, command.request) {
                Ok(value) => Response::ok(id, value),
                Err(error) => Response::err(id, error.to_string()),
            };
            let _ = command.respond.send(response);
            if method == "shutdown" {
                shutdown = true;
            }
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
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let (respond_tx, respond_rx) = mpsc::channel();
                if commands
                    .send(Command {
                        request,
                        respond: respond_tx,
                    })
                    .is_err()
                {
                    break;
                }
                match respond_rx.recv_timeout(Duration::from_secs(300)) {
                    Ok(response) => response,
                    Err(_) => Response::err(0, "daemon request timed out"),
                }
            }
            Err(error) => Response::err(0, format!("invalid request: {error}")),
        };
        if serde_json::to_writer(&mut writer, &response).is_err()
            || writer.write_all(b"\n").is_err()
            || writer.flush().is_err()
        {
            break;
        }
        line.clear();
    }
    let _ = writer.shutdown(Shutdown::Both);
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
        "pause" | "resume" | "cancel" => {
            let task_id = task_id(&request)?;
            let task = state
                .store
                .task(&task_id)?
                .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
            let command = match request.method.as_str() {
                "pause" => HostCommand::Pause {
                    task_id: task_id.clone(),
                },
                "resume" => HostCommand::Resume {
                    task_id: task_id.clone(),
                },
                _ => HostCommand::Cancel {
                    task_id: task_id.clone(),
                },
            };
            state.manager.send(&task.plugin_id, &command)?;
            let next = match request.method.as_str() {
                "pause" => TaskState::Paused,
                "resume" => TaskState::Running,
                _ => TaskState::Canceled,
            };
            state.store.set_state(&task_id, next)?;
            Ok(json!({"task_id": task_id, "state": next}))
        }
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

    if let Err(error) = state.manager.start(
        &plugin_id,
        task,
        TaskPaths { staging, final_dir },
        Budget::default(),
    ) {
        state
            .store
            .fail_task(&task_id, "internal", &error.to_string(), true)?;
        return Err(DaemonError::Host(error));
    }
    state.store.set_state(&task_id, TaskState::Running)?;

    Ok(json!({"task_id": task_id, "plugin_id": plugin_id}))
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
        }
        PluginEvent::Completed { task_id, files, .. } => {
            let task_root = state.download_dir.join(".aether").join(&task_id);
            let staging = task_root.join("staging");
            let final_dir = task_root.join("final");
            match finalize_task(&staging, &final_dir, &files) {
                Ok(_) => {
                    let _ = state.store.complete_task(&task_id, &files);
                }
                Err(error) => {
                    let _ = state
                        .store
                        .fail_task(&task_id, "finalize", &error.to_string(), false);
                }
            }
        }
        PluginEvent::Failed { task_id, error } => {
            let _ = state.store.fail_task(
                &task_id,
                &format!("{:?}", error.kind).to_ascii_lowercase(),
                &error.message,
                error.retryable,
            );
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
        resume_stored(state, task);
    }
}

fn resume_stored(state: &mut State, task: StoredTask) {
    let task_root = state.download_dir.join(".aether").join(&task.id);
    let staging = task_root.join("staging");
    let final_dir = task_root.join("final");
    if fs::create_dir_all(&staging).is_err() {
        return;
    }
    let spec = TaskSpec {
        task_id: task.id.clone(),
        url: task.url.clone(),
        filename: task.filename.clone(),
        settings: task.settings.clone(),
        plugin_state: task.plugin_state.clone(),
        expected_size: task.total,
        checksum: None,
    };
    if state
        .manager
        .start(
            &task.plugin_id,
            spec,
            TaskPaths { staging, final_dir },
            Budget::default(),
        )
        .is_ok()
    {
        let _ = state.store.set_state(&task.id, TaskState::Running);
    }
}

fn task_id(request: &Request) -> Result<String, DaemonError> {
    request
        .task_id
        .clone()
        .ok_or_else(|| DaemonError::Protocol("task_id is required".into()))
}
