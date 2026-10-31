//! aria2 JSON-RPC compatibility over the daemon's shared TCP port.
//!
//! End-to-end: HTTP request -> daemon token check -> service plugin IPC ->
//! plugin's native daemon client -> fake downloader. Also proves the native
//! line protocol still works on the same port.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_daemon::settings::Settings;
use aether_daemon::{Client, DaemonConfig};
use base64::Engine;
use serde_json::{json, Value};

const TOKEN: &str = "s3cret";
const ARIA2: &str = "aether-plugin-aria2";
const FAKE: &str = "aether-plugin-fake";

struct Daemon {
    root: PathBuf,
    socket: PathBuf,
    addr: SocketAddr,
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
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-aria2-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn daemon_endpoint(root: &Path) -> PathBuf {
    #[cfg(unix)]
    {
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
            r"\\.\pipe\aether-aria2-{}-{nanos}",
            std::process::id()
        ))
    }
}

fn debug_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let binary = debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    binary.is_file().then_some(binary)
}

fn stage_package(root: &Path, id: &str, binary: &Path, mut manifest: Value) {
    let name = binary.file_name().unwrap().to_string_lossy().into_owned();
    manifest["executable"] = json!({
        aether_plugin_sdk::current_platform_key(): format!("bin/{name}")
    });
    let package = root.join("plugins").join(id);
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::copy(binary, package.join("bin").join(name)).unwrap();
    std::fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn fake_manifest(id: &str) -> Value {
    json!({
        "id": id,
        "name": format!("Fake {id} plugin"),
        "version": "0.1.0",
        "api": aether_plugin_sdk::API_VERSION,
        "protocols": ["http"],
        "capabilities": ["network", "filesystem:task"],
    })
}

fn aria2_manifest() -> Value {
    package_manifest("aether-plugin-aria2")
}

fn package_manifest(crate_name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_name)
        .join("plugin.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn spawn() -> Option<Daemon> {
    spawn_with_downloaders(false)
}

fn spawn_with_downloaders(with_bt_http: bool) -> Option<Daemon> {
    let fake = debug_binary(FAKE)?;
    let aria2 = debug_binary(ARIA2)?;
    let root = temp_dir("e2e");
    stage_package(&root, "fake", &fake, fake_manifest("fake"));
    stage_package(&root, "aria2", &aria2, aria2_manifest());
    if with_bt_http {
        let bt = debug_binary("aether-plugin-bt")?;
        let http = debug_binary("aether-plugin-http")?;
        stage_package(&root, "bt", &bt, package_manifest("aether-plugin-bt"));
        stage_package(&root, "http", &http, package_manifest("aether-plugin-http"));
    }

    let download_dir = root.join("downloads");
    let socket = daemon_endpoint(&root);
    let addr = free_addr();
    let settings_path = Settings::path(&download_dir);
    let mut settings = Settings::load(&settings_path);
    settings.rpc_listen = Some(addr.to_string());
    settings.rpc_secret = Some(TOKEN.into());
    settings.save(&settings_path).unwrap();

    let config = DaemonConfig {
        download_dir,
        plugins_dir: root.join("plugins"),
        socket: socket.clone(),
        host_version: "0.2.2".into(),
        update_url: None,
        update_public_key: None,
        update_appimage: None,
        max_concurrent: 1,
        max_speed_bps: None,
        retry_max: 0,
        retry_base_secs: 1,
        proxy: None,
        pac_url: None,
        rpc_listen: Some(addr.to_string()),
    };
    let handle = thread::spawn(move || aether_daemon::run(config));
    wait_for_port(addr);
    Some(Daemon {
        root,
        socket,
        addr,
        handle: Some(handle),
    })
}

fn wait_for_port(addr: SocketAddr) {
    for _ in 0..100 {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("rpc port {addr} never came up");
}

fn http(addr: SocketAddr, body: &Value) -> (String, Value) {
    let raw = serde_json::to_string(body).unwrap();
    let request = format!(
        "POST /jsonrpc HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
        raw.len()
    );
    exchange(addr, &request)
}

fn exchange(addr: SocketAddr, request: &str) -> (String, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let text = String::from_utf8_lossy(&response).into_owned();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let json = serde_json::from_str(body).unwrap_or(Value::Null);
    (text, json)
}

fn rpc(addr: SocketAddr, id: u64, method: &str, params: Value) -> Value {
    let (_, body) = http(
        addr,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    );
    body
}

fn native_lines(addr: SocketAddr, requests: &[Value]) -> Vec<Value> {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut raw = String::new();
    for request in requests {
        raw.push_str(&serde_json::to_string(request).unwrap());
        raw.push('\n');
    }
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[test]
fn http_and_native_protocols_share_the_port() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;

    // CORS preflight for browser front-ends.
    let (headers, _) = exchange(
        addr,
        "OPTIONS /jsonrpc HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(headers.starts_with("HTTP/1.1 204"), "{headers}");
    assert!(
        headers.contains("Access-Control-Allow-Origin: *"),
        "{headers}"
    );

    // Token gate.
    let unauthorized = rpc(addr, 1, "aria2.getVersion", json!(["token:wrong"]));
    assert!(unauthorized["error"].is_object(), "{unauthorized}");

    let version = rpc(addr, 2, "aria2.getVersion", json!(["token:s3cret"]));
    assert_eq!(version["result"]["version"], "1.37.0", "{version}");

    // Add through the fake downloader, then query the GID.
    let added = rpc(
        addr,
        3,
        "aria2.addUri",
        json!(["token:s3cret", ["http://example.test/fake.bin"]]),
    );
    let gid = added["result"].as_str().unwrap_or_default().to_string();
    assert_eq!(gid.len(), 16, "unexpected gid: {added}");

    let status = rpc(addr, 4, "aria2.tellStatus", json!(["token:s3cret", gid]));
    assert_eq!(status["result"]["gid"], gid, "{status}");
    assert!(status["result"]["status"].is_string(), "{status}");

    let stat = rpc(addr, 5, "aria2.getGlobalStat", json!(["token:s3cret"]));
    assert!(stat["result"]["numActive"].is_string(), "{stat}");

    // Per-task limit: stored in settings and pushed to a live plugin.
    let changed = rpc(
        addr,
        9,
        "aria2.changeOption",
        json!(["token:s3cret", gid, { "max-download-limit": "1K" }]),
    );
    assert_eq!(changed["result"], "OK", "{changed}");
    let option = rpc(addr, 10, "aria2.getOption", json!(["token:s3cret", gid]));
    assert_eq!(option["result"]["max-download-limit"], "1024", "{option}");
    let servers = rpc(addr, 11, "aria2.getServers", json!(["token:s3cret", gid]));
    assert!(
        servers["result"][0]["servers"][0]["uri"].is_string(),
        "{servers}"
    );
    // The fake downloader has no peers; the daemon short-circuits non-BT
    // tasks instead of waiting on a plugin round-trip.
    let peers = rpc(addr, 12, "aria2.getPeers", json!(["token:s3cret", gid]));
    assert_eq!(peers["result"], json!([]), "{peers}");

    let multicall = rpc(
        addr,
        6,
        "system.multicall",
        json!([[
            { "methodName": "aria2.getVersion", "params": ["token:s3cret"] },
            { "methodName": "aria2.getGlobalStat", "params": ["token:s3cret"] },
        ]]),
    );
    let calls = multicall["result"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{multicall}");
    assert!(calls[0]["result"]["version"].is_string(), "{multicall}");

    // Disabling the service plugin stops routing to it but keeps the port;
    // enabling it again starts a fresh bridge without a daemon restart.
    let mut local = Client::connect(&daemon.socket).unwrap();
    assert!(local
        .plugins()
        .unwrap()
        .iter()
        .find(|plugin| plugin.id == "aria2")
        .is_some_and(|plugin| plugin.running));
    local.set_plugin_enabled("aria2", false).unwrap();
    assert!(!local
        .plugins()
        .unwrap()
        .iter()
        .find(|plugin| plugin.id == "aria2")
        .is_some_and(|plugin| plugin.running));
    let disabled = rpc(addr, 7, "aria2.getVersion", json!(["token:s3cret"]));
    assert!(
        disabled["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not installed"),
        "{disabled}"
    );
    local.set_plugin_enabled("aria2", true).unwrap();
    assert!(local
        .plugins()
        .unwrap()
        .iter()
        .find(|plugin| plugin.id == "aria2")
        .is_some_and(|plugin| plugin.running));
    let enabled = rpc(addr, 8, "aria2.getVersion", json!(["token:s3cret"]));
    assert_eq!(enabled["result"]["version"], "1.37.0", "{enabled}");

    // Native clients keep working on the same port: auth + ping, then a
    // clean shutdown (which also closes the connection).
    let responses = native_lines(
        addr,
        &[
            json!({"id": 1, "method": "auth", "settings": {"token": TOKEN}}),
            json!({"id": 2, "method": "ping", "settings": {}}),
            json!({"id": 3, "method": "shutdown", "settings": {}}),
        ],
    );
    assert_eq!(responses.len(), 3, "{responses:?}");
    assert_eq!(responses[0]["ok"], json!(true), "{responses:?}");
    assert_eq!(responses[1]["ok"], json!(true), "{responses:?}");
}

#[test]
fn inline_torrent_and_metalink_reach_their_plugins() {
    let Some(daemon) = spawn_with_downloaders(true) else {
        eprintln!("bt/http plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;

    // The bridge decodes the inline bytes, publishes them on loopback, and
    // forces the BT plugin so the .torrent URL starts a torrent instead of
    // being downloaded as a file.
    let torrent = base64::engine::general_purpose::STANDARD.encode(b"d4:infod4:name4:teste");
    let added = rpc(
        addr,
        1,
        "aria2.addTorrent",
        json!(["token:s3cret", torrent, [], {}]),
    );
    let gid = added["result"].as_str().unwrap_or_default();
    assert_eq!(gid.len(), 16, "addTorrent failed: {added}");

    // addMetalink uses the same loopback source and forces the HTTP plugin.
    let metalink = base64::engine::general_purpose::STANDARD.encode(b"<metalink/>");
    let added = rpc(
        addr,
        2,
        "aria2.addMetalink",
        json!(["token:s3cret", metalink, {}]),
    );
    let gid = added["result"].as_str().unwrap_or_default();
    assert_eq!(gid.len(), 16, "addMetalink failed: {added}");

    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}

#[test]
fn websocket_answers_calls_and_pushes_notifications() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut ws = TcpStream::connect(addr).unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    ws.write_all(
        b"GET /jsonrpc HTTP/1.1\r\n\
          Host: localhost\r\n\
          Upgrade: websocket\r\n\
          Connection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
          Sec-WebSocket-Version: 13\r\n\
          \r\n",
    )
    .unwrap();
    ws.flush().unwrap();

    // Read the 101 response byte-by-byte so no frame bytes are swallowed.
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        ws.read_exact(&mut byte).unwrap();
        response.push(byte[0]);
    }
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 101"), "{response}");
    assert!(
        response.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "{response}"
    );

    write_client_text(
        &mut ws,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "aria2.getVersion",
            "params": ["token:s3cret"],
        })
        .to_string(),
    );
    let reply = read_server_text(&mut ws).expect("no websocket reply");
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["version"], "1.37.0", "{reply}");

    // Browsers may split large addTorrent payloads across continuation frames.
    let text = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "aria2.getVersion",
        "params": ["token:s3cret"],
    })
    .to_string();
    let split = text.len() / 2;
    write_client_frame(&mut ws, 0x1, false, &text.as_bytes()[..split]);
    write_client_frame(&mut ws, 0x0, true, &text.as_bytes()[split..]);
    let reply = read_server_text(&mut ws).expect("no fragmented reply");
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["version"], "1.37.0", "{reply}");

    // A task added over HTTP must reach the WebSocket subscriber.
    let added = rpc(
        addr,
        2,
        "aria2.addUri",
        json!(["token:s3cret", ["http://example.test/ws.bin"]]),
    );
    assert!(added["result"].is_string(), "{added}");

    let mut saw_notification = false;
    for _ in 0..10 {
        let Some(text) = read_server_text(&mut ws) else {
            break;
        };
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if value["method"]
            .as_str()
            .is_some_and(|method| method.starts_with("aria2.onDownload"))
        {
            saw_notification = true;
            break;
        }
    }
    assert!(
        saw_notification,
        "no aria2.onDownload* notification arrived"
    );
    drop(ws);

    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}

