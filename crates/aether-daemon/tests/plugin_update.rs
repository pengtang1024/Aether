//! Plugin update channel end-to-end (ROADMAP 4.6b): a local HTTP server
//! publishes a signed update manifest with plugin entries; the daemon
//! downloads, hash-verifies and installs them through the same verified
//! `install_package` path the install RPC uses.
//!
//! Cross-platform: the daemon endpoint helper mirrors tests/rpc.rs.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_daemon::{Client, DaemonConfig};
use aether_plugin_sdk::manifest::current_platform_key;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};

const SEED: [u8; 32] = [7_u8; 32];

fn temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-plugin-update-{label}-{}-{nanos}",
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
            r"\\.\pipe\aether-update-{label}-{}-{nanos}",
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

fn spawn(
    label: &str,
    root: PathBuf,
    update_url: Option<String>,
    public_key: Option<String>,
) -> (Daemon, Client) {
    let socket = endpoint(label, &root);
    let config = DaemonConfig {
        download_dir: root.join("downloads"),
        plugins_dir: root.join("plugins"),
        socket: socket.clone(),
        host_version: "0.2.0".into(),
        update_url,
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

type Routes = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Minimal HTTP file server. The route map is handed out before the
/// listener address is known, so callers can register manifest URLs that
/// embed the address itself.
fn serve(routes: Routes) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let routes = routes.clone();
            thread::spawn(move || handle_request(&mut stream, &routes));
        }
    });
    addr
}

fn handle_request(stream: &mut TcpStream, routes: &Routes) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let body = routes.lock().unwrap().get(path).cloned();
    let response = match body {
        Some(body) => {
            let mut response =
                format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
            response.extend(body);
            response
        }
        None => b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
    };
    let _ = stream.write_all(&response);
}

fn public_key() -> String {
    base64::engine::general_purpose::STANDARD
        .encode(SigningKey::from_bytes(&SEED).verifying_key().as_bytes())
}

/// Signed package dir (4.6a contract: files map + canonical signature).
fn signed_package(dir: &Path, id: &str, version: &str) -> PathBuf {
    let package = dir.join(format!("{id}-{version}"));
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("bin").join("plugin"),
        format!("binary {id} {version}"),
    )
    .unwrap();
    let mut manifest = json!({
        "id": id,
        "name": format!("Test plugin {id}"),
        "version": version,
        "api": 1,
        "protocols": ["test"],
        "capabilities": ["network"],
        "executable": { current_platform_key(): "bin/plugin" },
    });
    let hash =
        aether_plugin_sdk::checksum::sha256_file(&package.join("bin").join("plugin")).unwrap();
    manifest["files"] = json!({ "bin/plugin": hash });
    let canonical = aether_update::canonical_manifest_bytes(&manifest);
    let signature = SigningKey::from_bytes(&SEED).sign(&canonical);
    manifest["signature"] =
        Value::String(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()));
    fs::write(
        package.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    package
}

