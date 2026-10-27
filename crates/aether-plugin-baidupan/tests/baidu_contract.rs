//! Baidu Netdisk official-API adapter test with a mock API/dlink server.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};

const FILE_SIZE: usize = 128 * 1024;

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 197) as u8).collect()
}

struct TestServer {
    addr: SocketAddr,
}

fn start_server(data: Vec<u8>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            thread::spawn(move || handle(stream, &data));
        }
    });
    TestServer { addr }
}

fn handle(mut stream: TcpStream, data: &[u8]) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
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
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/");
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = value.trim().strip_prefix("bytes=");
            }
        }
    }

    if path.starts_with("/api") {
        let body = format!(
            "{{\"errno\":0,\"list\":[{{\"server_filename\":\"file.bin\",\"size\":{},\"dlink\":\"http://{}/download/file.bin?x=1\"}}]}}",
            data.len(),
            stream.local_addr().unwrap()
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    let (status, body, content_range) = if let Some(range) = range {
        let (start, end) = range.split_once('-').unwrap_or(("0", ""));
        let start: usize = start.parse().unwrap_or(0);
        let end = end
            .parse::<usize>()
            .unwrap_or(data.len() - 1)
            .min(data.len() - 1);
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
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: \"baidu-test\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
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
        "aether-baidu-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "baidupan",
        "name": "Baidu plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["baidupan"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-baidupan"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn downloads_via_official_api() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("download");
    std::env::set_var("BAIDU_ACCESS_TOKEN", "test-token");
    std::env::set_var("BAIDU_API_BASE", format!("http://{}/api", server.addr));
    let api = format!("http://{}/api", server.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe("baidupan://123456").unwrap();
    assert!(probe.supported);
    assert_eq!(
        probe.suggested_filename.as_deref(),
        Some("file.bin"),
        "reason={:?}",
        probe.reason
    );

    let staging = root.join("staging");
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url: "baidupan://123456".into(),
                filename: None,
                settings: serde_json::json!({
                    "access_token": "test-token",
                    "api_base": api
                }),
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
    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
