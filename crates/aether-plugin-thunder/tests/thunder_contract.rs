//! thunder:// decode + HTTP download test.

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
use base64::Engine;

const FILE_SIZE: usize = 128 * 1024;

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 211) as u8).collect()
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
    let method = lines
        .next()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("GET");
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = value.trim().strip_prefix("bytes=");
            }
        }
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
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: \"thunder-test\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
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
        "aether-thunder-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "thunder",
        "name": "Thunder plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["thunder"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-thunder"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn downloads_decoded_thunder_link() {
    let data = test_data();
    let server = start_server(data.clone());
    let original = format!("http://{}/file.bin", server.addr);
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("AA{original}ZZ"));
    let url = format!("thunder://{encoded}");
    let root = temp_dir("download");

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("file.bin"));

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