fn write_client_text(stream: &mut TcpStream, text: &str) {
    write_client_frame(stream, 0x1, true, text.as_bytes());
}

fn write_client_frame(stream: &mut TcpStream, opcode: u8, fin: bool, payload: &[u8]) {
    assert!(payload.len() < 126, "test payload too large");
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let first = if fin { 0x80 | opcode } else { opcode };
    let mut frame = vec![first, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    for (index, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[index % 4]);
    }
    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
}

fn read_server_text(stream: &mut TcpStream) -> Option<String> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).ok()?;
    let opcode = header[0] & 0x0f;
    let mut length = usize::from(header[1] & 0x7f);
    if length == 126 {
        let mut extended = [0u8; 2];
        stream.read_exact(&mut extended).ok()?;
        length = usize::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0u8; 8];
        stream.read_exact(&mut extended).ok()?;
        length = usize::try_from(u64::from_be_bytes(extended)).ok()?;
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).ok()?;
    match opcode {
        0x1 => String::from_utf8(payload).ok(),
        0x8 => None,
        _ => Some(String::new()),
    }
}

#[test]
fn change_position_reorders_the_waiting_queue() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut local = Client::connect(&daemon.socket).unwrap();
    // The first task occupies the only slot; the next two queue. `duration_ms`
    // is the fake plugin's built-in sleep, keeping the first task running.
    for name in ["a.bin", "b.bin", "c.bin"] {
        let url = format!("http://example.test/{name}");
        local
            .add_with_settings(&url, None, None, json!({ "duration_ms": 5000 }))
            .unwrap();
    }

    let waiting = rpc(addr, 1, "aria2.tellWaiting", json!(["token:s3cret", 0, 10]));
    let tasks = waiting["result"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "{waiting}");
    let target = tasks[1]["gid"].as_str().unwrap().to_string();

    let moved = rpc(
        addr,
        2,
        "aria2.changePosition",
        json!(["token:s3cret", target, 0, "POS_SET"]),
    );
    assert_eq!(moved["result"], 0, "{moved}");

    let reordered = rpc(addr, 3, "aria2.tellWaiting", json!(["token:s3cret", 0, 10]));
    let tasks = reordered["result"].as_array().unwrap();
    assert_eq!(tasks[0]["gid"], target, "{reordered}");

    local.shutdown().unwrap();
}

