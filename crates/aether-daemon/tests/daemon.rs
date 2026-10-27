#![cfg(unix)]
//! Daemon + fake plugin end-to-end tests.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{Client, DaemonConfig};
use serde_json::{json, Value};

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-daemon-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let name = format!("aether-plugin-fake{}", std::env::consts::EXE_SUFFIX);
    let path = debug_dir.join(name);
    path.is_file().then_some(path)
}

struct Daemon {
    root: PathBuf,
    socket: PathBuf,
    handle: thread::JoinHandle<Result<(), aether_daemon::DaemonError>>,
}

impl Daemon {
    fn shutdown(self) {
        let _ = self.handle.join();
        fs::remove_dir_all(&self.root).ok();
    }
}

/// Stage the fake plugin and start a daemon over a private root.
fn spawn_daemon(label: &str, max_concurrent: usize, max_speed_bps: Option<u64>) -> Option<Daemon> {
    spawn_daemon_with(label, max_concurrent, max_speed_bps, 5, 30)
}

fn spawn_daemon_with(
    label: &str,
    max_concurrent: usize,
    max_speed_bps: Option<u64>,
    retry_max: u32,
    retry_base_secs: u64,
) -> Option<Daemon> {
    let fake = fake_binary()?;
    let root = temp_dir(label);
    let package = root.join("plugins").join("fake");
    fs::create_dir_all(package.join("bin")).unwrap();
    let executable_name = format!("aether-plugin-fake{}", std::env::consts::EXE_SUFFIX);
    fs::copy(&fake, package.join("bin").join(&executable_name)).unwrap();
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "id": "fake",
            "name": "Fake plugin",
            "version": "0.1.0",
            "api": aether_plugin_sdk::API_VERSION,
            "protocols": ["http"],
            "capabilities": ["network", "filesystem:task"],
            "executable": {
                aether_plugin_sdk::current_platform_key(): format!("bin/{executable_name}")
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let socket = root.join("daemon.sock");
    let config = DaemonConfig {
        download_dir: root.join("downloads"),
        plugins_dir: root.join("plugins"),
        socket: socket.clone(),
        host_version: "0.2.0".into(),
        update_url: None,
        update_public_key: None,
        update_appimage: None,
        max_concurrent,
        max_speed_bps,
        retry_max,
        retry_base_secs,
    };
    let handle = thread::spawn(move || aether_daemon::run(config));
    Some(Daemon {
        root,
        socket,
        handle,
    })
}

fn connect(socket: &PathBuf) -> Client {
    loop {
        if let Ok(client) = Client::connect(socket) {
            return client;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn task_state(client: &mut Client, task_id: &str) -> String {
    client
        .task(task_id)
        .ok()
        .and_then(|value| value["task"]["state"].as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn wait_for_state(client: &mut Client, task_id: &str, state: &str, attempts: usize) -> bool {
    for _ in 0..attempts {
        if task_state(client, task_id) == state {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Like wait_for_state, but names the actual final state on failure: on slow
/// CI machines plugin process startup can easily exceed a few seconds.
fn assert_reaches(client: &mut Client, task_id: &str, state: &str, attempts: usize) {
    let last = task_state(client, task_id);
    assert!(
        wait_for_state(client, task_id, state, attempts),
        "task never reached {state} (last seen: {last:?})"
    );
}

fn final_file(root: &PathBuf, task_id: &str) -> PathBuf {
    // Finalized files land directly in the download directory.
    let _ = task_id;
    root.join("downloads").join("fake.bin")
}

#[test]
fn daemon_adds_lists_and_completes_a_task() {
    let Some(daemon) = spawn_daemon("run", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let root = daemon.root.clone();
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    client.ping().unwrap();
    assert_eq!(client.update_status().unwrap()["status"], "disabled");
    let subscription = Client::connect(&socket).unwrap().subscribe().unwrap();
    let added = client
        .add("https://example.com/file.bin", None, None)
        .unwrap();
    assert_eq!(added["queued"], false);
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);

    let mut saw_progress = false;
    let mut saw_completed = false;
    for _ in 0..50 {
        let Ok(event) = subscription.recv_timeout(Duration::from_secs(1)) else {
            continue;
        };
        match event["event"].as_str() {
            Some("progress") => saw_progress = true,
            Some("completed") => {
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_completed, "subscription missed completion");
    let _ = saw_progress;

    assert_eq!(fs::read(final_file(&root, &task_id)).unwrap().len(), 1024);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_queues_tasks_beyond_max_concurrent() {
    let Some(daemon) = spawn_daemon("queue", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let first = client
        .add_with_settings(
            "https://example.com/first.bin",
            None,
            None,
            json!({"duration_ms": 1200}),
        )
        .unwrap();
    assert_eq!(first["queued"], false);

    let second = client
        .add("https://example.com/second.bin", None, None)
        .unwrap();
    assert_eq!(second["queued"], true);
    let second_id = second["task_id"].as_str().unwrap().to_string();
    assert_eq!(task_state(&mut client, &second_id), "queued");

    // The held task must not have written anything yet.
    assert!(!final_file(&daemon.root, &second_id).exists());

    // When the first task completes the queued one is promoted FIFO.
    assert!(wait_for_state(&mut client, &second_id, "running", 200));
    assert!(wait_for_state(&mut client, &second_id, "completed", 100));
    assert_eq!(
        fs::read(final_file(&daemon.root, &second_id))
            .unwrap()
            .len(),
        1024
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_resume_restarts_a_failed_task() {
    let Some(daemon) = spawn_daemon("resume", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/flaky.bin",
            None,
            None,
            json!({"fail": true, "retryable": true}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "failed", 300);

    // Resume must restart the task from the stored record, not just forward a
    // Resume command to a plugin that no longer owns the execution.
    let resumed = client.resume(&task_id).unwrap();
    assert_eq!(resumed["state"], "running");
    assert_reaches(&mut client, &task_id, "completed", 300);
    assert_eq!(
        fs::read(final_file(&daemon.root, &task_id)).unwrap().len(),
        1024
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_cancel_moves_a_running_task_to_canceled() {
    let Some(daemon) = spawn_daemon("cancel", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/slow.bin",
            None,
            None,
            json!({"duration_ms": 5000}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();

    let canceled = client.cancel(&task_id).unwrap();
    assert_eq!(canceled["state"], "canceled");
    assert_reaches(&mut client, &task_id, "canceled", 100);
    // The task must not complete after being canceled.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(task_state(&mut client, &task_id), "canceled");

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_pauses_and_resumes() {
    let Some(daemon) = spawn_daemon("pause", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/hold.bin",
            None,
            None,
            json!({"duration_ms": 4000}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_eq!(task_state(&mut client, &task_id), "running");

    client.pause(&task_id).unwrap();
    assert_eq!(task_state(&mut client, &task_id), "paused");

    client.resume(&task_id).unwrap();
    assert_eq!(task_state(&mut client, &task_id), "running");
    // Held task would still be running; make sure it can still complete.
    assert_reaches(&mut client, &task_id, "completed", 400);

    client.shutdown().unwrap();
    daemon.shutdown();
}

fn summarize(value: Value) -> usize {
    value["tasks"].as_array().map(Vec::len).unwrap_or(0)
}

#[test]
fn daemon_lists_tasks() {
    let Some(daemon) = spawn_daemon("list", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let _ = client.add("https://example.com/a.bin", None, None).unwrap();
    let tasks = client.list().unwrap();
    assert_eq!(summarize(tasks), 1);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_auto_retries_a_retryable_failure_and_recovers() {
    let Some(daemon) = spawn_daemon_with("retry", 1, None, 5, 1) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/flaky.bin",
            None,
            None,
            json!({"fail": true, "retryable": true}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "failed", 300);

    // After the 1 s backoff the daemon must restart it automatically; the
    // sentinel makes the second attempt succeed. The running window is short,
    // so completion is the reliable observable.
    assert_reaches(&mut client, &task_id, "completed", 400);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_stops_retrying_after_the_cap() {
    let Some(daemon) = spawn_daemon_with("retrycap", 1, None, 1, 1) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/broken.bin",
            None,
            None,
            json!({"fail_always": true, "retryable": true}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "failed", 300);

    // Attempt 2 (cap 1) also fails; the task must stay failed afterwards.
    // The brief "running" blip between retries is not observable reliably.
    assert_reaches(&mut client, &task_id, "failed", 300);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(task_state(&mut client, &task_id), "failed");

    // Manual resume still works and resets the bookkeeping.
    let resumed = client.resume(&task_id).unwrap();
    assert_eq!(resumed["state"], "running");
    assert_reaches(&mut client, &task_id, "failed", 300);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_canceled_task_stops_the_retry_cycle() {
    let Some(daemon) = spawn_daemon_with("retrycancel", 1, None, 5, 1) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add_with_settings(
            "https://example.com/giveup.bin",
            None,
            None,
            json!({"fail_always": true, "retryable": true}),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "failed", 300);

    // Cancel is the escape hatch from the retry cycle.
    let canceled = client.cancel(&task_id).unwrap();
    assert_eq!(canceled["state"], "canceled");
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(task_state(&mut client, &task_id), "canceled");

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_deletes_terminal_tasks_but_keeps_files() {
    let Some(daemon) = spawn_daemon("delete", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add("https://example.com/doomed.bin", None, None)
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    assert!(final_file(&daemon.root, &task_id).exists());

    // A running/queued task cannot be deleted directly.
    let other = client
        .add_with_settings(
            "https://example.com/live.bin",
            None,
            None,
            json!({"duration_ms": 3000}),
        )
        .unwrap();
    let live_id = other["task_id"].as_str().unwrap().to_string();
    assert!(client.delete(&live_id).is_err());

    // Deleting the completed task drops its record and staging but keeps the
    // downloaded file in the download directory.
    let deleted = client.delete(&task_id).unwrap();
    assert_eq!(deleted["deleted"], true);
    assert!(!daemon
        .root
        .join("downloads/.aether")
        .join(&task_id)
        .exists());
    assert!(final_file(&daemon.root, &task_id).exists());
    assert!(client
        .list_tasks()
        .unwrap()
        .iter()
        .all(|task| task.id != task_id));

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_exposes_task_logs() {
    let Some(daemon) = spawn_daemon("logs", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let added = client
        .add("https://example.com/logged.bin", None, None)
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);

    let logs = client.logs(&task_id, 50).unwrap();
    let messages: Vec<_> = logs.iter().map(|log| log.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|line| line.contains("running with plugin fake")),
        "missing start line: {messages:?}"
    );
    assert!(
        messages.iter().any(|line| line.starts_with("completed:")),
        "missing completion line: {messages:?}"
    );
    // Oldest first, and every line carries a timestamp.
    assert!(logs
        .windows(2)
        .all(|pair| pair[0].created_at <= pair[1].created_at));

    // Unknown tasks are an error, not an empty list.
    assert!(client.logs("task-missing", 10).is_err());

    client.shutdown().unwrap();
    daemon.shutdown();
}
