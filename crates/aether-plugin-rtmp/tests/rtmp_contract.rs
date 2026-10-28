#![cfg(unix)]
//! RTMP plugin test with a fake ffmpeg.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::{sha256_bytes, sha256_file};
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};

const FAKE_FFMPEG: &str = r#"#!/bin/sh
if [ "$1" = "-version" ]; then
  echo "ffmpeg version fake"
  exit 0
fi
out=""
for arg in "$@"; do out="$arg"; done
dd if=/dev/zero of="$out" bs=1024 count=2 2>/dev/null
echo "frame= 10 time=00:00:01.00 bitrate=1.2" >&2
exit 0
"#;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-rtmp-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "rtmp",
        "name": "RTMP plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["rtmp", "rtmps", "rtsp"],
        "capabilities": ["network", "filesystem:task", "external-process"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-rtmp"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn captures_with_fake_ffmpeg() {
    let root = temp_dir("capture");
    let fake = root.join("fake-ffmpeg");
    fs::write(&fake, FAKE_FFMPEG).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("AETHER_FFMPEG", &fake);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe("rtmp://example.com/live").unwrap();
    assert!(probe.supported);

    let staging = root.join("staging");
    let expected = vec![0_u8; 2048];
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url: "rtmp://example.com/live".into(),
                filename: None,
                settings: serde_json::json!({ "ffmpeg_path": fake }),
                plugin_state: None,
                expected_size: None,
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: sha256_bytes(&expected),
                }),
                proxy: None,
            },
            TaskPaths {
                staging: staging.clone(),
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget::default(),
        )
        .unwrap();

    let mut completed = false;
    for _ in 0..50 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => {
                completed = true;
                break;
            }
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            _ => {}
        }
    }
    assert!(completed);
    let output = staging.join("stream.flv");
    assert_eq!(fs::read(&output).unwrap().len(), 2048);
    assert_eq!(sha256_file(&output).unwrap(), sha256_bytes(&expected));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
