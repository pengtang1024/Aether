//! Contract tests: real process, real stdio, real MessagePack frames.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::{
    current_platform_key, Budget, Init, LogLevel, PluginEvent, PluginManifest, PluginProcess,
    TaskPaths, TaskSpec, API_VERSION,
};

const PLUGIN_ID: &str = "fake";
const PLUGIN_VERSION: &str = "0.1.0";

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-plugin-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest(id: &str) -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": id,
        "name": "Fake plugin",
        "version": PLUGIN_VERSION,
        "api": API_VERSION,
        "protocols": ["http", "https"],
        "capabilities": ["network", "filesystem:task"],
        "executable": {}
    }))
    .unwrap();
    manifest
        .executable
        .insert(current_platform_key().into(), "unused".into());
    manifest
}

fn init(data_dir: &Path) -> Init {
    Init {
        network: None,
        host_version: "0.2.0".into(),
        data_dir: data_dir.to_string_lossy().into_owned(),
        log_level: LogLevel::Debug,
        proxy: None,
        limits: Default::default(),
    }
}

fn executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-fake"))
}

#[test]
fn fake_plugin_handshake_probe_and_task() {
    let root = temp_dir("task");
    let mut plugin = PluginProcess::spawn(manifest(PLUGIN_ID), &executable(), init(&root)).unwrap();

    let probe = plugin.probe("https://example.com/file.bin").unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("fake.bin"));

    // Events that arrive while waiting for ProbeResult are queued, not lost.
    match plugin.recv_event(Duration::from_secs(1)).unwrap() {
        PluginEvent::Log { message, .. } => assert!(message.starts_with("probe ")),
        other => panic!("expected queued log event, got {other:?}"),
    }

    let rejected = plugin.probe("ftp://example.com/file.bin").unwrap();
    assert!(!rejected.supported);

    let staging = root.join("staging");
    let task = TaskSpec {
        task_id: "task-1".into(),
        url: "https://example.com/file.bin".into(),
        filename: None,
        settings: serde_json::json!({}),
        plugin_state: None,
        expected_size: Some(1024),
        checksum: None,
        proxy: None,
    };
    plugin
        .start_task(
            task,
            TaskPaths {
                staging: staging.clone(),
                final_dir: root.join("final"),
            },
            Budget::default(),
        )
        .unwrap();

    let mut completed = false;
    for _ in 0..50 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Progress { .. } | PluginEvent::Metadata { .. } => continue,
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => {
                completed = true;
                break;
            }
            _ => continue,
        }
    }

    assert!(completed, "plugin did not report completion");
    let bytes = fs::read(staging.join("fake.bin")).unwrap();
    assert_eq!(bytes.len(), 1024);
    assert!(bytes.iter().all(|byte| *byte == 0xA5));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn rejects_plugin_id_mismatch() {
    let root = temp_dir("mismatch");
    let result = PluginProcess::spawn(manifest("other"), &executable(), init(&root));

    match result {
        Ok(_) => panic!("supervisor accepted a plugin with the wrong id"),
        Err(error) => assert!(
            error.to_string().contains("plugin id mismatch"),
            "unexpected error: {error}"
        ),
    }

    fs::remove_dir_all(root).ok();
}
