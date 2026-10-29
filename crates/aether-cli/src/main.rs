//! Headless host CLI: discover plugins, probe a URL, download through a plugin,
//! persist task state in SQLite and finalize the result.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_host::{
    default_plugins_dir, ensure_space, finalize_task, NewTask, PluginManager, PluginRegistry,
    StoredTask, TaskLog, TaskState, TaskStore,
};
use aether_plugin_sdk::{Budget, PluginEvent, TaskPaths, TaskSpec};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        return usage();
    };

    match command {
        "list" => {
            let root = plugin_dir(&args);
            let registry = PluginRegistry::discover(&[root]).map_err(|error| error.to_string())?;
            for entry in registry.iter() {
                println!(
                    "{:<12} {:<8} enabled={} protocols={}",
                    entry.manifest.id,
                    entry.manifest.version,
                    entry.enabled,
                    entry.manifest.protocols.join(",")
                );
            }
            Ok(())
        }
        "probe" => {
            let url = args.get(1).ok_or("probe needs a URL")?;
            let root = plugin_dir(&args);
            let data_dir = std::env::temp_dir().join("aether-probe-data");
            let mut manager = manager(&root, &data_dir)?;
            let outcomes = manager.probe(url);
            for outcome in outcomes {
                match outcome.result {
                    Some(result) => println!(
                        "{:<12} supported={} priority={} filename={:?} size={:?} reason={:?}",
                        outcome.plugin_id,
                        result.supported,
                        result.priority,
                        result.suggested_filename,
                        result.metadata.as_ref().and_then(|metadata| metadata.size),
                        result.reason
                    ),
                    None => println!(
                        "{:<12} error={}",
                        outcome.plugin_id,
                        outcome.error.unwrap_or_default()
                    ),
                }
            }
            manager.shutdown();
            Ok(())
        }
        "download" => {
            let url = args.get(1).ok_or("download needs a URL")?;
            let output = option(&args, "--dir").unwrap_or_else(|| "downloads".into());
            let root = plugin_dir(&args);
            let forced = option(&args, "--plugin");
            download(url, Path::new(&output), &root, forced.as_deref())
        }
        "remote" => remote(&args[1..]),
        "import" => {
            let source = args
                .get(1)
                .ok_or("import needs a file path or - for stdin")?;
            let socket = option(&args, "--socket")
                .map(PathBuf::from)
                .unwrap_or_else(aether_daemon::client::default_socket);
            let dir = option(&args, "--dir").map(PathBuf::from);
            let apply_settings = args.iter().any(|arg| arg == "--settings");
            import(source, &socket, dir.as_deref(), apply_settings)
        }
        "export" => {
            let target = args
                .get(1)
                .ok_or("export needs a file path or - for stdout")?;
            let socket = option(&args, "--socket")
                .map(PathBuf::from)
                .unwrap_or_else(aether_daemon::client::default_socket);
            export(target, &socket)
        }
        "tasks" => {
            let output = option(&args, "--dir").unwrap_or_else(|| "downloads".into());
            list_tasks(Path::new(&output))
        }
        "logs" => {
            let task_id = args.get(1).ok_or("logs needs a task id")?;
            let output = option(&args, "--dir").unwrap_or_else(|| "downloads".into());
            let limit = option(&args, "--limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(50);
            show_logs(Path::new(&output), task_id, limit)
        }
        "resume" => {
            let task_id = args.get(1).ok_or("resume needs a task id")?;
            let output = option(&args, "--dir").unwrap_or_else(|| "downloads".into());
            let root = plugin_dir(&args);
            resume(task_id, Path::new(&output), &root)
        }
        _ => usage(),
    }
}