/// tar.gz the package contents (plugin.json at the archive root).
fn archive_package(package: &Path, out: &Path) {
    let file = fs::File::create(out).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (from, name) in [
        (package.join("plugin.json"), "plugin.json"),
        (package.join("bin").join("plugin"), "bin/plugin"),
    ] {
        let bytes = fs::read(&from).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

/// A single-entry tar.gz with a hostile path (the tar Builder refuses `..`,
/// so the ustar header is crafted by hand — the unpacker must defend
/// itself against whatever it is given).
fn hostile_archive(out: &Path) {
    let mut header = vec![0_u8; 512];
    header[..9].copy_from_slice(b"../escape");
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(b"00000000005\0");
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
    header[148..156].copy_from_slice(format!("{:06o}\0 ", checksum).as_bytes());
    let file = fs::File::create(out).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&header).unwrap();
    encoder.write_all(b"pwned").unwrap();
    encoder.write_all(&[0_u8; 507]).unwrap();
    encoder.write_all(&[0_u8; 1024]).unwrap();
    encoder.finish().unwrap();
}

/// update.json with `plugins` entries + its detached signature, registered
/// on the live server (URLs embed the server address itself).
fn publish_manifest(routes: &Routes, addr: SocketAddr, entries: Value) {
    let rewrite = |value: &mut Value| {
        for (_id, entry) in value["plugins"].as_object_mut().unwrap() {
            let file = entry["url"].as_str().unwrap().to_string();
            entry["url"] = Value::String(format!("http://{addr}/{file}"));
        }
    };
    let mut manifest = json!({
        "version": "0.2.0",
        "channel": "stable",
        "assets": {},
        "plugins": entries,
    });
    rewrite(&mut manifest);
    let raw = serde_json::to_vec_pretty(&manifest).unwrap();
    let signature = base64::engine::general_purpose::STANDARD
        .encode(SigningKey::from_bytes(&SEED).sign(&raw).to_bytes());
    let mut routes = routes.lock().unwrap();
    routes.insert("/update.json".to_string(), raw);
    routes.insert("/update.json.sig".to_string(), signature.into_bytes());
}

fn sha256_of(path: &Path) -> String {
    aether_plugin_sdk::checksum::sha256_file(path).unwrap()
}

#[test]
fn unconfigured_channel_reports_honestly_and_setting_roundtrips() {
    let root = temp_root("unconfigured");
    let (daemon, mut client) = spawn("unconfigured", root, None, None);

    let value = client.plugin_updates().unwrap();
    assert_eq!(value["configured"], json!(false), "{value}");

    // The auto-update switch is a real daemon setting.
    let updated = client
        .set_settings(json!({"plugin_auto_update": true}))
        .unwrap();
    assert_eq!(updated["plugin_auto_update"], json!(true));
    assert_eq!(
        client.settings().unwrap()["plugin_auto_update"],
        json!(true)
    );
    client
        .set_settings(json!({"plugin_auto_update": false}))
        .unwrap();
    assert_eq!(
        client.settings().unwrap()["plugin_auto_update"],
        json!(false)
    );

    // Without the channel configured, update-all refuses instead of
    // pretending it did something.
    let error = client.plugin_update_all().expect_err("no channel");
    assert!(error.to_string().contains("update_url"), "{error}");

    client.shutdown().unwrap();
    drop(daemon);
}

#[test]
fn update_check_reports_outdated_and_missing_plugins() {
    let root = temp_root("check");
    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let addr = serve(routes.clone());

    // Archives and manifest reference the live server address.
    let scratch = temp_root("check-assets");
    let v2 = signed_package(&scratch, "demo", "2.0.0");
    let extra = signed_package(&scratch, "extra", "1.0.0");
    archive_package(&v2, &scratch.join("demo-2.tar.gz"));
    archive_package(&extra, &scratch.join("extra-1.tar.gz"));
    routes.lock().unwrap().insert(
        "/demo-2.tar.gz".to_string(),
        fs::read(scratch.join("demo-2.tar.gz")).unwrap(),
    );
    routes.lock().unwrap().insert(
        "/extra-1.tar.gz".to_string(),
        fs::read(scratch.join("extra-1.tar.gz")).unwrap(),
    );
    publish_manifest(
        &routes,
        addr,
        json!({
            "demo": { "version": "2.0.0", "url": "demo-2.tar.gz", "sha256": sha256_of(&scratch.join("demo-2.tar.gz")) },
            "extra": { "version": "1.0.0", "url": "extra-1.tar.gz", "sha256": sha256_of(&scratch.join("extra-1.tar.gz")) },
        }),
    );

    let (daemon, mut client) = spawn(
        "check",
        root,
        Some(format!("http://{addr}/update.json")),
        Some(public_key()),
    );
    // Installed baseline: demo@1.0.0.
    let v1 = signed_package(&daemon.root, "demo", "1.0.0");
    client.install_plugin(v1.to_str().unwrap()).unwrap();

    let value = client.plugin_updates().unwrap();
    assert_eq!(value["configured"], json!(true));
    let list = value["updates"].as_array().unwrap();
    let demo = list
        .iter()
        .find(|item| item["plugin_id"] == "demo")
        .expect("demo listed as outdated");
    assert_eq!(demo["installed"], json!("1.0.0"));
    assert_eq!(demo["latest"], json!("2.0.0"));
    let extra = list
        .iter()
        .find(|item| item["plugin_id"] == "extra")
        .expect("extra listed as missing");
    assert_eq!(extra["installed"], Value::Null);

    client.shutdown().unwrap();
    drop(daemon);
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn update_all_installs_and_rejects_hostile_or_corrupt_archives() {
    let root = temp_root("update-all");
    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let addr = serve(routes.clone());

    let scratch = temp_root("update-all-assets");
    let v2 = signed_package(&scratch, "demo", "2.0.0");
    let fresh = signed_package(&scratch, "fresh", "3.0.0");
    archive_package(&v2, &scratch.join("demo-2.tar.gz"));
    archive_package(&fresh, &scratch.join("fresh-3.tar.gz"));
    hostile_archive(&scratch.join("evil.tar.gz"));
    fs::write(scratch.join("corrupt.tar.gz"), b"pretending to be gzip").unwrap();
    for file in [
        "demo-2.tar.gz",
        "fresh-3.tar.gz",
        "evil.tar.gz",
        "corrupt.tar.gz",
    ] {
        routes
            .lock()
            .unwrap()
            .insert(format!("/{file}"), fs::read(scratch.join(file)).unwrap());
    }
    publish_manifest(
        &routes,
        addr,
        json!({
            // Up to date already: must be skipped, not "updated".
            "stable": { "version": "1.0.0", "url": "demo-2.tar.gz", "sha256": sha256_of(&scratch.join("demo-2.tar.gz")) },
            "demo": { "version": "2.0.0", "url": "demo-2.tar.gz", "sha256": sha256_of(&scratch.join("demo-2.tar.gz")) },
            "fresh": { "version": "3.0.0", "url": "fresh-3.tar.gz", "sha256": sha256_of(&scratch.join("fresh-3.tar.gz")) },
            "evil": { "version": "1.0.0", "url": "evil.tar.gz", "sha256": sha256_of(&scratch.join("evil.tar.gz")) },
            "corrupt": { "version": "1.0.0", "url": "corrupt.tar.gz", "sha256": "0".repeat(64) },
        }),
    );

    let (daemon, mut client) = spawn(
        "update-all",
        root,
        Some(format!("http://{addr}/update.json")),
        Some(public_key()),
    );
    // Baseline installs: demo@1.0.0 (will be updated) and stable@1.0.0
    // (already matches the manifest; its archive is deliberately wrong for
    // it, proving the skip happens before any download).
    client
        .install_plugin(
            signed_package(&daemon.root, "demo", "1.0.0")
                .to_str()
                .unwrap(),
        )
        .unwrap();
    client
        .install_plugin(
            signed_package(&daemon.root, "stable", "1.0.0")
                .to_str()
                .unwrap(),
        )
        .unwrap();

    let value = client.plugin_update_all().unwrap();
    let updated = value["updated"].as_array().unwrap();
    let failed = value["failed"].as_array().unwrap();
    assert_eq!(value["up_to_date"], json!(false));

    // demo and fresh installed/updated; stable untouched (skipped).
    assert!(updated.contains(&json!("demo")), "{value}");
    assert!(updated.contains(&json!("fresh")), "{value}");
    assert!(!updated.contains(&json!("stable")), "{value}");

    // Hostile and corrupt entries fail independently, with reasons.
    assert_eq!(failed.len(), 2, "{value}");
    let evil = failed
        .iter()
        .find(|item| item["plugin_id"] == "evil")
        .expect("evil reported");
    assert!(
        evil["error"].as_str().unwrap().contains("escapes"),
        "{evil}"
    );
    let corrupt = failed
        .iter()
        .find(|item| item["plugin_id"] == "corrupt")
        .expect("corrupt reported");
    assert!(
        corrupt["error"].as_str().unwrap().contains("download"),
        "{corrupt}"
    );

    // Registry state: the new versions are live, the hostile ones were not.
    let plugins = serde_json::to_value(client.plugins().unwrap()).unwrap();
    let version_of = |id: &str| {
        plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["id"] == id)
            .map(|plugin| plugin["version"].clone())
    };
    assert_eq!(version_of("demo"), Some(json!("2.0.0")));
    assert_eq!(version_of("fresh"), Some(json!("3.0.0")));
    assert_eq!(version_of("stable"), Some(json!("1.0.0")));
    assert_eq!(version_of("evil"), None);
    assert_eq!(version_of("corrupt"), None);
    assert!(
        !daemon.root.join("plugins/evil").exists(),
        "hostile package must not land in plugins_dir"
    );

    client.shutdown().unwrap();
    drop(daemon);
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn auto_update_pass_runs_only_when_the_setting_is_on() {
    let root = temp_root("auto");
    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let addr = serve(routes.clone());

    let scratch = temp_root("auto-assets");
    let pkg = signed_package(&scratch, "autoed", "5.0.0");
    archive_package(&pkg, &scratch.join("autoed-5.tar.gz"));
    routes.lock().unwrap().insert(
        "/autoed-5.tar.gz".to_string(),
        fs::read(scratch.join("autoed-5.tar.gz")).unwrap(),
    );
    publish_manifest(
        &routes,
        addr,
        json!({
            "autoed": { "version": "5.0.0", "url": "autoed-5.tar.gz", "sha256": sha256_of(&scratch.join("autoed-5.tar.gz")) },
        }),
    );

    let (daemon, mut client) = spawn(
        "auto",
        root,
        Some(format!("http://{addr}/update.json")),
        Some(public_key()),
    );

    // Off (default): the pass does nothing.
    let result = aether_daemon::server::plugin_auto_update_pass(&daemon.socket).unwrap();
    assert_eq!(result, None, "pass must stay idle while the setting is off");

    // On: the periodic pass installs the outdated plugin as a client of
    // the daemon — exactly what the 6-hourly update loop does.
    client
        .set_settings(json!({"plugin_auto_update": true}))
        .unwrap();
    let result = aether_daemon::server::plugin_auto_update_pass(&daemon.socket)
        .unwrap()
        .expect("pass must report work");
    assert!(
        result["updated"]
            .as_array()
            .unwrap()
            .contains(&json!("autoed")),
        "{result}"
    );
    let plugins = serde_json::to_value(client.plugins().unwrap()).unwrap();
    assert!(plugins
        .as_array()
        .unwrap()
        .iter()
        .any(|plugin| plugin["id"] == "autoed" && plugin["version"] == json!("5.0.0")));

    client.shutdown().unwrap();
    drop(daemon);
    fs::remove_dir_all(&scratch).ok();
}
