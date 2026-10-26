//! End-to-end Host runtime test using the fake plugin binary.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use aether_host::{finalize_task, PluginManager, PluginRegistry};
use aether_plugin_sdk::{current_platform_key, Budget, PluginEvent, TaskPaths, TaskSpec};

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aether-host-{label}-{}", std::process::id()));
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
fn discovers_probes_downloads_and_finalizes() {
    let Some(fake) = fake_binary() else {
        eprintln!("fake plugin binary is not built; skipping");
        return;
    };

    let root = temp_dir("runtime");
    let package = root.join("plugins").join("fake");
    fs::create_dir_all(package.join("bin")).unwrap();
    let executable_name = format!("aether-plugin-fake{}", std::env::consts::EXE_SUFFIX);
    fs::copy(&fake, package.join("bin").join(&executable_name)).unwrap();
    let manifest = serde_json::json!({
        "id": "fake",
        "name": "Fake plugin",
        "version": "0.1.0",
        "api": aether_plugin_sdk::API_VERSION,
        "protocols": ["http"],
        "capabilities": ["network", "filesystem:task"],
        "executable": {
            current_platform_key(): format!("bin/{executable_name}")
        }
    });
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let registry = PluginRegistry::discover(&[root.join("plugins")]).unwrap();
    assert_eq!(registry.len(), 1);
    let mut manager = PluginManager::new(registry, root.join("data"), "0.2.0");
    let outcomes = manager.probe("https://example.com/file.bin");
    let probe = outcomes
        .iter()
        .find_map(|outcome| outcome.result.as_ref())
        .expect("fake probe");
    assert!(probe.supported);

    let staging = root.join("task").join("staging");
    let final_dir = root.join("task").join("final");
    fs::create_dir_all(&staging).unwrap();
    manager
        .start(
            "fake",
            TaskSpec {
                task_id: "task-1".into(),
                url: "https://example.com/file.bin".into(),
                filename: None,
                settings: serde_json::json!({}),
                plugin_state: None,
                expected_size: Some(1024),
                checksum: None,
            },
            TaskPaths {
                staging: staging.clone(),
                final_dir: final_dir.clone(),
            },
            Budget::default(),
        )
        .unwrap();

    let mut completed = None;
    for _ in 0..100 {
        let Some(envelope) = manager.recv_event(Duration::from_secs(1)).unwrap() else {
            continue;
        };
        if let PluginEvent::Completed { files, .. } = envelope.event {
            completed = Some(files);
            break;
        }
    }
    let files = completed.expect("fake plugin completion");
    let moved = finalize_task(&staging, &final_dir, &files).unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(fs::read(&moved[0]).unwrap().len(), 1024);

    manager.shutdown();
    fs::remove_dir_all(root).ok();
}

#[test]
fn disabled_plugins_are_not_probed() {
    let root = temp_dir("disabled");
    let package = root.join("plugins").join("empty");
    fs::create_dir_all(&package).unwrap();
    let manifest = serde_json::json!({
        "id": "empty",
        "name": "Empty",
        "version": "0.1.0",
        "api": aether_plugin_sdk::API_VERSION,
        "protocols": ["empty"],
        "executable": { current_platform_key(): "bin/none" }
    });
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let mut registry = PluginRegistry::discover(&[root.join("plugins")]).unwrap();
    registry.set_enabled("empty", false).unwrap();
    let mut manager = PluginManager::new(registry, root.join("data"), "0.2.0");
    assert!(manager.probe("anything").is_empty());
    manager.shutdown();
    fs::remove_dir_all(root).ok();
}