#[test]
fn out_option_overrides_the_probed_filename() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut local = Client::connect(&daemon.socket).unwrap();
    let added = local
        .add_with_settings(
            "http://example.test/original.bin",
            None,
            None,
            json!({ "filename": "custom.bin", "duration_ms": 5000 }),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap();
    let gid = gid_of(task_id);

    let status = rpc(addr, 1, "aria2.tellStatus", json!(["token:s3cret", gid]));
    let path = status["result"]["files"][0]["path"]
        .as_str()
        .unwrap_or_default();
    assert!(path.ends_with("custom.bin"), "{status}");
    local.shutdown().unwrap();
}

fn gid_of(task_id: &str) -> String {
    let value = task_id
        .strip_prefix("task-")
        .and_then(|suffix| suffix.parse::<u64>().ok())
        .unwrap();
    format!("{value:016x}")
}

#[test]
fn task_checksum_is_enforced_at_finalize() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut local = Client::connect(&daemon.socket).unwrap();
    let expected = format!("sha256:{}", "0".repeat(64));
    let added = local
        .add_with_settings(
            "http://example.test/checksum.bin",
            None,
            None,
            json!({ "checksum": expected }),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap();
    let gid = gid_of(task_id);

    let mut last = Value::Null;
    for _ in 0..40 {
        last = rpc(addr, 1, "aria2.tellStatus", json!(["token:s3cret", gid]));
        if last["result"]["status"] == "error" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(last["result"]["status"], "error", "{last}");
    let message = last["result"]["errorMessage"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(message.contains("checksum"), "{last}");
    local.shutdown().unwrap();
}

#[test]
fn http_keep_alive_serves_multiple_requests() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    for (id, close) in [(1, false), (2, true)] {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "aria2.getVersion",
            "params": ["token:s3cret"],
        })
        .to_string();
        let connection = if close { "close" } else { "keep-alive" };
        let request = format!(
            "POST /jsonrpc HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();

        let response = read_http_response(&mut stream);
        assert_eq!(response["result"]["version"], "1.37.0", "{response}");
    }

    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}

