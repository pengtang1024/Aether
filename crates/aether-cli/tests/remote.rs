#![cfg(unix)]
//! `aether-cli remote ...` against a real daemon with the fake plugin.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{run, Client, DaemonConfig};
use serde_json::{json, Value};

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-remote-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_daemon(root: &Path) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug_dir = exe.parent().unwrap().parent().unwrap();
    let fake = debug_dir.join(format!(
        "aether-plugin-fake{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(fake.is_file(), "fake plugin binary is not built");

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
        max_concurrent: 1,
        max_speed_bps: None,
        retry_max: 0,
        retry_base_secs: 30,
        proxy: None,
        pac_url: None,
        rpc_listen: None,
    };
    thread::spawn(move || run(config));
    loop {
        if Client::connect(&socket).is_ok() {
            return socket;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn remote(socket: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aether-cli"));
    command.arg("remote").args(args).arg("--socket").arg(socket);
    command.output().unwrap()
}

fn remote_ok(socket: &Path, args: &[&str]) -> String {
    let output = remote(socket, args);
    assert!(
        output.status.success(),
        "remote {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn task_state(socket: &Path, task_id: &str) -> String {
    let output = remote_ok(socket, &["get", task_id]);
    let value: Value = serde_json::from_str(&output).unwrap();
    value["task"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn remote_controls_a_running_daemon() {
    let root = temp_dir("control");
    let socket = spawn_daemon(&root);

    assert!(remote_ok(&socket, &["ping"]).starts_with("ok"));
    assert_eq!(remote_ok(&socket, &["list"]).trim(), "");

    // add -> id, plugin and destination on one line
    let added = remote_ok(
        &socket,
        &[
            "add",
            "https://example.com/remote.bin",
            "--priority",
            "3",
            "--settings",
            r#"{"duration_ms": 400}"#,
        ],
    );
    let task_id = added.split_whitespace().next().unwrap().to_string();
    assert!(added.contains("fake"), "{added}");
    assert!(added.contains("priority=3"), "{added}");
    let listed = remote_ok(&socket, &["get", &task_id]);
    assert_eq!(
        serde_json::from_str::<Value>(&listed).unwrap()["task"]["priority"],
        3
    );

    // JSON listing is machine readable and carries the same task.
    let listed = remote_ok(&socket, &["list", "--json"]);
    let tasks: Vec<Value> = serde_json::from_str(&listed).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"].as_str(), Some(task_id.as_str()));

    // The fake task finishes on its own; logs come back oldest first.
    for _ in 0..100 {
        if task_state(&socket, &task_id) == "completed" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(task_state(&socket, &task_id), "completed");
    let logs = remote_ok(&socket, &["logs", &task_id]);
    assert!(logs.contains("completed:"), "{logs}");

    // A held task can be paused, resumed and canceled from the CLI.
    let added = remote_ok(
        &socket,
        &[
            "add",
            "https://example.com/hold.bin",
            "--settings",
            r#"{"duration_ms": 5000}"#,
        ],
    );
    let held = added.split_whitespace().next().unwrap().to_string();
    assert!(remote_ok(&socket, &["pause", &held]).contains("\"paused\""));
    assert_eq!(task_state(&socket, &held), "paused");
    assert!(remote_ok(&socket, &["resume", &held]).contains("\"running\""));
    assert!(remote_ok(&socket, &["cancel", &held]).contains("\"canceled\""));
    assert_eq!(task_state(&socket, &held), "canceled");

    // Only terminal tasks can be deleted.
    assert!(remote(&socket, &["rm", &held]).status.success());
    assert!(!remote_ok(&socket, &["list", "--json"]).contains(&held));

    // Settings round-trip, including an invalid object being rejected. A
    // fresh daemon reports the scheduler limits it resolved at startup and
    // nothing else: proxy and network stay absent until someone sets them.
    let settings: Value = serde_json::from_str(&remote_ok(&socket, &["settings"])).unwrap();
    let mut keys: Vec<&str> = settings
        .as_object()
        .expect("settings is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "connections",
            "max_concurrent",
            "max_speed_bps",
            "retry_max"
        ],
        "{settings}"
    );
    // The test daemon runs one task at a time, without a speed cap or retries.
    assert_eq!(settings["max_concurrent"], 1, "{settings}");
    assert_eq!(settings["max_speed_bps"], 0, "{settings}");
    assert_eq!(settings["retry_max"], 0, "{settings}");
    assert_eq!(settings["connections"], 4, "{settings}");
    let stored = remote_ok(&socket, &["set-settings", r#"{"proxy":{"mode":"none"}}"#]);
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["proxy"]["mode"],
        "none"
    );
    assert!(
        !remote(&socket, &["set-settings", r#"{"proxy":{"mode":"nope"}}"#])
            .status
            .success()
    );
    let resolved = remote_ok(&socket, &["resolve-proxy", "https://example.com/x"]);
    let resolved: Value = serde_json::from_str(&resolved).unwrap();
    assert_eq!(resolved["proxy"], Value::Null);
    remote_ok(&socket, &["set-settings", r#"{"proxy":null}"#]);

    Client::connect(&socket).unwrap().shutdown().unwrap();
    fs::remove_dir_all(&root).ok();
}