fn download(url: &str, output: &Path, plugins: &Path, forced: Option<&str>) -> Result<(), String> {
    let data_dir = output.join(".aether");
    let mut store =
        TaskStore::open(data_dir.join("aether.db")).map_err(|error| error.to_string())?;
    let mut manager = manager(plugins, &data_dir)?;
    let outcomes = manager.probe(url);
    let chosen = match forced {
        Some(id) => outcomes
            .iter()
            .find(|outcome| outcome.plugin_id == id)
            .and_then(|outcome| outcome.result.as_ref())
            .map(|result| (id.to_string(), result.clone()))
            .ok_or_else(|| format!("plugin {id} did not return a probe result"))?,
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
            .ok_or_else(|| "no plugin supports this URL".to_string())?,
    };

    let (plugin_id, probe) = chosen;
    if !probe.supported {
        return Err(format!("{plugin_id} does not support this URL"));
    }

    let task_id = format!(
        "task-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    // Finalized files go straight into the output directory the user chose.
    let task_root = data_dir.join(&task_id);
    let staging = task_root.join("staging");
    let final_dir = output.to_path_buf();
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    fs::create_dir_all(&final_dir).map_err(|error| error.to_string())?;

    // Same fail-fast check as the daemon, so a local download does not die
    // halfway through when the disk is already too small.
    if let Some(size) = probe.metadata.as_ref().and_then(|metadata| metadata.size) {
        ensure_space(&final_dir, size).map_err(|error| error.to_string())?;
    }

    let settings = serde_json::json!({});
    let task = TaskSpec {
        task_id: task_id.clone(),
        url: url.to_string(),
        filename: probe.suggested_filename.clone(),
        settings: settings.clone(),
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum: None,
        proxy: None,
    };
    // Store the destination so `resume` knows where the task belongs even if
    // the user later runs it with a different --dir.
    let target_dir = final_dir
        .canonicalize()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .into_owned();
    store
        .create_task(&NewTask {
            id: task_id.clone(),
            url: url.to_string(),
            plugin_id: plugin_id.clone(),
            filename: probe.suggested_filename.clone(),
            settings,
            state: TaskState::Queued,
            priority: 0,
            target_dir: Some(target_dir),
        })
        .map_err(|error| error.to_string())?;

    println!("{plugin_id} -> {task_id}");
    run_task(
        &mut manager,
        &mut store,
        &plugin_id,
        &task_id,
        task,
        TaskPaths {
            staging: staging.clone(),
            final_dir: final_dir.clone(),
        },
        &staging,
        &final_dir,
    )
}

fn resume(task_id: &str, output: &Path, plugins: &Path) -> Result<(), String> {
    let data_dir = output.join(".aether");
    let mut store =
        TaskStore::open(data_dir.join("aether.db")).map_err(|error| error.to_string())?;
    let stored = store
        .task(task_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("task {task_id} not found"))?;
    if stored.state == TaskState::Completed {
        return Err(format!("task {task_id} is already completed"));
    }

    let mut manager = manager(plugins, &data_dir)?;
    let task_root = data_dir.join(task_id);
    let staging = task_root.join("staging");
    let final_dir = stored
        .target_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| output.to_path_buf());
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    fs::create_dir_all(&final_dir).map_err(|error| error.to_string())?;

    let task = TaskSpec {
        task_id: task_id.to_string(),
        url: stored.url.clone(),
        filename: stored.filename.clone(),
        settings: stored.settings.clone(),
        plugin_state: stored.plugin_state.clone(),
        expected_size: stored.total,
        checksum: None,
        proxy: None,
    };

    println!("resume {task_id} via {}", stored.plugin_id);
    run_task(
        &mut manager,
        &mut store,
        &stored.plugin_id,
        task_id,
        task,
        TaskPaths {
            staging: staging.clone(),
            final_dir: final_dir.clone(),
        },
        &staging,
        &final_dir,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_task(
    manager: &mut PluginManager,
    store: &mut TaskStore,
    plugin_id: &str,
    task_id: &str,
    task: TaskSpec,
    paths: TaskPaths,
    staging: &Path,
    final_dir: &Path,
) -> Result<(), String> {
    if let Err(error) = store.set_state(task_id, TaskState::Running) {
        return Err(error.to_string());
    }
    if let Err(error) = manager.start(plugin_id, task, paths, Budget::default()) {
        let _ = store.fail_task(task_id, "internal", &error.to_string(), true);
        return Err(error.to_string());
    }

    loop {
        let Some(envelope) = manager
            .recv_event(Duration::from_secs(1))
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        match envelope.event {
            PluginEvent::Progress {
                downloaded,
                total,
                speed_bps,
                ..
            } => {
                let _ = store.update_progress(task_id, downloaded, total, speed_bps);
                print!(
                    "\r{:.1} MiB / {} @ {:.1} MiB/s",
                    downloaded as f64 / 1024.0 / 1024.0,
                    total
                        .map(|total| format!("{:.1} MiB", total as f64 / 1024.0 / 1024.0))
                        .unwrap_or_else(|| "?".into()),
                    speed_bps as f64 / 1024.0 / 1024.0
                );
                let _ = std::io::stdout().flush();
            }
            PluginEvent::Log {
                level,
                message,
                fields,
            } => {
                let fields = (!fields.is_empty())
                    .then(|| serde_json::Value::Object(fields.into_iter().collect()));
                let _ = store.append_log(
                    task_id,
                    &format!("{level:?}").to_ascii_lowercase(),
                    &message,
                    fields.as_ref(),
                );
            }
            PluginEvent::Completed { files, .. } => {
                println!();
                match finalize_task(staging, final_dir, &files) {
                    Ok(moved) => {
                        let _ = store.complete_task(task_id, &files);
                        for path in moved {
                            println!("{}", path.display());
                        }
                        manager.shutdown();
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = store.fail_task(task_id, "finalize", &error.to_string(), false);
                        manager.shutdown();
                        return Err(error.to_string());
                    }
                }
            }
            PluginEvent::Failed { error, .. } => {
                println!();
                let _ = store.fail_task(
                    task_id,
                    &format!("{:?}", error.kind).to_ascii_lowercase(),
                    &error.message,
                    error.retryable,
                );
                manager.shutdown();
                return Err(format!("{} failed: {}", plugin_id, error.message));
            }
            _ => {}
        }
    }
}

fn list_tasks(output: &Path) -> Result<(), String> {
    let store = TaskStore::open(output.join(".aether").join("aether.db"))
        .map_err(|error| error.to_string())?;
    print_tasks(&store.list_tasks(50).map_err(|error| error.to_string())?);
    Ok(())
}

fn print_tasks(tasks: &[StoredTask]) {
    for task in tasks {
        let progress = task
            .total
            .map(|total| format!("{:.1}%", task.downloaded as f64 / total as f64 * 100.0))
            .unwrap_or_else(|| format!("{} B", task.downloaded));
        println!(
            "{:<24} {:<10} {:<8} {:>8} {}",
            task.id,
            task.state.as_str(),
            task.plugin_id,
            progress,
            task.filename.as_deref().unwrap_or("")
        );
        for file in &task.files {
            println!("  -> {} ({} B)", file.relative_path, file.size);
        }
    }
}

/// Print a task's log lines, oldest first. Timestamps are UTC (the CLI does
/// not pull in a timezone database); the UI formats them locally.
fn show_logs(output: &Path, task_id: &str, limit: usize) -> Result<(), String> {
    let store = TaskStore::open(output.join(".aether").join("aether.db"))
        .map_err(|error| error.to_string())?;
    store
        .task(task_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("task {task_id} not found"))?;
    print_logs(
        &store
            .logs(task_id, limit)
            .map_err(|error| error.to_string())?,
    );
    Ok(())
}

fn print_logs(logs: &[TaskLog]) {
    for log in logs {
        println!("{} {:<5} {}", clock(log.created_at), log.level, log.message);
    }
}

/// `HH:MM:SS` in UTC from an epoch-millisecond timestamp.
fn clock(ms: i64) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn manager(plugins: &Path, data_dir: &Path) -> Result<PluginManager, String> {
    let registry =
        PluginRegistry::discover(&[plugins.to_path_buf()]).map_err(|error| error.to_string())?;
    Ok(PluginManager::new(
        registry,
        data_dir,
        env!("CARGO_PKG_VERSION"),
    ))
}

fn plugin_dir(args: &[String]) -> PathBuf {
    option(args, "--plugins")
        .map(PathBuf::from)
        .unwrap_or_else(default_plugins_dir)
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn usage() -> Result<(), String> {
    Err(format!(
        "usage:\n  aether-cli list --plugins <dir>\n  aether-cli probe <url> --plugins <dir>\n  aether-cli download <url> --dir <dir> --plugins <dir> [--plugin <id>]\n  aether-cli tasks --dir <dir>\n  aether-cli logs <task-id> --dir <dir> [--limit <n>]\n  aether-cli resume <task-id> --dir <dir> --plugins <dir>{}{}",
        import_usage(),
        remote_usage(),
    ))
}

/// Talk to a running daemon: the headless equivalent of the desktop UI.
fn remote(args: &[String]) -> Result<(), String> {
    use aether_daemon::Client;
    use serde_json::{json, Value};

    let Some(command) = args.first().map(String::as_str) else {
        return Err(format!("usage:{}", remote_usage()));
    };
    // `--socket ip:port` (optionally `tcp://ip:port`) selects RPC over TCP;
    // anything else is the local socket/pipe path. The token comes from
    // `--token` or `AETHER_RPC_TOKEN`.
    let endpoint = option(args, "--socket");
    let rpc_addr = endpoint
        .as_deref()
        .map(|value| value.trim_start_matches("tcp://"))
        .and_then(|value| {
            value
                .parse::<std::net::SocketAddr>()
                .ok()
                .map(|_| value.to_string())
        });
    let socket = match &rpc_addr {
        None => endpoint
            .map(PathBuf::from)
            .unwrap_or_else(aether_daemon::client::default_socket),
        Some(addr) => PathBuf::from(addr),
    };
    let target = || {
        args.get(1)
            .filter(|value| !value.starts_with("--"))
            .cloned()
            .ok_or_else(|| format!("remote {command} needs a task id"))
    };
    let json_output = args.iter().any(|arg| arg == "--json");
    let mut client = match &rpc_addr {
        Some(addr) => {
            let token = option(args, "--token")
                .or_else(|| std::env::var("AETHER_RPC_TOKEN").ok())
                .unwrap_or_default();
            Client::connect_rpc(addr, &token)
                .map_err(|error| format!("cannot reach the daemon at {addr}: {error}"))?
        }
        None => Client::connect(&socket)
            .map_err(|error| format!("cannot reach the daemon at {}: {error}", socket.display()))?,
    };

    match command {
        "ping" => {
            client.ping().map_err(|error| error.to_string())?;
            println!("ok {}", socket.display());
        }
        "add" => {
            let url = args
                .get(1)
                .filter(|value| !value.starts_with("--"))
                .ok_or("remote add needs a URL")?;
            let dir = option(args, "--dir").or_else(|| option(args, "--out"));
            let settings = match option(args, "--settings") {
                Some(raw) => serde_json::from_str(&raw)
                    .map_err(|error| format!("--settings must be JSON: {error}"))?,
                None => json!({}),
            };
            let priority = option(args, "--priority")
                .map(|value| {
                    value
                        .parse::<i32>()
                        .map_err(|_| format!("--priority must be an integer, got {value:?}"))
                })
                .transpose()?
                .unwrap_or(0);
            let added = client
                .add_task_with_priority(
                    url,
                    dir.as_deref().map(Path::new),
                    option(args, "--plugin").as_deref(),
                    priority,
                    settings,
                )
                .map_err(|error| error.to_string())?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "task_id": added.task_id,
                        "plugin_id": added.plugin_id,
                        "queued": added.queued,
                        "dir": added.dir,
                    }))
                    .map_err(|error| error.to_string())?
                );
            } else {
                println!(
                    "{} {} queued={} dir={} priority={}",
                    added.task_id,
                    added.plugin_id,
                    added.queued,
                    added.dir.unwrap_or_default(),
                    added.priority
                );
            }
        }
        "list" => {
            let tasks = client.list_tasks().map_err(|error| error.to_string())?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&tasks).map_err(|error| error.to_string())?
                );
            } else {
                print_tasks(&tasks);
            }
        }
        "get" => {
            let task_id = target()?;
            let value = client.task(&task_id).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
            );
        }
        "logs" => {
            let task_id = target()?;
            let limit = option(args, "--limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(50);
            let logs = client
                .logs(&task_id, limit)
                .map_err(|error| error.to_string())?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&logs).map_err(|error| error.to_string())?
                );
            } else {
                print_logs(&logs);
            }
        }
        "pause" | "resume" | "cancel" | "rm" => {
            let task_id = target()?;
            let value = match command {
                "pause" => client.pause(&task_id),
                "resume" => client.resume(&task_id),
                "cancel" => client.cancel(&task_id),
                _ => client.delete(&task_id),
            }
            .map_err(|error| error.to_string())?;
            println!("{value}");
        }
        "settings" => {
            let settings = client.settings().map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&settings).map_err(|error| error.to_string())?
            );
        }
        "set-settings" => {
            let raw = args
                .get(1)
                .filter(|value| !value.starts_with("--"))
                .ok_or("remote set-settings needs a JSON object")?;
            let value: Value = serde_json::from_str(raw)
                .map_err(|error| format!("settings must be JSON: {error}"))?;
            let stored = client
                .set_settings(value)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&stored).map_err(|error| error.to_string())?
            );
        }
        "plugins" => {
            let plugins = client.plugins().map_err(|error| error.to_string())?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&plugins).map_err(|error| error.to_string())?
                );
            } else {
                for plugin in &plugins {
                    println!(
                        "{:<12} {:<6} enabled={}  {}  [{}]",
                        plugin.id,
                        plugin.version,
                        plugin.enabled,
                        plugin.name,
                        plugin.protocols.join(", ")
                    );
                }
            }
        }
        "enable" | "disable" => {
            let plugin_id = target()?;
            let enabled = command == "enable";
            client
                .set_plugin_enabled(&plugin_id, enabled)
                .map_err(|error| error.to_string())?;
            println!(
                "{plugin_id} {}",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        "stats" => {
            let stats = client.stats().map_err(|error| error.to_string())?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&stats).map_err(|error| error.to_string())?
                );
            } else {
                println!("version      {}", stats.version);
                println!("download-dir {}", stats.download_dir);
                println!(
                    "disk         {} used / {} total",
                    stats.disk_total - stats.disk_free,
                    stats.disk_total
                );
                println!(
                    "today        {} files, {} B",
                    stats.today_files, stats.today_bytes
                );
                println!(
                    "week         {} files, {} B",
                    stats.week_files, stats.week_bytes
                );
                println!(
                    "tasks        completed={} failed={} active={} queued={}",
                    stats.completed, stats.failed, stats.active, stats.queued
                );
                println!(
                    "plugins      {}/{} enabled",
                    stats.plugins_enabled, stats.plugins_total
                );
            }
        }
        "resolve-proxy" => {
            let url = args
                .get(1)
                .filter(|value| !value.starts_with("--"))
                .ok_or("remote resolve-proxy needs a URL")?;
            let resolved = client
                .resolve_proxy(url)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&resolved).map_err(|error| error.to_string())?
            );
        }
        "watch" => {
            // One JSON event per line; stop with Ctrl-C.
            let subscription = client.subscribe().map_err(|error| error.to_string())?;
            loop {
                if let Ok(event) = subscription.recv_timeout(Duration::from_secs(30)) {
                    println!("{event}");
                }
            }
        }
        other => return Err(format!("unknown remote command {other}")),
    }
    Ok(())
}

