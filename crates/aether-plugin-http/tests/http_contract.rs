//! End-to-end HTTP plugin tests against a local Range-capable server.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_http::state::{self, ResumeState};
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};
use sha2::{Digest, Sha256};

const PLUGIN_ID: &str = "http";
const PLUGIN_VERSION: &str = "0.1.0";
const ETAG: &str = "\"aether-test-etag\"";
const FILE_SIZE: usize = 512 * 1024;

struct TestServer {
    addr: SocketAddr,
    ranges: Arc<Mutex<Vec<String>>>,
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
    let data = Arc::new(data);
    let server_ranges = ranges.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let ranges = server_ranges.clone();
            thread::spawn(move || handle_connection(stream, &data, &ranges));
        }
    });

    TestServer { addr, ranges }
}

fn handle_connection(mut stream: TcpStream, data: &[u8], ranges: &Mutex<Vec<String>>) {
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
    let _path = parts.next().unwrap_or("/");

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
                    value: checksum,
                }),
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
