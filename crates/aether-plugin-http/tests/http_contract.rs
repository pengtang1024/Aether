//! End-to-end HTTP plugin tests against a local Range-capable server.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_http::{
    client,
    state::{self, ResumeState},
};
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, ProxyConfig, TaskPaths, TaskSpec, API_VERSION,
};
use sha2::{Digest, Sha256};

const PLUGIN_ID: &str = "http";
const PLUGIN_VERSION: &str = "0.1.0";
const ETAG: &str = "\"aether-test-etag\"";
const FILE_SIZE: usize = 512 * 1024;

struct TestServer {
    addr: SocketAddr,
    ranges: Arc<Mutex<Vec<String>>>,
    headers: Arc<Mutex<Vec<String>>>,
    /// Request targets as they arrived. With a forward proxy these are
    /// absolute URLs (`http://host/path`), which is how proxy usage is proven.
    targets: Arc<Mutex<Vec<String>>>,
}

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 251) as u8).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn start_server(data: Vec<u8>) -> TestServer {
    start_server_with_etag(data, ETAG)
}

fn start_server_with_etag(data: Vec<u8>, etag: &'static str) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let data = Arc::new(data);
    let server_ranges = ranges.clone();
    let server_headers = headers.clone();
    let server_targets = targets.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let ranges = server_ranges.clone();
            let headers = server_headers.clone();
            let targets = server_targets.clone();
            thread::spawn(move || {
                handle_connection(stream, &data, &ranges, &headers, &targets, etag)
            });
        }
    });

    TestServer {
        addr,
        ranges,
        headers,
        targets,
    }
}

fn handle_connection(
    mut stream: TcpStream,
    data: &[u8],
    ranges: &Mutex<Vec<String>>,
    headers: &Mutex<Vec<String>>,
    targets: &Mutex<Vec<String>>,
    etag: &str,
) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&request);
    let mut lines = text.lines();
    let Some(request_line) = lines.next() else {
        return;
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    targets.lock().unwrap().push(target.to_string());

    let mut range_header = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range_header = Some(value.trim().to_string());
            }
            headers
                .lock()
                .unwrap()
                .push(format!("{}: {}", name.trim(), value.trim()));
        }
    }

    if let Some(range) = &range_header {
        ranges.lock().unwrap().push(range.clone());
    }

    let range = range_header
        .as_deref()
        .and_then(|value| parse_range(value, data.len()));
    let (status, body, content_range) = match range {
        Some((start, end)) if method != "HEAD" => {
            let body = data[start..=end].to_vec();
            (
                "206 Partial Content",
                body,
                Some(format!("bytes {start}-{end}/{}", data.len())),
            )
        }
        Some((start, end)) => (
            "206 Partial Content",
            Vec::new(),
            Some(format!("bytes {start}-{end}/{}", data.len())),
        ),
        None => ("200 OK", data.to_vec(), None),
    };

    let body_len = if method == "HEAD" {
        if let Some((start, end)) = range {
            end - start + 1
        } else {
            data.len()
        }
    } else {
        body.len()
    };

    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
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

struct TruncatingServer {
    addr: SocketAddr,
    ranges: Arc<Mutex<Vec<String>>>,
}

/// Serve valid headers, then send only `cut` body bytes and close. This models
/// a source that dies mid-range without changing its ETag/size.
fn start_truncating_server(data: Vec<u8>, cut: usize) -> TruncatingServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let server_ranges = ranges.clone();
    let data = Arc::new(data);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let ranges = server_ranges.clone();
            thread::spawn(move || handle_truncating(stream, &data, cut, &ranges));
        }
    });
    TruncatingServer { addr, ranges }
}

fn handle_truncating(mut stream: TcpStream, data: &[u8], cut: usize, ranges: &Mutex<Vec<String>>) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&request);
    let mut lines = text.lines();
    let Some(request_line) = lines.next() else {
        return;
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let mut range_header = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range_header = Some(value.trim().to_string());
            }
        }
    }
    if let Some(range) = &range_header {
        ranges.lock().unwrap().push(range.clone());
    }
    let range = range_header
        .as_deref()
        .and_then(|value| parse_range(value, data.len()));
    let (start, end) = range.unwrap_or((0, data.len() - 1));
    let body_len = end - start + 1;
    let status = if range_header.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: {ETAG}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if range_header.is_some() {
        response.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            data.len()
        ));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    if method != "HEAD" {
        let sent = cut.min(body_len);
        let _ = stream.write_all(&data[start..start + sent]);
    }
    let _ = stream.flush();
}

