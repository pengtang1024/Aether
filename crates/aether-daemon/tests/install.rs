//! `install_plugin` / `rescan_plugins` end-to-end (ROADMAP 4.6a).
//!
//! Cross-platform: the daemon endpoint helper mirrors tests/rpc.rs (socket
//! file on Unix, pipe name on Windows). Signatures are built in-process with
//! the same canonical API the daemon verifies against — the signing *tool*
//! has its own roundtrip coverage in aether-update/tests/plugin_sign.rs.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{Client, DaemonConfig};
use aether_plugin_sdk::manifest::current_platform_key;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};

fn temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-install-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("plugins")).unwrap();
    dir
}

/// The daemon's local endpoint: socket file on Unix, unique pipe on Windows.
fn endpoint(label: &str, root: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        let _ = label;
        root.join("daemon.sock")
    }
    #[cfg(windows)]
    {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let _ = root;
        PathBuf::from(format!(
            r"\\.\pipe\aether-install-{label}-{}-{nanos}",
            std::process::id()
        ))
    }
}

struct Daemon {
    root: PathBuf,
    socket: PathBuf,
    handle: Option<thread::JoinHandle<Result<(), aether_daemon::DaemonError>>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(mut client) = Client::connect(&self.socket) {
            let _ = client.shutdown();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        fs::remove_dir_all(&self.root).ok();
    }
}

