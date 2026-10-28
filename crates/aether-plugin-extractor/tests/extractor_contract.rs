#![cfg(unix)]
//! Extractor plugin test with a fake yt-dlp executable.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};

const FAKE_YTDLP: &str = r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "-J" ]; then
    printf '{"title":"Test Clip","ext":"mp4","filesize":4096}\n'
    exit 0
  fi
done
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then
    out="$arg"
  fi
  prev="$arg"
done
out=$(printf '%s' "$out" | sed 's/%(title)s/Test Clip/; s/%(ext)s/mp4/')
mkdir -p "$(dirname "$out")"
dd if=/dev/zero of="$out" bs=1024 count=4 2>/dev/null
printf '[download] 100.0%% of 4.00KiB at 1.00MiB/s ETA 00:00\n'
"#;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-extractor-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "extractor",
        "name": "Extractor plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["extractor"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-extractor"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn probes_and_downloads_with_fake_ytdlp() {
    let root = temp_dir("run");
    let fake = root.join("fake-ytdlp");
    fs::write(&fake, FAKE_YTDLP).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("AETHER_YTDLP", &fake);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe("https://example.com/video").unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("Test Clip.mp4"));

    let staging = root.join("staging");
    let expected = vec![0_u8; 4096];
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url: "https://example.com/video".into(),
                filename: None,
                settings: serde_json::json!({ "yt_dlp_path": fake }),
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
    assert_eq!(fs::read(staging.join("Test Clip.mp4")).unwrap().len(), 4096);

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