fn write_at(file: &std::fs::File, mut buffer: &[u8], mut offset: u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        while !buffer.is_empty() {
            let written = file.write_at(buffer, offset).unwrap();
            buffer = &buffer[written..];
            offset += written as u64;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt as _;
        while !buffer.is_empty() {
            let written = file.seek_write(buffer, offset).unwrap();
            buffer = &buffer[written..];
            offset += written as u64;
        }
    }
}

fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let value = value.strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    let start: usize = start.parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if end.is_empty() {
        len - 1
    } else {
        end.parse::<usize>().ok()?.min(len - 1)
    };
    (start <= end).then_some((start, end))
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-http-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": PLUGIN_ID,
        "name": "HTTP plugin",
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-http"))
}

fn start_task(plugin: &mut PluginProcess, url: String, staging: PathBuf, checksum: String) {
    start_task_with_settings(plugin, url, staging, checksum, serde_json::json!({}))
}

fn start_task_with_settings(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    checksum: String,
    settings: serde_json::Value,
) {
    start_task_full(plugin, url, staging, checksum, settings, None)
}

fn start_task_full(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    checksum: String,
    settings: serde_json::Value,
    proxy: Option<ProxyConfig>,
) {
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings,
                plugin_state: None,
                expected_size: None,
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: checksum,
                }),
                proxy,
            },
            TaskPaths {
                staging,
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget {
                connections: 4,
                per_host_connections: 4,
                download_bps: 0,
                upload_bps: 0,
            },
        )
        .unwrap();
}

fn wait_for_completed(plugin: &mut PluginProcess) {
    for _ in 0..100 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => return,
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            PluginEvent::Log {
                level: LogLevel::Warn,
                message,
                ..
            } => eprintln!("[warn] {message}"),
            _ => {}
        }
    }
    panic!("task did not complete");
}

fn wait_for_failed(plugin: &mut PluginProcess) -> aether_plugin_sdk::PluginError {
    for _ in 0..100 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => return error,
            PluginEvent::Completed { .. } => panic!("task unexpectedly completed"),
            _ => {}
        }
    }
    panic!("task did not finish");
}

fn start_task_one_connection(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    checksum: String,
    settings: serde_json::Value,
) {
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings,
                plugin_state: None,
                expected_size: Some(FILE_SIZE as u64),
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: checksum,
                }),
                proxy: None,
            },
            TaskPaths {
                staging,
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget {
                connections: 1,
                per_host_connections: 1,
                download_bps: 0,
                upload_bps: 0,
            },
        )
        .unwrap();
}

fn range_start(range: &str) -> Option<u64> {
    range
        .strip_prefix("bytes=")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    assert_eq!(manifest.id, PLUGIN_ID);
    assert_eq!(manifest.version, PLUGIN_VERSION);
    manifest.validate().unwrap();
}

fn dead_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr
}

#[test]
fn uses_a_mirror_when_the_primary_looks_down() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("mirror");
    let primary = format!("http://{}/file.bin", dead_addr());
    let mirror = format!("http://{}/file.bin", server.addr);
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task_with_settings(
        &mut plugin,
        primary,
        staging,
        sha256_hex(&data),
        serde_json::json!({ "mirrors": [mirror] }),
    );
    wait_for_completed(&mut plugin);
    plugin.shutdown().unwrap();
    fs::remove_dir_all(&root).ok();
}

