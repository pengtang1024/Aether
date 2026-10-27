//! S3 plugin test against a local unsigned object server.

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

const FILE_SIZE: usize = 128 * 1024;

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 223) as u8).collect()
}

struct TestServer {
    addr: SocketAddr,
    authorizations: Arc<Mutex<Vec<String>>>,
}

fn start_server(data: Vec<u8>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    let authorizations = Arc::new(Mutex::new(Vec::new()));
    let auths = authorizations.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let auths = auths.clone();
            thread::spawn(move || handle(stream, &data, &auths));
        }
    });
    TestServer {
        addr,
        authorizations,
    }
}

fn handle(mut stream: TcpStream, data: &[u8], auths: &Mutex<Vec<String>>) {
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
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = value.trim().strip_prefix("bytes=");
            }
            if name.eq_ignore_ascii_case("authorization") {
                auths.lock().unwrap().push(value.trim().to_string());
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
        "HTTP/1.1 {status}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nETag: \"s3-test\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
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
    let dir =
        std::env::temp_dir().join(format!("aether-s3-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "s3",
        "name": "S3 plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["s3"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-s3"))
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    manifest.validate().unwrap();
}

#[test]
fn downloads_s3_object_with_sigv4_headers() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("download");
    let url = "s3://bucket/path/file.bin".to_string();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);

    let staging = root.join("staging");
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings: serde_json::json!({
                    "access_key": "AKIDEXAMPLE",
                    "secret_key": "secret",
                    "region": "us-east-1",
                    "endpoint": format!("http://{}", server.addr),
                    "path_style": true
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
    let auths = server.authorizations.lock().unwrap();
    assert!(!auths.is_empty());
    assert!(auths
        .iter()
        .all(|auth| auth.contains("Credential=AKIDEXAMPLE")));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
