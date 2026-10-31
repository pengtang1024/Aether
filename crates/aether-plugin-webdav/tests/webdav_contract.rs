//! WebDAV plugin contract test: scheme rewrite + PROPFIND metadata + HTTP
//! range download.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};

const FILE_SIZE: usize = 256 * 1024;
const ETAG: &str = "\"webdav-etag\"";

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 227) as u8).collect()
}

#[derive(Debug, Clone)]
struct RequestRecord {
    method: String,
    path: String,
    depth: Option<String>,
    authorization: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropfindMode {
    MultiStatus,
    Collection,
    Unsupported,
}

struct TestServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
}

impl TestServer {
    fn requests(&self) -> Vec<RequestRecord> {
        self.requests.lock().unwrap().clone()
    }
}

fn start_server(data: Vec<u8>) -> TestServer {
    start_server_with(data, PropfindMode::MultiStatus)
}

fn start_server_with(data: Vec<u8>, mode: PropfindMode) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let requests = recorded.clone();
            thread::spawn(move || handle(stream, &data, mode, &requests));
        }
    });
    TestServer { addr, requests }
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..position]).to_string();
        let content_length = headers
            .lines()
            .skip(1)
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        // Drain a PROPFIND request body so closing the socket does not reset
        // the connection before the client reads the response.
        while request.len() < position + 4 + content_length {
            let read = stream.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        return Some(headers);
    }
}

fn multistatus(size: usize, collection: bool) -> String {
    let resource_type = if collection { "<D:collection/>" } else { "" };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/files/file.bin</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>{size}</D:getcontentlength>
        <D:getlastmodified>Wed, 01 Jan 2025 00:00:00 GMT</D:getlastmodified>
        <D:getetag>{ETAG}</D:getetag>
        <D:resourcetype>{resource_type}</D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#
    )
}

fn handle(
    mut stream: TcpStream,
    data: &[u8],
    mode: PropfindMode,
    requests: &Mutex<Vec<RequestRecord>>,
) {
    let Some(headers) = read_request(&mut stream) else {
        return;
    };

    let mut lines = headers.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut range = None;
    let mut depth = None;
    let mut authorization = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("range") {
                range = value.strip_prefix("bytes=").map(ToOwned::to_owned);
            } else if name.eq_ignore_ascii_case("depth") {
                depth = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_string());
            }
        }
    }
    requests.lock().unwrap().push(RequestRecord {
        method: method.clone(),
        path,
        depth,
        authorization,
    });

    if method == "PROPFIND" {
        if mode == PropfindMode::Unsupported {
            let response =
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
        } else {
            let body = multistatus(data.len(), mode == PropfindMode::Collection);
            let response = format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nContent-Type: application/xml; charset=utf-8\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
        let _ = stream.flush();
        return;
    }

    let (status, body, content_range) = if let Some(range) = range.as_deref() {
        let (start, end) = range.split_once('-').unwrap_or(("0", ""));
        let start: usize = start.parse().unwrap_or(0);
        let end: usize = end.parse().unwrap_or(data.len() - 1).min(data.len() - 1);
        (
            "206 Partial Content",
            data[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{}", data.len())),
        )
    } else {
        ("200 OK", data.to_vec(), None)
    };

    let body_len = if method == "HEAD" {
        data.len()
    } else {
        body.len()
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: {ETAG}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if let Some(content_range) = content_range {
        response.push_str(&format!("Content-Range: {content_range}\r\n"));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    if method != "HEAD" {
        let _ = stream.write_all(&body);
    }
    let _ = stream.flush();
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-webdav-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "webdav",
        "name": "WebDAV plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["webdav"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-webdav"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn downloads_via_webdav_scheme() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("download");
    let url = format!(
        "webdav://user:pass@127.0.0.1:{}/files/file.bin",
        server.addr.port()
    );

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("file.bin"));
    let metadata = probe.metadata.as_ref().expect("probe metadata");
    assert_eq!(metadata.size, Some(FILE_SIZE as u64));
    assert_eq!(metadata.etag.as_deref(), Some(ETAG));
    let propfind = server
        .requests()
        .into_iter()
        .find(|request| request.method == "PROPFIND")
        .expect("PROPFIND probe request");
    assert_eq!(propfind.path, "/files/file.bin");
    assert_eq!(propfind.depth.as_deref(), Some("0"));
    assert_eq!(
        propfind.authorization.as_deref(),
        Some("Basic dXNlcjpwYXNz")
    );

    let staging = root.join("staging");
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings: serde_json::json!({}),
                plugin_state: None,
                expected_size: None,
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: sha256_bytes(&data),
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
    let downloaded = fs::read(staging.join("file.bin")).unwrap();
    assert_eq!(downloaded, data);
    assert_eq!(metadata.size, Some(downloaded.len() as u64));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn probes_via_head_when_propfind_is_unsupported() {
    let data = test_data();
    let server = start_server_with(data, PropfindMode::Unsupported);
    let root = temp_dir("probe-head");
    let url = format!(
        "webdav://user:pass@127.0.0.1:{}/files/file.bin",
        server.addr.port()
    );

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();

    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("file.bin"));
    let metadata = probe.metadata.as_ref().expect("probe metadata");
    assert_eq!(metadata.size, Some(FILE_SIZE as u64));
    assert_eq!(metadata.etag.as_deref(), Some(ETAG));

    let requests = server.requests();
    assert!(requests.iter().any(|request| request.method == "PROPFIND"));
    assert!(requests.iter().any(|request| request.method == "HEAD"));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn reports_webdav_collections_as_unsupported() {
    let data = test_data();
    let server = start_server_with(data, PropfindMode::Collection);
    let root = temp_dir("probe-collection");
    let url = format!(
        "webdav://user:pass@127.0.0.1:{}/files/folder",
        server.addr.port()
    );

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();

    assert!(!probe.supported);
    assert!(probe.metadata.is_none());
    assert_eq!(
        probe.reason.as_deref(),
        Some("WebDAV collection, not a file")
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PROPFIND");

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