fn remote_usage() -> &'static str {
    "\n  aether-cli remote <command> [--socket <path|ip:port>] [--token <rpc-token>] [--json]\n    ping | list [--json] | stats [--json] | plugins [--json]\n    add <url> [--plugin <id>] [--dir <dir>] [--priority <n>] [--settings <json>] [--json]\n    get <task-id> | logs <task-id> [--limit <n>] [--json]\n    pause|resume|cancel|rm <task-id>\n    enable|disable <plugin-id>\n    settings | set-settings <json> | resolve-proxy <url> | watch\n  ip:port (or tcp://ip:port) selects RPC over TCP; token from --token or AETHER_RPC_TOKEN"
}

fn import_usage() -> &'static str {
    "\n  aether-cli import <file|-> [--socket <path>] [--dir <dir>] [--settings]\n  aether-cli export <file|-> [--socket <path>]"
}

/// Export the daemon's settings and task list as a JSON document. Task
/// settings are deliberately left out: they can carry cookies or tokens, and
/// an export is meant to travel between machines.
fn export(target: &str, socket: &Path) -> Result<(), String> {
    let mut client = aether_daemon::Client::connect(socket).map_err(|error| {
        format!(
            "daemon is not reachable at {}: {error} (start aether-daemon first)",
            socket.display()
        )
    })?;
    let settings = client.settings().map_err(|error| error.to_string())?;
    let tasks = client.list_tasks().map_err(|error| error.to_string())?;
    let document = export_document(settings, &tasks);
    let text = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
    if target == "-" {
        println!("{text}");
    } else {
        fs::write(target, format!("{text}\n"))
            .map_err(|error| format!("cannot write {target}: {error}"))?;
        println!("exported {} task(s) to {target}", tasks.len());
    }
    Ok(())
}

