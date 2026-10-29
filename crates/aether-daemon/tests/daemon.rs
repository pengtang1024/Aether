#![cfg(unix)]
//! Daemon + fake plugin end-to-end tests.

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
        let root = self.root.clone();
        self.stop();
        fs::remove_dir_all(&root).ok();
    }

    /// Stop the daemon but keep its directory, to simulate a restart.
    fn stop(self) {
        let _ = self.handle.join();
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
    spawn_daemon_full(
        label,
        max_concurrent,
        max_speed_bps,
        retry_max,
        retry_base_secs,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_daemon_full(
    label: &str,
    max_concurrent: usize,
    max_speed_bps: Option<u64>,
    retry_max: u32,
    retry_base_secs: u64,
    proxy: Option<&str>,
) -> Option<Daemon> {
    spawn_daemon_at(
        temp_dir(label),
        max_concurrent,
        max_speed_bps,
        retry_max,
        retry_base_secs,
        proxy,
        false,
    )
}

/// Start a daemon that only stages the real HTTP plugin, for tests that need
/// actual network behaviour (user agent, DNS servers, IP mode).
fn spawn_http_daemon(label: &str) -> Option<Daemon> {
    spawn_daemon_at(temp_dir(label), 1, None, 5, 30, None, true)
}

/// Stage the built `aether-plugin-http` binary plus its manifest. Returns
/// `None` when the binary has not been built yet; the caller skips then.
fn stage_http_plugin(root: &Path) -> Option<()> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let binary = debug_dir.join(format!(
        "aether-plugin-http{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !binary.is_file() {
        return None;
    }
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("aether-plugin-http")
        .join("plugin.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let package = root.join("plugins").join("http");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let relative = manifest["executable"][aether_plugin_sdk::current_platform_key()]
        .as_str()
        .unwrap()
        .to_string();
    fs::copy(&binary, package.join(relative)).unwrap();
    Some(())
}

/// Start a daemon over an existing root; used to simulate a restart.
#[allow(clippy::too_many_arguments)]
fn spawn_daemon_at(
    root: PathBuf,
    max_concurrent: usize,
    max_speed_bps: Option<u64>,
    retry_max: u32,
    retry_base_secs: u64,
    proxy: Option<&str>,
    stage_http: bool,
) -> Option<Daemon> {
    if stage_http {
        stage_http_plugin(&root)?;
    } else {
        let fake = fake_binary()?;
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
    }

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
        proxy: proxy.map(ToOwned::to_owned),
        pac_url: None,
        rpc_listen: None,
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

    // When the first task completes the queued one is promoted FIFO. The
    // no-hold fake task can start and finish between two polls, so completion
    // is the observable proof that the promotion happened.
    assert_reaches(&mut client, &second_id, "completed", 300);
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
fn daemon_applies_scheduler_limits_from_settings() {
    // Boot with room for four so the settings page has something to change.
    let Some(daemon) = spawn_daemon("limits", 4, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let mut client = connect(&socket);

    // Startup resolution reports what runs instead of `null`.
    let settings = client.settings().unwrap();
    assert_eq!(settings["max_concurrent"], 4);
    assert_eq!(settings["max_speed_bps"], 0);
    assert_eq!(settings["retry_max"], 5);
    assert_eq!(settings["connections"], 4);

    // Two long tasks fit the boot cap of four.
    let first = client
        .add_with_settings(
            "https://example.com/first.bin",
            None,
            None,
            json!({"duration_ms": 1500}),
        )
        .unwrap();
    let second = client
        .add_with_settings(
            "https://example.com/second.bin",
            None,
            None,
            json!({"duration_ms": 1500}),
        )
        .unwrap();
    assert_eq!(first["queued"], false);
    assert_eq!(second["queued"], false);

    // Lowering the cap to one queues new work without touching the pair
    // that is already running.
    let updated = client.set_settings(json!({"max_concurrent": 1})).unwrap();
    assert_eq!(updated["max_concurrent"], 1);
    let third = client
        .add_with_settings(
            "https://example.com/third.bin",
            None,
            None,
            json!({"duration_ms": 400}),
        )
        .unwrap();
    assert_eq!(third["queued"], true);
    let third_id = third["task_id"].as_str().unwrap().to_string();

    // A cap with no capacity, an out-of-range connection count, a typo'd key
    // and a non-object patch are all rejected; the stored value stays at one.
    assert!(client.set_settings(json!({"max_concurrent": 0})).is_err());
    assert!(client.set_settings(json!({"connections": 17})).is_err());
    assert!(client.set_settings(json!({"max_concurent": 4})).is_err());
    assert!(client.set_settings(json!(42)).is_err());
    assert_eq!(client.settings().unwrap()["max_concurrent"], 1);

    // Raising the cap releases the queue right away: the third task starts
    // while the first two are still running.
    let updated = client.set_settings(json!({"max_concurrent": 4})).unwrap();
    assert_eq!(updated["max_concurrent"], 4);
    assert_reaches(&mut client, &third_id, "completed", 300);
    let first_id = first["task_id"].as_str().unwrap().to_string();
    let second_id = second["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &first_id, "completed", 300);
    assert_reaches(&mut client, &second_id, "completed", 300);

    // A valid patch round-trips and is what a restart would load.
    let updated = client
        .set_settings(json!({"connections": 8, "retry_max": 0}))
        .unwrap();
    assert_eq!(updated["connections"], 8);
    assert_eq!(updated["retry_max"], 0);
    let raw = fs::read_to_string(daemon.root.join("downloads/.aether/settings.json")).unwrap();
    assert!(raw.contains("\"max_concurrent\": 4"), "{raw}");

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
    // The brief "running" blip between retries is not observable reliably, so
    // wait for the record to lose its pending-retry deadline instead: that is
    // what "the cap was hit" looks like.
    for _ in 0..100 {
        if let Ok(record) = client.task_record(&task_id) {
            if record.retry_at.is_none() && record.retry_attempts > 0 {
                break;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    let record = client.task_record(&task_id).unwrap();
    assert_eq!(record.state, aether_host::TaskState::Failed);
    assert_eq!(record.retry_attempts, 1, "cap must stop after one retry");
    assert!(record.retry_at.is_none(), "no retry may stay pending");
    let starts = client
        .logs(&task_id, 100)
        .unwrap()
        .iter()
        .filter(|log| log.message.starts_with("running with plugin"))
        .count();
    assert_eq!(starts, 2, "exactly one retry may run");
    assert!(client
        .logs(&task_id, 100)
        .unwrap()
        .iter()
        .any(|log| log.message.contains("auto-retry exhausted")));

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

#[test]
fn daemon_updates_and_persists_proxy_settings() {
    let Some(daemon) = spawn_daemon("settings", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let settings_path = daemon.root.join("downloads/.aether/settings.json");

    let mut client = connect(&socket);
    // Nothing configured yet: the plugin default (honor the environment).
    assert!(client.settings().unwrap()["proxy"].is_null());

    let updated = client
        .set_settings(json!({
            "proxy": {"mode": "custom", "url": "http://10.0.0.1:3128",
                      "username": "user", "password": "secret"}
        }))
        .unwrap();
    assert_eq!(updated["proxy"]["mode"], "custom");
    assert_eq!(updated["proxy"]["url"], "http://10.0.0.1:3128");

    // Persisted next to the database, readable only by the user.
    let raw = fs::read_to_string(&settings_path).unwrap();
    assert!(raw.contains("10.0.0.1:3128"), "{raw}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "settings may hold proxy credentials");
    }

    // Invalid settings are rejected without touching the stored value.
    assert!(client
        .set_settings(json!({"proxy": {"mode": "custom"}}))
        .is_err());
    assert!(client
        .set_settings(json!({"proxy": {"mode": "hyperdrive"}}))
        .is_err());
    assert_eq!(
        client.settings().unwrap()["proxy"]["url"],
        "http://10.0.0.1:3128"
    );

    // Selecting "no proxy" clears the URL.
    let cleared = client
        .set_settings(json!({"proxy": {"mode": "none"}}))
        .unwrap();
    assert_eq!(cleared["proxy"]["mode"], "none");
    assert!(cleared["proxy"]["url"].is_null());

    // `null` removes the setting entirely.
    let removed = client.set_settings(json!({"proxy": null})).unwrap();
    assert!(removed["proxy"].is_null());

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_uses_the_proxy_from_the_environment() {
    let Some(daemon) =
        spawn_daemon_full("proxy-env", 1, None, 5, 30, Some("socks5://127.0.0.1:1080"))
    else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let settings = client.settings().unwrap();
    assert_eq!(settings["proxy"]["mode"], "custom");
    assert_eq!(settings["proxy"]["url"], "socks5://127.0.0.1:1080");
    // The environment override is per run: it must not rewrite the user's
    // persisted settings behind their back.
    assert!(!daemon.root.join("downloads/.aether/settings.json").exists());

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_honors_the_destination_directory() {
    let Some(daemon) = spawn_daemon("target-dir", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let root = daemon.root.clone();
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);

    // Absolute "save as": the file must not land in the default directory.
    let target = root.join("elsewhere/nested");
    let added = client
        .add("https://example.com/fake.bin", Some(&target), None)
        .unwrap();
    assert_eq!(added["dir"], target.to_string_lossy().as_ref());
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    assert!(
        target.join("fake.bin").exists(),
        "not saved to the target dir"
    );
    assert!(!root.join("downloads/fake.bin").exists());

    // Relative paths are resolved inside the download root.
    let added = client
        .add(
            "https://example.com/fake.bin",
            Some(Path::new("sub/inner")),
            None,
        )
        .unwrap();
    let relative_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &relative_id, "completed", 300);
    assert!(root.join("downloads/sub/inner/fake.bin").exists());

    // Escaping the download root is rejected.
    assert!(client
        .add(
            "https://example.com/fake.bin",
            Some(Path::new("../escape")),
            None
        )
        .is_err());

    // The record keeps the destination so a restart can resume into it.
    let record = client.task_record(&task_id).unwrap();
    assert_eq!(
        record.target_dir,
        Some(target.to_string_lossy().into_owned())
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_resolves_pac_per_url() {
    let Some(daemon) = spawn_daemon("pac", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let script = r#"
        function FindProxyForURL(url, host) {
            if (host === "proxied.example") { return "PROXY 127.0.0.1:9"; }
            if (shExpMatch(url, "*socks*")) { return "SOCKS5 127.0.0.1:1080"; }
            return "DIRECT";
        }
    "#;
    let settings = client
        .set_settings(json!({"proxy": {"mode": "pac", "script": script}}))
        .unwrap();
    assert_eq!(settings["proxy"]["mode"], "pac");

    let proxied = client
        .resolve_proxy("http://proxied.example/file.bin")
        .unwrap();
    assert_eq!(proxied["pac"], true);
    assert_eq!(proxied["proxy"]["url"], "http://127.0.0.1:9");
    assert!(proxied["pac_error"].is_null());

    let socks = client
        .resolve_proxy("https://example.com/socks/list")
        .unwrap();
    assert_eq!(socks["proxy"]["url"], "socks5://127.0.0.1:1080");

    // PAC "DIRECT" maps to the explicit no-proxy mode, not to "unset".
    let direct = client
        .resolve_proxy("https://example.com/plain.bin")
        .unwrap();
    assert_eq!(direct["proxy"]["mode"], "none");

    // Invalid PAC configuration is rejected, the old one stays active.
    assert!(client
        .set_settings(json!({"proxy": {"mode": "pac"}}))
        .is_err());
    assert!(client
        .set_settings(json!({"proxy": {"mode": "pac", "url": "http://x/wpad.dat", "script": "x"}}))
        .is_err());
    assert_eq!(
        client.settings().unwrap()["proxy"]["script"].as_str(),
        Some(script)
    );

    // A task added under PAC still completes (the fake plugin ignores proxies,
    // but the daemon must not fail while resolving).
    let added = client
        .add("https://example.com/pac-task.bin", None, None)
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);

    // A broken script resolves to "no proxy override" plus an error for the UI.
    let broken = client
        .set_settings(json!({"proxy": {"mode": "pac", "script": "function FindProxyForURL( {"}}))
        .unwrap();
    assert_eq!(broken["proxy"]["mode"], "pac");
    let failed = client.resolve_proxy("http://proxied.example/").unwrap();
    assert!(failed["proxy"].is_null());
    assert!(failed["pac_error"]
        .as_str()
        .unwrap_or_default()
        .contains("script"));

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_resumes_pending_retries_after_a_restart() {
    let root = temp_dir("retryrestart");
    let Some(daemon) = spawn_daemon_at(root.clone(), 1, None, 1, 2, None, false) else {
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

    // A retry is scheduled two seconds out; stop the daemon before it fires.
    let record = client.task_record(&task_id).unwrap();
    assert_eq!(record.retry_attempts, 1);
    assert!(record.retry_at.is_some(), "retry must be pending");
    client.shutdown().unwrap();
    daemon.stop();

    // The restarted daemon must pick the schedule up and run the retry.
    let Some(daemon) = spawn_daemon_at(root, 1, None, 1, 2, None, false) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let mut client = connect(&socket);

    let mut restored = false;
    for _ in 0..150 {
        if let Ok(record) = client.task_record(&task_id) {
            if record.state == aether_host::TaskState::Failed && record.retry_at.is_none() {
                restored = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(restored, "retry did not complete after restart");

    let logs = client.logs(&task_id, 200).unwrap();
    assert!(
        logs.iter().any(|log| log.message.contains("after restart")),
        "missing restore log: {:?}",
        logs.iter().map(|log| &log.message).collect::<Vec<_>>()
    );
    let starts = logs
        .iter()
        .filter(|log| log.message.starts_with("running with plugin"))
        .count();
    assert_eq!(starts, 2, "one attempt before, one after the restart");

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_manual_resume_drops_a_pending_retry() {
    let Some(daemon) = spawn_daemon_at(temp_dir("retryresume"), 1, None, 5, 60, None, false) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    // `fail` fails exactly once: the plugin records that it already tried, so
    // the manual resume succeeds and we can observe a cleared retry state.
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
    let record = client.task_record(&task_id).unwrap();
    assert_eq!(record.retry_attempts, 1);
    assert_eq!(record.retry_at.is_some(), true);

    // An explicit resume clears the pending schedule and restarts the counter,
    // so the retry does not fire a second time behind the user's back.
    client.resume(&task_id).unwrap();
    let record = client.task_record(&task_id).unwrap();
    assert!(record.retry_at.is_none());
    assert_eq!(record.retry_attempts, 0);
    assert_reaches(&mut client, &task_id, "completed", 300);
    let record = client.task_record(&task_id).unwrap();
    assert_eq!(record.retry_attempts, 0);
    assert!(record.retry_at.is_none());

    client.shutdown().unwrap();
    daemon.shutdown();
}

/// Desktop proxy detection (`mode: "system"`) needs a fake `gsettings`, so this
/// runs the real daemon binary as a child process with its own environment.
#[test]
fn daemon_follows_the_desktop_proxy() {
    let root = temp_dir("desktop-proxy");
    fs::create_dir_all(root.join("plugins")).unwrap();
    fs::create_dir_all(root.join("downloads")).unwrap();

    let stub = root.join("gsettings");
    fs::write(
        &stub,
        "#!/bin/sh\ncase \"$2\" in\n  \"org.gnome.system.proxy mode\") echo \"'manual'\" ;;\n  \"org.gnome.system.proxy.http host\") echo \"'127.0.0.1'\" ;;\n  \"org.gnome.system.proxy.http port\") echo \"3128\" ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let socket = root.join("daemon.sock");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_aether-daemon"))
        .arg("--download-dir")
        .arg(root.join("downloads"))
        .arg("--plugins")
        .arg(root.join("plugins"))
        .env("AETHER_SOCKET", &socket)
        .env("AETHER_GSETTINGS", &stub)
        .env_remove("AETHER_PROXY")
        .env_remove("AETHER_PAC_URL")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .spawn()
        .unwrap();

    let mut client = connect(&socket);
    let resolved = client
        .resolve_proxy("https://example.com/file.bin")
        .unwrap();
    assert_eq!(resolved["proxy"]["mode"], "custom");
    assert_eq!(resolved["proxy"]["url"], "http://127.0.0.1:3128");
    client.shutdown().unwrap();
    child.wait().unwrap();

    // "Automatic" desktop mode hands a PAC URL to the host evaluator.
    let pac = root.join("proxy.pac");
    fs::write(
        &pac,
        "function FindProxyForURL(url, host){ if (host === 'corp.example') { return 'PROXY 10.1.1.1:8080'; } return 'DIRECT'; }",
    )
    .unwrap();
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\ncase \"$2\" in\n  \"org.gnome.system.proxy mode\") echo \"'auto'\" ;;\n  \"org.gnome.system.proxy autoconfig-url\") echo \"'file://{}'\" ;;\n  *) exit 1 ;;\nesac\n",
            pac.display()
        ),
    )
    .unwrap();
    let _ = fs::remove_file(&socket);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_aether-daemon"))
        .arg("--download-dir")
        .arg(root.join("downloads"))
        .arg("--plugins")
        .arg(root.join("plugins"))
        .env("AETHER_SOCKET", &socket)
        .env("AETHER_GSETTINGS", &stub)
        .env_remove("AETHER_PROXY")
        .env_remove("AETHER_PAC_URL")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .spawn()
        .unwrap();

    let mut client = connect(&socket);
    let corp = client.resolve_proxy("https://corp.example/x").unwrap();
    assert_eq!(corp["proxy"]["url"], "http://10.1.1.1:8080");
    let other = client.resolve_proxy("https://example.com/x").unwrap();
    assert_eq!(other["proxy"]["mode"], "none");
    client.shutdown().unwrap();
    child.wait().unwrap();

    fs::remove_dir_all(&root).ok();
}

#[test]
fn daemon_promotes_queued_tasks_by_priority() {
    let Some(daemon) = spawn_daemon("priority", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    // Hold the single slot so the next three tasks stay queued.
    let holder = client
        .add_with_settings(
            "https://example.com/holder.bin",
            None,
            None,
            json!({"duration_ms": 1000}),
        )
        .unwrap();
    let holder_id = holder["task_id"].as_str().unwrap().to_string();

    let mut add = |url: &str, priority: i32| {
        let added = client
            .add_with_priority(url, None, None, priority, json!({"duration_ms": 700}))
            .unwrap();
        assert_eq!(added["queued"], true);
        assert_eq!(added["priority"], priority);
        added["task_id"].as_str().unwrap().to_string()
    };
    let normal = add("https://example.com/normal.bin", 0);
    let high = add("https://example.com/high.bin", 10);
    let low = add("https://example.com/low.bin", -5);

    assert_reaches(&mut client, &holder_id, "completed", 300);
    // The high-priority task must take the free slot first, and keep it long
    // enough that the others cannot have started yet.
    wait_started(&mut client, &high, 100);
    assert!(started(&mut client, &high));
    assert!(!started(&mut client, &normal));
    assert!(!started(&mut client, &low));
    assert_reaches(&mut client, &high, "completed", 300);

    wait_started(&mut client, &normal, 100);
    assert!(!started(&mut client, &low), "low priority jumped the queue");
    assert_reaches(&mut client, &normal, "completed", 300);

    assert_reaches(&mut client, &low, "completed", 300);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn plugin_toggles_persist_and_refuse_running_work() {
    let Some(daemon) = spawn_daemon("plugins", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let root = daemon.root.clone();
    let socket = daemon.socket.clone();

    let mut client = connect(&socket);
    let plugins = client.plugins().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].id, "fake");
    assert!(plugins[0].enabled);

    // A running task blocks the switch-off.
    let running = client
        .add_with_settings(
            "https://example.com/x.bin",
            None,
            None,
            json!({"duration_ms": 5000}),
        )
        .unwrap();
    let running_id = running["task_id"].as_str().unwrap().to_string();
    wait_started(&mut client, &running_id, 100);
    let error = client
        .set_plugin_enabled("fake", false)
        .expect_err("disable must refuse while tasks run");
    assert!(error.to_string().contains("running tasks"));
    client.cancel(&running_id).unwrap();
    assert_reaches(&mut client, &running_id, "canceled", 100);

    // Idle now: disabling succeeds and the plugin no longer takes work.
    client.set_plugin_enabled("fake", false).unwrap();
    assert!(!client.plugins().unwrap()[0].enabled);
    let error = client
        .add("https://example.com/x.bin", None, None)
        .expect_err("disabled plugins must not accept work");
    assert!(error.to_string().contains("no plugin supports"));

    // The choice survives a restart.
    client.shutdown().unwrap();
    drop(client);
    daemon.stop();

    let restarted =
        spawn_daemon_at(root, 1, None, 5, 30, None, false).expect("restart over the existing root");
    let mut client = connect(&restarted.socket);
    assert!(!client.plugins().unwrap()[0].enabled);

    // Turning it back on also persists.
    client.set_plugin_enabled("fake", true).unwrap();
    assert!(client.plugins().unwrap()[0].enabled);

    client.shutdown().unwrap();
    restarted.shutdown();
}

#[test]
fn stats_counts_completed_work_and_disk() {
    let Some(daemon) = spawn_daemon("stats", 1, None) else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };
    let mut client = connect(&daemon.socket);

    let stats = client.stats().unwrap();
    assert_eq!(stats.plugins_total, 1);
    assert_eq!(stats.plugins_enabled, 1);
    assert_eq!(stats.completed, 0);
    assert!(!stats.version.is_empty());
    assert!(!stats.download_dir.is_empty());
    assert!(stats.disk_total > 0);

    let added = client
        .add_with_settings(
            "https://example.com/f.bin",
            None,
            None,
            json!({"duration_ms": 800}),
        )
        .unwrap();
    let id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &id, "completed", 300);

    let stats = client.stats().unwrap();
    assert_eq!(stats.completed, 1);
    assert_eq!(stats.today_files, 1);
    assert!(stats.today_bytes > 0);
    assert_eq!(stats.week_files, 1);
    assert_eq!(stats.week_bytes, stats.today_bytes);
    assert_eq!(stats.active, 0);

    client.shutdown().unwrap();
    daemon.shutdown();
}

/// True once the task's plugin execution has started (log line is durable).
fn started(client: &mut Client, task_id: &str) -> bool {
    client
        .logs(task_id, 100)
        .map(|logs| {
            logs.iter()
                .any(|log| log.message.starts_with("running with plugin"))
        })
        .unwrap_or(false)
}

fn wait_started(client: &mut Client, task_id: &str, attempts: usize) {
    for _ in 0..attempts {
        if started(client, task_id) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("task {task_id} never started");
}

// ---- network settings reach the real HTTP plugin ---------------------------

/// Recorded probe/download requests: "<METHOD> <User-Agent>".
type RequestLog = Arc<Mutex<Vec<String>>>;

/// Tiny HTTP server: answers every request with a 1024-byte body and records
/// the method and User-Agent it saw.
struct HttpStub {
    addr: SocketAddr,
    requests: RequestLog,
}

fn spawn_http_stub(ip: IpAddr) -> Option<HttpStub> {
    let listener = TcpListener::bind(SocketAddr::new(ip, 0)).ok()?;
    let addr = listener.local_addr().unwrap();
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            let mut data = Vec::new();
            let mut buffer = [0_u8; 4096];
            // Read until the end of the request headers (GET/HEAD have no body).
            loop {
                let Ok(size) = stream.read(&mut buffer) else {
                    break;
                };
                if size == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..size]);
                if data.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&data);
            let method = text.split(' ').next().unwrap_or_default().to_string();
            let header = |wanted: &str| {
                text.lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case(wanted)
                            .then(|| value.trim().to_string())
                    })
                    .unwrap_or_default()
            };
            let user_agent = header("User-Agent");
            let referer = header("Referer");
            log.lock()
                .unwrap()
                .push(format!("{method} {user_agent} | referer: {referer}"));
            let body = vec![7_u8; 1024];
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if method != "HEAD" {
                let _ = stream.write_all(&body);
            }
        }
    });
    Some(HttpStub { addr, requests })
}

/// Minimal DNS over UDP: answers A queries with 127.0.0.1 and AAAA with ::1.
/// Returns the stub address plus the queries it answered ("qtype=qtype name"),
/// for assertions and failure diagnostics.
fn spawn_dns_stub() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = queries.clone();
    thread::spawn(move || {
        let mut buffer = [0_u8; 1500];
        while let Ok((size, source)) = socket.recv_from(&mut buffer) {
            if size < 17 {
                continue;
            }
            let query = &buffer[..size];
            // Find the end of the question: labels, root label, qtype, qclass.
            let mut index = 12;
            let mut name = Vec::new();
            while index < size && buffer[index] != 0 {
                let label_len = buffer[index] as usize;
                name.extend_from_slice(&buffer[index + 1..index + 1 + label_len]);
                name.push(b'.');
                index += 1 + label_len;
            }
            let question_end = index + 1 + 4;
            if question_end > size {
                continue;
            }
            let qtype = u16::from_be_bytes([buffer[index + 1], buffer[index + 2]]);
            log.lock().unwrap().push(format!(
                "qtype={qtype} name={}",
                String::from_utf8_lossy(&name)
            ));
            // A -> 127.0.0.1, AAAA -> ::1; the HTTP stub listens on IPv4, so
            // an AAAA answer still fails to connect and reqwest falls through
            // to the A address.
            let (record_type, rdata): (u16, Vec<u8>) = if qtype == 28 {
                (28, vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
            } else {
                (1, vec![127, 0, 0, 1])
            };
            let mut response = Vec::with_capacity(question_end + 16);
            response.extend_from_slice(&query[0..2]); // transaction id
            response.extend_from_slice(&[0x81, 0x80]); // QR|RD, RA, RCODE 0
            response.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]); // QDCOUNT/ANCOUNT
            response.extend_from_slice(&query[12..question_end]); // echoed question
            response.extend_from_slice(&[0xC0, 0x0C]); // name pointer
            response.extend_from_slice(&record_type.to_be_bytes());
            response.extend_from_slice(&[0, 1, 0, 0, 0, 60]); // class IN, TTL 60
            response.extend_from_slice(&(rdata.len() as u16).to_be_bytes()); // RDLENGTH
            response.extend_from_slice(&rdata);
            let _ = socket.send_to(&response, source);
        }
    });
    (addr, queries)
}

fn observed_user_agents(stub: &HttpStub) -> Vec<String> {
    stub.requests
        .lock()
        .unwrap()
        .iter()
        .map(|entry| {
            entry
                .split_once(' ')
                .map(|(_, agent)| agent.split(" | ").next().unwrap_or_default().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn clear_requests(stub: &HttpStub) {
    stub.requests.lock().unwrap().clear();
}

#[test]
fn daemon_hands_the_user_agent_to_http_plugins() {
    let Some(daemon) = spawn_http_daemon("user-agent") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let Some(stub) = spawn_http_stub(IpAddr::from([127, 0, 0, 1])) else {
        eprintln!("cannot bind the http stub; skipping");
        return;
    };
    let mut client = connect(&socket);

    // The default user agent is the built-in Aether one.
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/default.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    let agents = observed_user_agents(&stub);
    assert!(!agents.is_empty(), "the stub saw no requests");
    assert!(
        agents.iter().all(|agent| agent.starts_with("Aether/0.2.0")),
        "unexpected default user agents: {agents:?}"
    );

    // A daemon-wide user agent overrides the built-in one on every request.
    client
        .set_settings(json!({"user_agent": "AetherTest/42"}))
        .unwrap();
    clear_requests(&stub);
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/custom.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    let agents = observed_user_agents(&stub);
    assert!(
        agents.iter().all(|agent| agent == "AetherTest/42"),
        "the custom user agent did not reach the plugin: {agents:?}"
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_applies_default_headers_to_http_tasks() {
    let Some(daemon) = spawn_http_daemon("headers") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let Some(stub) = spawn_http_stub(IpAddr::from([127, 0, 0, 1])) else {
        eprintln!("cannot bind the http stub; skipping");
        return;
    };
    let mut client = connect(&socket);

    let referers = || -> Vec<String> {
        stub.requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.split("referer: ").nth(1).map(ToOwned::to_owned))
            .collect()
    };

    // Without the setting no request carries a Referer.
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/plain.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    assert!(
        referers().iter().all(|referer| referer.is_empty()),
        "a Referer appeared without the setting"
    );

    // The daemon-wide header reaches every request of the next task.
    client
        .set_settings(json!({"default_headers": "Referer: https://ref.test/a"}))
        .unwrap();
    clear_requests(&stub);
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/tagged.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    let seen = referers();
    assert!(
        !seen.is_empty() && seen.iter().all(|referer| referer == "https://ref.test/a"),
        "the default header did not reach the plugin: {seen:?}"
    );

    // A malformed list is rejected before anything changes.
    let error = client
        .set_settings(json!({"default_headers": "Referer https://ref.test"}))
        .unwrap_err();
    assert!(
        error.to_string().contains("header"),
        "unexpected rejection: {error}"
    );

    // Clearing the setting removes the header again.
    client
        .set_settings(json!({"default_headers": null}))
        .unwrap();
    clear_requests(&stub);
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/cleared.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);
    assert!(
        referers().iter().all(|referer| referer.is_empty()),
        "the header survived a null patch"
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_schedule_gates_task_starts() {
    let Some(daemon) = spawn_http_daemon("schedule") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let Some(stub) = spawn_http_stub(IpAddr::from([127, 0, 0, 1])) else {
        eprintln!("cannot bind the http stub; skipping");
        return;
    };
    let mut client = connect(&socket);

    // A window starting 12 hours from now can never contain now, whatever
    // time the CI happens to run.
    use chrono::Timelike;
    let now = chrono::Local::now();
    let now_minutes = now.hour() * 60 + now.minute();
    let fmt = |minutes: u32| format!("{:02}:{:02}", minutes / 60, minutes % 60);
    client
        .set_settings(json!({
            "schedule_on": true,
            "schedule_start": fmt((now_minutes + 720) % 1440),
            "schedule_end": fmt((now_minutes + 780) % 1440),
        }))
        .unwrap();

    // A bad time is rejected outright.
    assert!(client
        .set_settings(json!({"schedule_start": "25:00"}))
        .is_err());

    // Outside the window the task stays queued and the stub sees nothing.
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/scheduled.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    // The add probe legitimately touches the stub; the download must not.
    clear_requests(&stub);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        task_state(&mut client, &task_id),
        "queued",
        "the schedule did not gate the launch"
    );
    assert!(
        stub.requests.lock().unwrap().is_empty(),
        "the plugin was started outside the window"
    );

    // Disabling the schedule lifts the gate at once and the task runs.
    client
        .set_settings(json!({
            "schedule_on": false,
            "schedule_start": fmt((now_minutes + 720) % 1440),
            "schedule_end": fmt((now_minutes + 780) % 1440),
        }))
        .unwrap();
    assert_reaches(&mut client, &task_id, "completed", 300);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_hands_dns_servers_to_http_plugins() {
    let Some(daemon) = spawn_http_daemon("dns") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let Some(stub) = spawn_http_stub(IpAddr::from([127, 0, 0, 1])) else {
        eprintln!("cannot bind the http stub; skipping");
        return;
    };
    let (dns_addr, dns_queries) = spawn_dns_stub();
    let mut client = connect(&socket);

    client
        .set_settings(json!({"dns": format!("127.0.0.1:{}", dns_addr.port())}))
        .unwrap();

    // `.test` is reserved for testing (RFC 6761): real resolvers have no
    // answers for it, and it is not a special-purpose zone for the resolver
    // itself (unlike `.invalid`, which hickory answers locally without any
    // network query), so the query must reach the configured DNS server.
    let host = format!(
        "aether-dns-{}.test",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let url = format!("http://{host}:{}/dns.bin", stub.addr.port());
    let added = match client.add(&url, None, None) {
        Ok(added) => added,
        Err(error) => {
            eprintln!("add failed: {error}");
            eprintln!("http stub requests: {:#?}", stub.requests.lock().unwrap());
            eprintln!("dns stub queries: {:#?}", dns_queries.lock().unwrap());
            panic!("add rejected a resolvable URL: {error}");
        }
    };
    let task_id = added["task_id"].as_str().unwrap().to_string();
    if !wait_for_state(&mut client, &task_id, "completed", 300) {
        eprintln!("http stub requests: {:#?}", stub.requests.lock().unwrap());
        eprintln!("dns stub queries: {:#?}", dns_queries.lock().unwrap());
        eprintln!("task logs: {:#?}", client.logs(&task_id, 50).unwrap());
        panic!(
            "task never completed (state: {})",
            task_state(&mut client, &task_id)
        );
    }
    // The download's name lookup must have gone to the configured server.
    let queries = dns_queries.lock().unwrap();
    assert!(
        queries.iter().any(|entry| entry.contains(&host)),
        "the DNS stub saw no query for {host}: {queries:?}"
    );
    assert_eq!(
        fs::read(daemon.root.join("downloads").join("dns.bin"))
            .unwrap()
            .len(),
        1024
    );

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_ip_mode_rejects_ipv4_targets_in_ipv6_only_mode() {
    let Some(daemon) = spawn_http_daemon("ip-mode-v6") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let Some(stub) = spawn_http_stub(IpAddr::from([127, 0, 0, 1])) else {
        eprintln!("cannot bind the http stub; skipping");
        return;
    };
    let mut client = connect(&socket);

    client
        .set_settings(json!({"ip_mode": "ipv6_only"}))
        .unwrap();
    // A literal IPv4 target can never satisfy IPv6-only: the socket is bound
    // to the IPv6 wildcard, so the download fails (probe errors are swallowed
    // on purpose to keep URLs addable, the failure surfaces on the task).
    let added = client
        .add(
            &format!("http://127.0.0.1:{}/v4.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "failed", 300);

    client.shutdown().unwrap();
    daemon.shutdown();
}

#[test]
fn daemon_ip_mode_allows_ipv6_targets_in_ipv6_only_mode() {
    // Some CI runners disable IPv6; skip instead of failing there.
    let Some(stub) = spawn_http_stub(IpAddr::V6(Ipv6Addr::LOCALHOST)) else {
        eprintln!("IPv6 loopback is not available; skipping");
        return;
    };
    let Some(daemon) = spawn_http_daemon("ip-mode-v6-ok") else {
        eprintln!("http plugin binary is not built; skipping");
        return;
    };
    let socket = daemon.socket.clone();
    let mut client = connect(&socket);

    client
        .set_settings(json!({"ip_mode": "ipv6_only"}))
        .unwrap();
    let added = client
        .add(
            &format!("http://[::1]:{}/v6.bin", stub.addr.port()),
            None,
            None,
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    assert_reaches(&mut client, &task_id, "completed", 300);

    client.shutdown().unwrap();
    daemon.shutdown();
}