fn spawn(label: &str, public_key: Option<String>) -> (Daemon, Client) {
    let root = temp_root(label);
    let socket = endpoint(label, &root);
    let config = DaemonConfig {
        download_dir: root.join("downloads"),
        plugins_dir: root.join("plugins"),
        socket: socket.clone(),
        host_version: "0.2.0".into(),
        update_url: None,
        update_public_key: public_key,
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
    let daemon = Daemon {
        root,
        socket: socket.clone(),
        handle: Some(handle),
    };
    let mut client = None;
    for _ in 0..100 {
        if let Ok(connected) = Client::connect(&socket) {
            client = Some(connected);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    (daemon, client.expect("daemon never became reachable"))
}

/// A minimal installable package: manifest for the current platform + a
/// dummy executable. The "binary" only needs to exist — install verifies
/// hashes, not whether it runs (running it is the supervisor's job).
fn write_package(dir: &Path, id: &str) -> PathBuf {
    let package = dir.join(id);
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(package.join("bin").join("plugin"), b"fake plugin binary").unwrap();
    let manifest = json!({
        "id": id,
        "name": format!("Test plugin {id}"),
        "version": "1.0.0",
        "api": 1,
        "protocols": ["test"],
        "capabilities": ["network"],
        "executable": { current_platform_key(): "bin/plugin" },
    });
    fs::write(
        package.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    package
}

/// In-process stand-in for `aether-plugin-sign`: files map + canonical
/// bytes + Ed25519, exactly the contract the daemon verifies.
fn sign_package(package: &Path, seed: [u8; 32]) {
    let manifest_path = package.join("plugin.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let hash =
        aether_plugin_sdk::checksum::sha256_file(&package.join("bin").join("plugin")).unwrap();
    manifest["files"] = json!({ "bin/plugin": hash });
    manifest.as_object_mut().unwrap().remove("signature");
    let canonical = aether_update::canonical_manifest_bytes(&manifest);
    let signature = SigningKey::from_bytes(&seed).sign(&canonical);
    manifest["signature"] =
        Value::String(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()));
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn pubkey(seed: [u8; 32]) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(SigningKey::from_bytes(&seed).verifying_key().as_bytes())
}

/// Find a plugin's enabled flag in the `plugins()` array.
fn contains(plugins: &Value, id: &str) -> Option<bool> {
    plugins
        .as_array()?
        .iter()
        .find(|plugin| plugin["id"] == id)
        .map(|plugin| plugin["enabled"].as_bool().unwrap_or(false))
}

#[test]
fn unsigned_install_needs_the_explicit_dev_opt_in() {
    let (daemon, mut client) = spawn("unsigned", None);
    let package = write_package(&daemon.root, "demo-unsigned");

    let error = client
        .install_plugin(package.to_str().unwrap())
        .expect_err("unsigned install without a key must fail");
    assert!(
        error.to_string().contains("AETHER_ALLOW_UNSIGNED_PLUGINS"),
        "{error}"
    );

    // Dev escape hatch: requested per install, so the daemon-side check is
    // what the test exercises (env is read at request time).
    std::env::set_var("AETHER_ALLOW_UNSIGNED_PLUGINS", "1");
    let installed = client
        .install_plugin(package.to_str().unwrap())
        .expect("unsigned install with the dev flag");
    std::env::remove_var("AETHER_ALLOW_UNSIGNED_PLUGINS");
    assert_eq!(installed["plugin_id"], "demo-unsigned");
    assert_eq!(installed["replaced"], false);

    // The package really landed in plugins_dir and the registry sees it.
    assert!(daemon
        .root
        .join("plugins/demo-unsigned/plugin.json")
        .is_file());
    let plugins = serde_json::to_value(client.plugins().unwrap()).unwrap();
    assert_eq!(contains(&plugins, "demo-unsigned"), Some(true));

    client.shutdown().unwrap();
}

#[test]
fn signed_install_verifies_signature_then_file_hashes() {
    let (daemon, mut client) = spawn("signed", Some(pubkey([7; 32])));
    let package = write_package(&daemon.root, "demo-signed");

    // Unsigned package against a keyed daemon: refused before any copy.
    let error = client
        .install_plugin(package.to_str().unwrap())
        .expect_err("unsigned package against a keyed daemon");
    assert!(error.to_string().contains("signature"), "{error}");
    assert!(!daemon.root.join("plugins/demo-signed").exists());

    // Signed with a *different* key: refused.
    sign_package(&package, [9; 32]);
    let error = client
        .install_plugin(package.to_str().unwrap())
        .expect_err("wrong signing key");
    assert!(error.to_string().contains("signature"), "{error}");

    // Correct key: installs.
    sign_package(&package, [7; 32]);
    let installed = client
        .install_plugin(package.to_str().unwrap())
        .expect("valid signed package");
    assert_eq!(installed["plugin_id"], "demo-signed");

    // Tampered binary after signing: manifest signature still holds, but
    // the files map catches the swap.
    fs::write(package.join("bin").join("plugin"), b"evil bytes").unwrap();
    let error = client
        .install_plugin(package.to_str().unwrap())
        .expect_err("tampered package");
    assert!(error.to_string().contains("integrity"), "{error}");

    // Re-installing over the installed copy replaces it (no running tasks).
    sign_package(&package, [7; 32]);
    let again = client
        .install_plugin(package.to_str().unwrap())
        .expect("reinstall");
    assert_eq!(again["replaced"], true);

    client.shutdown().unwrap();
}

#[test]
fn rescan_registers_dropped_packages_and_keeps_disabled_state() {
    let (daemon, mut client) = spawn("rescan", None);

    // Simulate a dev dropping a package straight into plugins_dir.
    let dropped = daemon.root.join("plugins");
    let _alpha = write_package(&dropped, "alpha");
    let _beta = write_package(&dropped, "beta");

    let value = client.rescan_plugins().unwrap();
    assert_eq!(value["plugins"], 2, "{value}");

    // Disable alpha, drop nothing new, rescan again: the persisted disable
    // list must survive the registry swap.
    client.set_plugin_enabled("alpha", false).unwrap();
    let plugins = serde_json::to_value(client.plugins().unwrap()).unwrap();
    assert_eq!(contains(&plugins, "alpha"), Some(false));

    client.rescan_plugins().unwrap();
    let plugins = serde_json::to_value(client.plugins().unwrap()).unwrap();
    assert_eq!(
        contains(&plugins, "alpha"),
        Some(false),
        "rescan must re-apply the disabled list"
    );
    assert_eq!(contains(&plugins, "beta"), Some(true));

    client.shutdown().unwrap();
}
