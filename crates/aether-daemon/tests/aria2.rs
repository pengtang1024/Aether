//! aria2 JSON-RPC compatibility over the daemon's shared TCP port.
//!
//! End-to-end: HTTP request -> daemon token check -> service plugin IPC ->
//! plugin's native daemon client -> fake downloader. Also proves the native
//! line protocol still works on the same port.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    local.set_plugin_enabled("aria2", false).unwrap();
    let disabled = rpc(addr, 7, "aria2.getVersion", json!(["token:s3cret"]));
    assert!(
        disabled["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not installed or enabled"),
        "{disabled}"
    );
    local.set_plugin_enabled("aria2", true).unwrap();
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
    let payload = text.as_bytes();
    assert!(payload.len() < 126, "test payload too large");
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
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
