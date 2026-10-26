//! Metalink resolution + download test.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Init, LogLevel, PluginEvent, PluginManifest, PluginProcess,
    TaskPaths, TaskSpec, API_VERSION,
};

struct TestServer {
    addr: SocketAddr,
}

fn start_server(routes: Vec<(String, String, Vec<u8>)>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(
        routes
            .into_iter()
            .map(|(path, content_type, body)| (path, (content_type, body)))
            .collect::<HashMap<_, _>>(),
    );
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let routes = routes.clone();
            thread::spawn(move || handle(stream, &routes));
        }
    });
    TestServer { addr }
}

fn handle(mut stream: TcpStream, routes: &HashMap<String, (String, Vec<u8>)>) {
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
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (content_type, body) = routes
        .get(path)
        .cloned()
        .unwrap_or_else(|| ("text/plain".into(), b"not found".to_vec()));
    let status = if routes.contains_key(path) {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-metalink-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "http",
        "name": "HTTP plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["http", "https", "metalink"],
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

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    assert!(manifest
        .protocols
        .iter()
        .any(|protocol| protocol == "metalink"));
    manifest.validate().unwrap();
}

#[test]
fn resolves_and_downloads_metalink() {
    let data = (0..4096)
        .map(|index| (index % 239) as u8)
        .collect::<Vec<_>>();
    let checksum = sha256_bytes(&data);
    let metalink = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="payload.bin">
                <size>{}</size>
                <hash type="sha-256">{checksum}</hash>
                <url priority="2">{}</url>
                <url priority="1">payload.bin</url>
            </file>
        </metalink>"#,
        data.len(),
        "http://example.invalid/payload.bin"
    );
    let server = start_server(vec![
        (
            "/test.meta4".into(),
            "application/metalink4+xml".into(),
            metalink.into_bytes(),
        ),
        (
            "/payload.bin".into(),
            "application/octet-stream".into(),
            data.clone(),
        ),
    ]);
    let root = temp_dir("download");
    let url = format!("http://{}/test.meta4", server.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.protocol.as_deref(), Some("metalink"));
    assert_eq!(probe.suggested_filename.as_deref(), Some("payload.bin"));

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
                checksum: None,
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
    assert_eq!(fs::read(staging.join("payload.bin")).unwrap(), data);

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