fn read_http_response(stream: &mut TcpStream) -> Value {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let length = head
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn dead_plugin_process_fails_its_running_tasks() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let mut local = Client::connect(&daemon.socket).unwrap();
    let added = local
        .add_with_settings(
            "http://example.test/crash.bin",
            None,
            None,
            json!({ "duration_ms": 5000 }),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();

    let plugin_dir = daemon.root.join("plugins/fake/bin");
    let pid = (0..60)
        .find_map(|_| {
            let pid = find_plugin_pid(&plugin_dir);
            if pid.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            pid
        })
        .expect("fake plugin process was not started");
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    let mut state = String::new();
    for _ in 0..80 {
        state = local.task(&task_id).unwrap()["task"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "failed" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(state, "failed", "task should fail after its plugin died");
    local.shutdown().unwrap();
}

#[cfg(target_os = "linux")]
fn find_plugin_pid(directory: &Path) -> Option<u32> {
    let needle = directory.to_string_lossy();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains(needle.as_ref()) {
            return Some(pid);
        }
    }
    None
}

#[test]
fn paused_task_stays_paused_while_progress_arrives() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let mut local = Client::connect(&daemon.socket).unwrap();
    let added = local
        .add_with_settings(
            "http://example.test/pause.bin",
            None,
            None,
            json!({ "duration_ms": 5000 }),
        )
        .unwrap();
    let task_id = added["task_id"].as_str().unwrap().to_string();
    for _ in 0..40 {
        let state = local.task(&task_id).unwrap()["task"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "running" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    local.pause(&task_id).unwrap();
    // The fake plugin keeps emitting progress while paused; the store must
    // not turn that into `running` again.
    thread::sleep(Duration::from_millis(1200));
    let state = local.task(&task_id).unwrap()["task"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(state, "paused");
    local.shutdown().unwrap();
}

#[test]
fn save_session_writes_active_urls() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut local = Client::connect(&daemon.socket).unwrap();
    local
        .add_with_settings(
            "http://example.test/session.bin",
            None,
            None,
            json!({ "duration_ms": 5000 }),
        )
        .unwrap();

    let saved = rpc(addr, 1, "aria2.saveSession", json!(["token:s3cret"]));
    assert_eq!(saved["result"], "OK", "{saved}");
    let path = daemon.root.join("downloads/.aether/aria2-session.txt");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("session.bin"), "{text}");
    local.shutdown().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn killed_service_is_replaced_on_the_next_request() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let plugin_dir = daemon.root.join("plugins/aria2/bin");
    let pid = (0..60)
        .find_map(|_| {
            let pid = find_plugin_pid(&plugin_dir);
            if pid.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            pid
        })
        .expect("aria2 service process was not started");
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    let mut version = Value::Null;
    for _ in 0..40 {
        version = rpc(addr, 1, "aria2.getVersion", json!(["token:s3cret"]));
        if version["result"]["version"] == "1.37.0" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(version["result"]["version"], "1.37.0", "{version}");

    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}

#[test]
fn concurrent_http_clients_share_the_service_bridge() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let mut workers = Vec::new();
    for worker in 0..8_u64 {
        workers.push(thread::spawn(move || {
            for request in 0..10_u64 {
                let response = rpc(
                    addr,
                    worker * 1000 + request,
                    "aria2.getVersion",
                    json!(["token:s3cret"]),
                );
                assert_eq!(
                    response["result"]["version"], "1.37.0",
                    "worker {worker}: {response}"
                );
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}

#[test]
#[ignore = "soak: set AETHER_SOAK_SECS to control duration"]
fn soak_http_clients() {
    let Some(daemon) = spawn() else {
        eprintln!("plugin binaries are not built; skipping");
        return;
    };
    let addr = daemon.addr;
    let seconds = std::env::var("AETHER_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut workers = Vec::new();
    for worker in 0..8_u64 {
        workers.push(thread::spawn(move || {
            let mut request = 0_u64;
            while Instant::now() < deadline {
                request += 1;
                let response = rpc(
                    addr,
                    worker * 1_000_000 + request,
                    "aria2.getVersion",
                    json!(["token:s3cret"]),
                );
                assert_eq!(
                    response["result"]["version"], "1.37.0",
                    "worker {worker} request {request}: {response}"
                );
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let mut local = Client::connect(&daemon.socket).unwrap();
    local.shutdown().unwrap();
}
