#![cfg(unix)]
//! Daemon + fake plugin end-to-end test.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{Client, DaemonConfig};

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

#[test]
fn daemon_adds_lists_and_completes_a_task() {
    let Some(fake) = fake_binary() else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };

    let root = temp_dir("run");
    let package = root.join("plugins").join("fake");
    fs::create_dir_all(package.join("bin")).unwrap();
    let executable_name = format!("aether-plugin-fake{}", std::env::consts::EXE_SUFFIX);
    fs::copy(&fake, package.join("bin").join(&executable_name)).unwrap();
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
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
    };
    let daemon = thread::spawn(move || aether_daemon::run(config));

    let mut client = loop {
        match Client::connect(&socket) {
            Ok(client) => break client,
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    };
    client.ping().unwrap();
    let added = client
        .add("https://example.com/file.bin", None, None)
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();

    let mut completed = false;
    for _ in 0..100 {
        let tasks = client.list().unwrap();
        if let Some(task) = tasks["tasks"]
            .as_array()
            .and_then(|tasks| tasks.iter().find(|task| task["id"] == task_id))
        {
            if task["state"] == "completed" {
                completed = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(completed, "daemon task did not complete");

    let final_file = root
        .join("downloads")
        .join(".aether")
        .join(&task_id)
        .join("final")
        .join("fake.bin");
    assert_eq!(fs::read(final_file).unwrap().len(), 1024);

    client.shutdown().unwrap();
    let _ = daemon.join();
    fs::remove_dir_all(root).ok();
}