#[test]
fn downloads_segmented_file_and_verifies_checksum() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("segmented");
    let url = format!("http://{}/file.bin", server.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("file.bin"));

    let staging = root.join("staging");
    start_task(&mut plugin, url, staging.clone(), sha256_hex(&data));
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let ranges = server.ranges.lock().unwrap().clone();
    assert!(
        ranges.len() >= 2,
        "expected segmented requests, got {ranges:?}"
    );
    assert!(ranges.iter().all(|range| range.starts_with("bytes=")));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn resumes_from_persisted_state() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("resume");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();

    let url = format!("http://{}/file.bin", server.addr);
    let mut resume = ResumeState::new(
        url.clone(),
        Some(ETAG.into()),
        data.len() as u64,
        "file.bin",
        true,
        1,
    );
    resume.ranges[0].done = 100;
    state::write(&staging, &resume).unwrap();

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(staging.join("file.bin"))
        .unwrap();
    file.set_len(data.len() as u64).unwrap();
    let head = std::fs::OpenOptions::new()
        .write(true)
        .open(staging.join("file.bin"))
        .unwrap();
    write_at(&head, &data[..100], 0);
    drop(head);
    drop(file);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task(&mut plugin, url, staging.clone(), sha256_hex(&data));
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let ranges = server.ranges.lock().unwrap().clone();
    let expected = format!("bytes=100-{}", FILE_SIZE - 1);
    assert!(
        ranges.iter().any(|range| range == &expected),
        "expected resume range {expected}, got {ranges:?}"
    );

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn sends_settings_headers_with_every_request() {
    let data = test_data();
    let server = start_server(data.clone());
    let mut plugin =
        PluginProcess::spawn(manifest(), &executable(), init(&temp_dir("headers-init"))).unwrap();

    let staging = temp_dir("headers-staging");
    let settings = serde_json::json!({
        "referrer": "https://example.com/page",
        "user_agent": "AetherTest/1.0",
        "cookies": {"a": "1", "b": "2"},
        "headers": {"X-Custom-Header": "enabled"}
    });
    start_task_with_settings(
        &mut plugin,
        format!("http://{}/file.bin", server.addr),
        staging.clone(),
        sha256_hex(&data),
        settings,
    );
    wait_for_completed(&mut plugin);

    let seen = server.headers.lock().unwrap().join("\n");
    assert!(
        seen.contains("referer: https://example.com/page"),
        "missing referer in:\n{seen}"
    );
    assert!(
        seen.contains("user-agent: AetherTest/1.0"),
        "missing user agent in:\n{seen}"
    );
    assert!(
        seen.contains("cookie: a=1; b=2"),
        "missing cookies in:\n{seen}"
    );
    assert!(
        seen.contains("x-custom-header: enabled"),
        "missing custom header in:\n{seen}"
    );
}

#[test]
fn rejects_managed_headers_from_settings() {
    let extra = client::headers_from_settings(&serde_json::json!({
        "headers": {"Range": "bytes=0-1"}
    }));
    assert!(
        extra.is_err(),
        "Range must not be overridable from settings"
    );

    let object = client::headers_from_settings(&serde_json::json!({
        "cookies": {"a": "1"},
        "referrer": "https://example.com/"
    }))
    .unwrap();
    assert_eq!(object.get("cookie").unwrap(), "a=1");
    assert_eq!(object.get("referer").unwrap(), "https://example.com/");
}

#[test]
fn throttles_when_the_budget_limits_speed() {
    // 16 KiB at 8 KiB/s must take noticeably longer than an instant.
    let data = vec![0x5A_u8; 16 * 1024];
    let server = start_server(data.clone());
    let mut plugin =
        PluginProcess::spawn(manifest(), &executable(), init(&temp_dir("throttle-init"))).unwrap();

    let staging = temp_dir("throttle-staging");
    let started = std::time::Instant::now();
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url: format!("http://{}/file.bin", server.addr),
                filename: None,
                settings: serde_json::json!({}),
                plugin_state: None,
                expected_size: None,
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: sha256_hex(&data),
                }),
                proxy: None,
            },
            TaskPaths {
                staging,
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget {
                connections: 1,
                per_host_connections: 1,
                download_bps: 8 * 1024,
                upload_bps: 0,
            },
        )
        .unwrap();
    wait_for_completed(&mut plugin);

    let elapsed = started.elapsed();
    // 16 KiB / 8 KiB/s = ~2 s; allow scheduler slack on both ends.
    assert!(
        elapsed >= Duration::from_millis(1500),
        "download finished too fast: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(10),
        "download unreasonably slow: {elapsed:?}"
    );
}

