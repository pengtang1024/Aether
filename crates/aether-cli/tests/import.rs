#![cfg(unix)]
//! `aether-cli import` against a running daemon with the fake plugin.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{run, DaemonConfig};
use serde_json::json;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-import-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_daemon(root: &PathBuf) -> PathBuf {
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
        if aether_daemon::Client::connect(&socket).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    socket
}

#[test]
fn import_queues_urls_through_the_daemon() {
    let root = temp_dir("run");
    let socket = spawn_daemon(&root);

    let list = root.join("urls.txt");
    fs::write(
        &list,
        "# comment line\nhttps://example.com/first.bin\n\nhttps://example.com/second.bin\n",
    )
    .unwrap();

    let cli = env!("CARGO_BIN_EXE_aether-cli");
    let output = Command::new(cli)
        .args([
            "import",
            list.to_string_lossy().as_ref(),
            "--socket",
            socket.to_string_lossy().as_ref(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("task-").count(), 2, "stdout: {stdout}");
    assert!(stdout.contains("queued=false"));
    assert!(stdout.contains("queued=true"));
    assert!(stdout.contains("imported 2/2"));

    // The daemon now owns the tasks.
    let mut client = aether_daemon::Client::connect(&socket).unwrap();
    assert_eq!(client.list_tasks().unwrap().len(), 2);

    let mut client = aether_daemon::Client::connect(&socket).unwrap();
    client.shutdown().unwrap();
    fs::remove_dir_all(&root).ok();
}

#[test]
fn export_writes_a_round_trippable_document() {
    let root = temp_dir("export");
    let socket = spawn_daemon(&root);
    let mut client = aether_daemon::Client::connect(&socket).unwrap();
    client
        .add_with_settings("https://example.com/exported.bin", None, None, json!({}))
        .unwrap();

    let target = root.join("export.json");
    let cli = env!("CARGO_BIN_EXE_aether-cli");
    let output = Command::new(cli)
        .args([
            "export",
            target.to_string_lossy().as_ref(),
            "--socket",
            socket.to_string_lossy().as_ref(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let text = fs::read_to_string(&target).unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(document["format"], "aether-export/1");
    assert!(
        document["settings"].is_object(),
        "daemon settings are exported"
    );
    let tasks = document["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["url"], "https://example.com/exported.bin");
    assert!(
        tasks[0].get("settings").is_none(),
        "task credentials must not be exported"
    );

    // Re-import with a different URL so the daemon accepts it as a new task;
    // `--settings` applies the exported daemon settings.
    document["tasks"][0]["url"] = json!("https://example.com/exported-2.bin");
    fs::write(&target, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    let output = Command::new(cli)
        .args([
            "import",
            target.to_string_lossy().as_ref(),
            "--socket",
            socket.to_string_lossy().as_ref(),
            "--settings",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("applied daemon settings"), "{stdout}");
    assert!(stdout.contains("imported 1/1"), "{stdout}");

    let mut client = aether_daemon::Client::connect(&socket).unwrap();
    assert_eq!(client.list_tasks().unwrap().len(), 2);
    client.shutdown().unwrap();
    fs::remove_dir_all(&root).ok();
}

#[test]
fn import_reports_an_unreachable_daemon() {
    let root = temp_dir("nodemon");
    let list = root.join("urls.txt");
    fs::write(&list, "https://example.com/file.bin\n").unwrap();
    let socket = root.join("missing.sock");

    let cli = env!("CARGO_BIN_EXE_aether-cli");
    let output = Command::new(cli)
        .args([
            "import",
            list.to_string_lossy().as_ref(),
            "--socket",
            socket.to_string_lossy().as_ref(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemon is not reachable"),
        "stderr: {stderr}"
    );
    fs::remove_dir_all(&root).ok();
}