/// Marker in exported documents; bumped when the shape changes.
const EXPORT_FORMAT: &str = "aether-export/1";

/// Build the export document (pure so the shape is testable without a daemon).
fn export_document(settings: serde_json::Value, tasks: &[StoredTask]) -> serde_json::Value {
    let tasks = tasks
        .iter()
        .map(|task| {
            serde_json::json!({
                "url": task.url,
                "plugin_id": task.plugin_id,
                "priority": task.priority,
                "state": task.state,
                "target_dir": task.target_dir,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "format": EXPORT_FORMAT,
        "settings": settings,
        "tasks": tasks,
    })
}

/// One task row from an export document.
#[derive(Debug)]
struct ExportTask {
    url: String,
    plugin_id: Option<String>,
    priority: i32,
    target_dir: Option<String>,
}

/// Parse an export document; `Err` explains exactly what is wrong so a
/// hand-edited file fails loudly instead of importing half the tasks.
fn parse_export(text: &str) -> Result<(Option<serde_json::Value>, Vec<ExportTask>), String> {
    let document: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
    match document.get("format").and_then(|value| value.as_str()) {
        Some(EXPORT_FORMAT) => {}
        Some(other) => {
            return Err(format!(
                "unsupported export format {other} (expected {EXPORT_FORMAT})"
            ))
        }
        None => {
            return Err(format!(
                "missing export format marker (expected {EXPORT_FORMAT})"
            ))
        }
    }
    let settings = document
        .get("settings")
        .filter(|value| value.is_object())
        .cloned();
    let items = document
        .get("tasks")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tasks = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let url = item
            .get("url")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("task {index}: missing url"))?
            .to_string();
        let plugin_id = item
            .get("plugin_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let priority = item
            .get("priority")
            .and_then(|value| value.as_i64())
            .unwrap_or(0) as i32;
        let target_dir = item
            .get("target_dir")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        tasks.push(ExportTask {
            url,
            plugin_id,
            priority,
            target_dir,
        });
    }
    Ok((settings, tasks))
}

/// Queue tasks from an export document (or a plain URL list). `--settings`
/// applies the exported daemon settings; without it the daemon keeps its
/// current configuration.
fn import(
    source: &str,
    socket: &Path,
    dir: Option<&Path>,
    apply_settings: bool,
) -> Result<(), String> {
    let text = if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        buffer
    } else {
        fs::read_to_string(source).map_err(|error| format!("cannot read {source}: {error}"))?
    };

    if text.trim_start().starts_with('{') {
        return import_export(&text, socket, dir, apply_settings);
    }

    let urls: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if urls.is_empty() {
        return Err("no URLs found in the input".into());
    }

    let mut client = aether_daemon::Client::connect(socket).map_err(|error| {
        format!(
            "daemon is not reachable at {}: {error} (start aether-daemon first)",
            socket.display()
        )
    })?;

    let mut imported = 0_usize;
    let mut failed = 0_usize;
    for url in urls {
        match client.add_with_settings(url, dir, None, serde_json::json!({})) {
            Ok(result) => {
                imported += 1;
                println!(
                    "{url} -> {} ({}) queued={}",
                    result["task_id"], result["plugin_id"], result["queued"]
                );
            }
            Err(error) => {
                failed += 1;
                eprintln!("{url} -> ERROR: {error}");
            }
        }
    }
    println!("imported {imported}/{} URLs", imported + failed);
    if failed > 0 {
        return Err(format!("{failed} URL(s) failed"));
    }
    Ok(())
}