/// A configured proxy must receive absolute-form requests; the target host
/// name does not resolve, so only going through the proxy can succeed.
#[test]
fn downloads_through_a_configured_proxy() {
    let data = test_data();
    let proxy = start_server(data.clone());
    let root = temp_dir("proxy");
    let url = "http://aether-proxy-test.invalid/file.bin";

    let mut settings = init(&root);
    settings.proxy = Some(ProxyConfig::parse(&format!("http://{}", proxy.addr)).unwrap());
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), settings).unwrap();

    let probe = plugin.probe(url).unwrap();
    assert!(probe.supported, "proxy probe failed: {probe:?}");

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        url.to_string(),
        staging.clone(),
        sha256_hex(&data),
    );
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let targets = proxy.targets.lock().unwrap().clone();
    assert!(!targets.is_empty(), "proxy saw no requests");
    assert!(
        targets
            .iter()
            .all(|target| target == "http://aether-proxy-test.invalid/file.bin"),
        "expected absolute-form proxy targets, got {targets:?}"
    );

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

/// The Host resolves PAC per URL, so a task carries its own proxy; it must win
/// over the process default (here: none) for both probing and downloading.
#[test]
fn honors_a_per_task_proxy_override() {
    let data = test_data();
    let proxy = start_server(data.clone());
    let root = temp_dir("task-proxy");
    let url = "http://aether-task-proxy.invalid/file.bin";
    let task_proxy = Some(ProxyConfig::parse(&format!("http://{}", proxy.addr)).unwrap());

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe_with_proxy(url, task_proxy.clone()).unwrap();
    assert!(probe.supported, "per-task proxy probe failed: {probe:?}");

    let staging = root.join("staging");
    start_task_full(
        &mut plugin,
        url.to_string(),
        staging.clone(),
        sha256_hex(&data),
        serde_json::json!({}),
        task_proxy,
    );
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let targets = proxy.targets.lock().unwrap().clone();
    assert!(!targets.is_empty(), "proxy saw no requests");
    assert!(targets
        .iter()
        .all(|target| target == "http://aether-task-proxy.invalid/file.bin"));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn fails_over_mid_download_and_keeps_completed_ranges() {
    let data = test_data();
    let cut = 16 * 1024;
    let primary = start_truncating_server(data.clone(), cut);
    let mirror = start_server(data.clone());
    let root = temp_dir("mid-failover");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    let mirror_url = format!("http://{}/file.bin", mirror.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task_one_connection(
        &mut plugin,
        format!("http://{}/file.bin", primary.addr),
        staging.clone(),
        sha256_hex(&data),
        serde_json::json!({ "mirrors": [mirror_url] }),
    );
    wait_for_completed(&mut plugin);
    plugin.shutdown().unwrap();

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let primary_ranges = primary.ranges.lock().unwrap().clone();
    assert!(
        !primary_ranges.is_empty(),
        "the primary must have been tried"
    );
    let mirror_ranges = mirror.ranges.lock().unwrap().clone();
    assert!(!mirror_ranges.is_empty(), "the mirror must have been used");
    // The probe uses a 1-byte `bytes=0-0` request; the actual download must
    // resume strictly after the bytes the primary delivered.
    assert!(
        mirror_ranges
            .iter()
            .filter_map(|range| range_start(range))
            .any(|start| start > 0),
        "failover must resume after the bytes the primary delivered: {mirror_ranges:?}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn refuses_a_mirror_with_a_different_etag() {
    let data = test_data();
    let primary = start_truncating_server(data.clone(), 16 * 1024);
    let mirror = start_server_with_etag(data.clone(), "\"different-etag\"");
    let root = temp_dir("mid-failover-etag");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    let mirror_url = format!("http://{}/file.bin", mirror.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task_one_connection(
        &mut plugin,
        format!("http://{}/file.bin", primary.addr),
        staging,
        sha256_hex(&data),
        serde_json::json!({ "mirrors": [mirror_url] }),
    );
    let error = wait_for_failed(&mut plugin);
    assert!(
        !error.message.is_empty(),
        "the primary's failure must be reported"
    );
    plugin.shutdown().unwrap();
    let mirror_ranges = mirror.ranges.lock().unwrap().clone();
    assert!(
        mirror_ranges.iter().all(|range| range == "bytes=0-0"),
        "an incompatible mirror may only answer the probe, not a download: {mirror_ranges:?}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn falls_through_multiple_mirrors() {
    let data = test_data();
    let primary = start_truncating_server(data.clone(), 16 * 1024);
    let first_mirror = start_truncating_server(data.clone(), 32 * 1024);
    let second_mirror = start_server(data.clone());
    let root = temp_dir("multi-failover");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task_one_connection(
        &mut plugin,
        format!("http://{}/file.bin", primary.addr),
        staging.clone(),
        sha256_hex(&data),
        serde_json::json!({
            "mirrors": [
                format!("http://{}/file.bin", first_mirror.addr),
                format!("http://{}/file.bin", second_mirror.addr),
            ]
        }),
    );
    wait_for_completed(&mut plugin);
    plugin.shutdown().unwrap();

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    assert!(
        !first_mirror.ranges.lock().unwrap().is_empty(),
        "the first mirror must be attempted before the second"
    );
    let second_ranges = second_mirror.ranges.lock().unwrap().clone();
    assert!(
        second_ranges
            .iter()
            .filter_map(|range| range_start(range))
            .any(|start| start > 0),
        "the last mirror must resume, not restart: {second_ranges:?}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn weak_etags_disable_mid_download_failover() {
    let data = test_data();
    let primary = start_truncating_server(data.clone(), 16 * 1024);
    let mirror = start_server_with_etag(data.clone(), "W/\"weak-etag\"");
    let root = temp_dir("weak-etag");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task_one_connection(
        &mut plugin,
        format!("http://{}/file.bin", primary.addr),
        staging,
        sha256_hex(&data),
        serde_json::json!({
            "mirrors": [format!("http://{}/file.bin", mirror.addr)]
        }),
    );
    let _ = wait_for_failed(&mut plugin);
    plugin.shutdown().unwrap();
    let ranges = mirror.ranges.lock().unwrap().clone();
    assert!(
        ranges.iter().all(|range| range == "bytes=0-0"),
        "a weak ETag must not authorize mid-download failover: {ranges:?}"
    );
    fs::remove_dir_all(&root).ok();
}

/// Server that limits how many connections may be open at once and answers
/// the overflow with `status`. Bodies are streamed at `bytes_per_sec`, which
/// keeps concurrent connections open long enough to observe a real peak.
struct GatedServer {
    addr: SocketAddr,
    peak: Arc<AtomicUsize>,
    rejections: Arc<AtomicUsize>,
}

fn start_gated_server(
    data: Vec<u8>,
    limit: usize,
    status: u16,
    bytes_per_sec: usize,
) -> GatedServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    let peak = Arc::new(AtomicUsize::new(0));
    let rejections = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let server_peak = peak.clone();
    let server_rejections = rejections.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let peak = server_peak.clone();
            let rejections = server_rejections.clone();
            let current = current.clone();
            thread::spawn(move || {
                handle_gated(
                    stream,
                    &data,
                    limit,
                    status,
                    bytes_per_sec,
                    &current,
                    &peak,
                    &rejections,
                )
            });
        }
    });

    GatedServer {
        addr,
        peak,
        rejections,
    }
}

struct ActiveConnections(Arc<AtomicUsize>);

impl Drop for ActiveConnections {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_gated(
    mut stream: TcpStream,
    data: &[u8],
    limit: usize,
    status: u16,
    bytes_per_sec: usize,
    current: &Arc<AtomicUsize>,
    peak: &Arc<AtomicUsize>,
    rejections: &Arc<AtomicUsize>,
) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&request);
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or_default();
    let method = request_line.split_whitespace().next().unwrap_or("GET");
    let mut range_header = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range_header = Some(value.trim().to_string());
            }
        }
    }

    // Probes (`bytes=0-0` / HEAD) are metadata requests. Excluding them from
    // the accounting lets the test measure the download connections only.
    let is_probe = method == "HEAD" || range_header.as_deref() == Some("bytes=0-0");
    let _counted = if is_probe {
        None
    } else {
        let active = current.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(active, Ordering::SeqCst);
        // The guard must exist before the reject path returns, otherwise a
        // rejected connection would be counted forever.
        let guard = ActiveConnections(current.clone());
        if active > limit {
            rejections.fetch_add(1, Ordering::SeqCst);
            let text = if status == 429 {
                "Too Many Requests"
            } else {
                "Teapot"
            };
            let response = format!(
                "HTTP/1.1 {status} {text}\r\nContent-Length: 0\r\nRetry-After: 1\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            return;
        }
        Some(guard)
    };

    let range = range_header
        .as_deref()
        .and_then(|value| parse_range(value, data.len()))
        .unwrap_or((0, data.len() - 1));
    let (start, end) = range;
    let body_len = end - start + 1;
    let status_line = if range_header.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: {ETAG}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if range_header.is_some() {
        response.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            data.len()
        ));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());

    if method != "HEAD" {
        let per_chunk = Duration::from_secs_f64(16.0 * 1024.0 / bytes_per_sec as f64);
        let chunks = data[start..=end].chunks(16 * 1024).count();
        for (index, piece) in data[start..=end].chunks(16 * 1024).enumerate() {
            if stream.write_all(piece).is_err() {
                return;
            }
            // Do not sleep after the last chunk: the connection must close as
            // soon as the client has all bytes, or the next request overlaps.
            if index + 1 < chunks {
                thread::sleep(per_chunk);
            }
        }
    }
}

