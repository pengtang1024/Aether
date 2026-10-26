//! Headless host CLI: discover plugins, probe a URL, download through a plugin
//! and finalize the result with the same Host rules used by the GUI.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_host::{default_plugins_dir, finalize_task, PluginManager, PluginRegistry};
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
            let mut manager = manager(&root)?;
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
        _ => usage(),
    }
}

fn download(url: &str, output: &Path, plugins: &Path, forced: Option<&str>) -> Result<(), String> {
    let mut manager = manager(plugins)?;
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
    let task_root = output.join(".aether").join(&task_id);
    let staging = task_root.join("staging");
    let final_dir = task_root.join("final");
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;

    let task = TaskSpec {
        task_id: task_id.clone(),
        url: url.to_string(),
        filename: probe.suggested_filename.clone(),
        settings: serde_json::json!({}),
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum: None,
    };

    manager
        .start(
            &plugin_id,
            task,
            TaskPaths {
                staging: staging.clone(),
                final_dir: final_dir.clone(),
            },
            Budget::default(),
        )
        .map_err(|error| error.to_string())?;

    println!("{plugin_id} -> {task_id}");
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
            PluginEvent::Completed { files, .. } => {
                println!();
                let moved = finalize_task(&staging, &final_dir, &files)
                    .map_err(|error| error.to_string())?;
                for path in moved {
                    println!("{}", path.display());
                }
                manager.shutdown();
                return Ok(());
            }
            PluginEvent::Failed { error, .. } => {
                println!();
                manager.shutdown();
                return Err(format!(
                    "{} failed: {:?} {}",
                    plugin_id, error.kind, error.message
                ));
            }
            _ => {}
        }
    }
}

fn manager(plugins: &Path) -> Result<PluginManager, String> {
    let registry =
        PluginRegistry::discover(&[plugins.to_path_buf()]).map_err(|error| error.to_string())?;
    Ok(PluginManager::new(registry, "aether-data", "0.2.0"))
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
        "usage:\n  aether-cli list --plugins <dir>\n  aether-cli probe <url> --plugins <dir>\n  aether-cli download <url> --dir <dir> --plugins <dir> [--plugin <id>]"
            .into(),
    )
}
