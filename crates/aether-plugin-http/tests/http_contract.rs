//! End-to-end HTTP plugin tests against a local Range-capable server.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
            thread::spawn(move || handle_connection(stream, &data, &ranges, &headers, &targets));
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
            _ => {}
        }
    }
    panic!("task did not complete");
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    assert_eq!(manifest.id, PLUGIN_ID);
    assert_eq!(manifest.version, PLUGIN_VERSION);
    manifest.validate().unwrap();
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