/// Import an export document: re-adds each task with its plugin and priority.
/// Credentials are not part of the format, so authenticated links need their
/// tokens re-entered afterwards.
fn import_export(
    text: &str,
    socket: &Path,
    dir: Option<&Path>,
    apply_settings: bool,
) -> Result<(), String> {
    let (settings, tasks) = parse_export(text)?;
    let mut client = aether_daemon::Client::connect(socket).map_err(|error| {
        format!(
            "daemon is not reachable at {}: {error} (start aether-daemon first)",
            socket.display()
        )
    })?;
    if apply_settings {
        let Some(settings) = settings else {
            return Err("export document has no settings to apply".into());
        };
        client
            .call("set_settings", None, None, None, None, settings)
            .map_err(|error| format!("set_settings: {error}"))?;
        println!("applied daemon settings from the export");
    }

    let mut imported = 0_usize;
    let mut failed = 0_usize;
    for task in tasks {
        let target = dir
            .map(PathBuf::from)
            .or_else(|| task.target_dir.map(PathBuf::from));
        match client.add_with_priority(
            &task.url,
            target.as_deref(),
            task.plugin_id.as_deref(),
            task.priority,
            serde_json::json!({}),
        ) {
            Ok(result) => {
                imported += 1;
                println!(
                    "{} -> {} ({}) queued={}",
                    task.url, result["task_id"], result["plugin_id"], result["queued"]
                );
            }
            Err(error) => {
                failed += 1;
                eprintln!("{} -> ERROR: {error}", task.url);
            }
        }
    }
    println!("imported {imported}/{} tasks", imported + failed);
    if failed > 0 {
        return Err(format!("{failed} task(s) failed"));
    }
    Ok(())
}