fn start_task_as(
    plugin: &mut PluginProcess,
    task_id: &str,
    url: String,
    staging: PathBuf,
    checksum: String,
    connections: u32,
    per_host_connections: u32,
) {
    plugin
        .start_task(
            TaskSpec {
                task_id: task_id.into(),
                url,
                filename: None,
                settings: serde_json::json!({}),
                plugin_state: None,
                expected_size: None,
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: checksum,
                }),
                proxy: None,
            },
            TaskPaths {
                staging,
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget {
                connections,
                per_host_connections,
                download_bps: 0,
                upload_bps: 0,
            },
        )
        .unwrap();
}

fn wait_for_all_completed(plugin: &mut PluginProcess, task_ids: &[&str]) {
    let mut done = Vec::new();
    for _ in 0..400 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Completed { task_id, .. } => {
                if task_ids.contains(&task_id.as_str()) {
                    done.push(task_id);
                }
                if done.len() == task_ids.len() {
                    return;
                }
            }
            PluginEvent::Failed { task_id, error } => {
                panic!("task {task_id} failed: {error:?}")
            }
            PluginEvent::Log {
                level: LogLevel::Warn,
                message,
                ..
            } => eprintln!("[warn] {message}"),
            _ => {}
        }
    }
    panic!("tasks {task_ids:?} did not complete");
}

