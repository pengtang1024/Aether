//! Headless host CLI: discover plugins, probe a URL, download through a plugin,
//! persist task state in SQLite and finalize the result.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_host::{
    default_plugins_dir, finalize_task, NewTask, PluginManager, PluginRegistry, TaskState,
    TaskStore,
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
        "import" => {
            let source = args
                .get(1)
                .ok_or("import needs a file path or - for stdin")?;
            let socket = option(&args, "--socket")
                .map(PathBuf::from)
                .unwrap_or_else(aether_daemon::client::default_socket);
            let dir = option(&args, "--dir").map(PathBuf::from);
            import(source, &socket, dir.as_deref())
        }
        "tasks" => {
            let output = option(&args, "--dir").unwrap_or_else(|| "downloads".into());
            list_tasks(Path::new(&output))
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

    let settings = serde_json::json!({});
    let task = TaskSpec {
        task_id: task_id.clone(),
        url: url.to_string(),
        filename: probe.suggested_filename.clone(),
        settings: settings.clone(),
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum: None,
    };
    store
        .create_task(&NewTask {
            id: task_id.clone(),
            url: url.to_string(),
            plugin_id: plugin_id.clone(),
            filename: probe.suggested_filename.clone(),
            settings,
            state: TaskState::Queued,
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
    let final_dir = output.to_path_buf();
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;

    let task = TaskSpec {
        task_id: task_id.to_string(),
        url: stored.url.clone(),
        filename: stored.filename.clone(),
        settings: stored.settings.clone(),
        plugin_state: stored.plugin_state.clone(),
        expected_size: stored.total,
        checksum: None,
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
    for task in store.list_tasks(50).map_err(|error| error.to_string())? {
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
    Ok(())
}

fn manager(plugins: &Path, data_dir: &Path) -> Result<PluginManager, String> {
    let registry =
        PluginRegistry::discover(&[plugins.to_path_buf()]).map_err(|error| error.to_string())?;
    Ok(PluginManager::new(registry, data_dir, "0.2.0"))
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
    Err(
        "usage:\n  aether-cli list --plugins <dir>\n  aether-cli probe <url> --plugins <dir>\n  aether-cli download <url> --dir <dir> --plugins <dir> [--plugin <id>]\n  aether-cli tasks --dir <dir>\n  aether-cli resume <task-id> --dir <dir> --plugins <dir>\n  aether-cli import <file|-> [--socket <path>] [--dir <dir>]"
            .into(),
    )
}

/// Queue many URLs through the running daemon: one per line, `#` comments and
/// blank lines ignored. Each add returns task_id/plugin_id/queued; failures
/// are reported per URL and reflected in the exit code.
fn import(source: &str, socket: &Path, dir: Option<&Path>) -> Result<(), String> {
    let text = if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        buffer
    } else {
        fs::read_to_string(source).map_err(|error| format!("cannot read {source}: {error}"))?
    };

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