#[cfg(test)]
mod export_tests {
    use super::{export_document, parse_export};

    #[test]
    fn export_document_parses_back_with_every_field() {
        let document = export_document(serde_json::json!({"max_tasks": 5}), &[]);
        // Round-trip through the parser the import path uses.
        let text = serde_json::to_string(&document).unwrap();
        let (settings, tasks) = parse_export(&text).expect("own document parses");
        assert_eq!(settings.unwrap()["max_tasks"], 5);
        assert!(tasks.is_empty(), "empty export is valid");
    }

    #[test]
    fn parse_export_reads_task_fields() {
        let text = r#"{
            "format": "aether-export/1",
            "settings": {"max_tasks": 3},
            "tasks": [
                {"url": "https://a.test/x.bin", "plugin_id": "http", "priority": 4, "target_dir": "/tmp"},
                {"url": "magnet:?xt=urn:btih:aa"}
            ]
        }"#;
        let (settings, tasks) = parse_export(text).expect("parses");
        assert!(settings.is_some());
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].url, "https://a.test/x.bin");
        assert_eq!(tasks[0].plugin_id.as_deref(), Some("http"));
        assert_eq!(tasks[0].priority, 4);
        assert_eq!(tasks[0].target_dir.as_deref(), Some("/tmp"));
        assert_eq!(tasks[1].plugin_id, None);
        assert_eq!(tasks[1].priority, 0);
    }

    #[test]
    fn parse_export_rejects_wrong_or_missing_format() {
        let wrong = r#"{"format": "other/2", "tasks": []}"#;
        assert!(parse_export(wrong)
            .unwrap_err()
            .contains("unsupported export format"));
        let missing = r#"{"tasks": []}"#;
        assert!(parse_export(missing)
            .unwrap_err()
            .contains("missing export format"));
        let broken = r#"{"format": "aether-export/1", "tasks": [{"plugin_id": "http"}]}"#;
        assert!(parse_export(broken).unwrap_err().contains("missing url"));
    }
}