/// Three tasks asking for 8 connections each on the same host must never
/// exceed the 8-connection host cap, and every task must still finish.
#[test]
fn per_host_cap_holds_across_three_tasks() {
    let data = test_data();
    let server = start_gated_server(data.clone(), 8, 418, 64 * 1024);
    let root = temp_dir("per-host-cap");
    let url = format!("http://{}/file.bin", server.addr);
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    for task in ["task-a", "task-b", "task-c"] {
        let staging = root.join(task);
        fs::create_dir_all(&staging).unwrap();
        start_task_as(
            &mut plugin,
            task,
            url.clone(),
            staging.clone(),
            sha256_hex(&data),
            8,
            8,
        );
    }
    wait_for_all_completed(&mut plugin, &["task-a", "task-b", "task-c"]);
    plugin.shutdown().unwrap();

    for task in ["task-a", "task-b", "task-c"] {
        assert_eq!(fs::read(root.join(task).join("file.bin")).unwrap(), data);
    }
    let peak = server.peak.load(Ordering::SeqCst);
    eprintln!(
        "[cap] peak={peak} rejections={}",
        server.rejections.load(Ordering::SeqCst)
    );
    assert!(
        peak <= 8,
        "24 requested connections must stay at the cap: {peak}"
    );
    assert!(
        peak >= 4,
        "the tasks must actually download concurrently: {peak}"
    );
    assert_eq!(
        server.rejections.load(Ordering::SeqCst),
        0,
        "the cap must be enforced by us, not by the server's 418"
    );
    fs::remove_dir_all(&root).ok();
}

/// A source that allows one connection and answers 418 beyond it: the task
/// must cool down, halve its connection count and finish instead of failing
/// with zero bytes.
#[test]
fn cooldown_recovers_from_418_and_finishes() {
    let data = test_data();
    let server = start_gated_server(data.clone(), 1, 418, 256 * 1024);
    let root = temp_dir("418-cooldown");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    start_task_as(
        &mut plugin,
        "task-1",
        format!("http://{}/file.bin", server.addr),
        staging.clone(),
        sha256_hex(&data),
        4,
        4,
    );
    wait_for_completed(&mut plugin);
    plugin.shutdown().unwrap();

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    assert!(
        server.rejections.load(Ordering::SeqCst) > 0,
        "the server must have rejected the over-quota connections"
    );
    fs::remove_dir_all(&root).ok();
}
