//! Per-plugin configuration: validation against `settings_schema`, persistence
//! across daemon restarts, and rejection of unknown plugins/keys.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{Client, DaemonConfig};
use aether_plugin_sdk::manifest::current_platform_key;
use serde_json::json;

fn temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-plugin-settings-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("plugins")).unwrap();
    dir
}

fn write_plugin(plugins: &Path, id: &str) {
    let package = plugins.join(id);
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(package.join("bin").join("plugin"), b"fake plugin binary").unwrap();
    let manifest = json!({
        "id": id,
        "name": format!("Test plugin {id}"),
        "version": "1.0.0",
        "api": 2,
        "protocols": ["test"],
        "executable": { current_platform_key(): "bin/plugin" },
        "settings_schema": {
            "sections": [{
                "title": "Test",
                "fields": [
                    {"key": "user_agent", "type": "string"},
                    {"key": "retries", "type": "int", "min": 0, "max": 5}
                ]
            }]
        },
    });
    fs::write(
        package.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn spawn(
    root: &Path,
) -> (
    thread::JoinHandle<Result<(), aether_daemon::DaemonError>>,
    Client,
) {
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
        retry_base_secs: 1,
        proxy: None,
        pac_url: None,
        rpc_listen: None,
    };
    let handle = thread::spawn(move || aether_daemon::run(config));
    let mut client = None;
    for _ in 0..100 {
        if let Ok(connected) = Client::connect(&socket) {
            client = Some(connected);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    (handle, client.expect("daemon never became reachable"))
}

fn stop(handle: thread::JoinHandle<Result<(), aether_daemon::DaemonError>>, root: &Path) {
    if let Ok(mut client) = Client::connect(&root.join("daemon.sock")) {
        let _ = client.shutdown();
    }
    let _ = handle.join();
}

#[test]
fn validates_persists_and_rejects_bad_plugin_settings() {
    let root = temp_root("validate");
    write_plugin(&root.join("plugins"), "http");
    let (handle, mut client) = spawn(&root);

    assert_eq!(client.plugin_settings().unwrap(), json!({}));

    let stored = client
        .set_plugin_settings("http", json!({"user_agent": "aether-test"}))
        .unwrap();
    assert_eq!(stored, json!({"user_agent": "aether-test"}));
    assert_eq!(
        client.plugin_settings().unwrap(),
        json!({"http": {"user_agent": "aether-test"}})
    );

    let unknown_key = client
        .set_plugin_settings("http", json!({"bogus": 1}))
        .unwrap_err()
        .to_string();
    assert!(
        unknown_key.contains("unknown setting bogus"),
        "{unknown_key}"
    );
    let wrong_type = client
        .set_plugin_settings("http", json!({"retries": "3"}))
        .unwrap_err()
        .to_string();
    assert!(wrong_type.contains("must be an integer"), "{wrong_type}");
    let out_of_range = client
        .set_plugin_settings("http", json!({"retries": 9}))
        .unwrap_err()
        .to_string();
    assert!(out_of_range.contains("above 5"), "{out_of_range}");
    assert!(client.set_plugin_settings("missing", json!({})).is_err());

    stop(handle, &root);
    // Stored configuration survives a restart.
    let (handle, mut client) = spawn(&root);
    assert_eq!(
        client.plugin_settings().unwrap(),
        json!({"http": {"user_agent": "aether-test"}})
    );
    stop(handle, &root);
    fs::remove_dir_all(root).ok();
}
