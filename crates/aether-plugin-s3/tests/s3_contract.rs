//! S3 plugin test against a local unsigned object server.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};

const FILE_SIZE: usize = 128 * 1024;
const MULTIPART_SIZE: usize = 6 * 1024 * 1024 + 4096;
const RESUME_SIZE: usize = 10 * 1024 * 1024 + 13;
const CANCEL_SIZE: usize = 24 * 1024 * 1024;

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 223) as u8).collect()
}

fn patterned(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

struct TestServer {
    addr: SocketAddr,
    authorizations: Arc<Mutex<Vec<String>>>,
    /// `(start, end_exclusive)` for every ranged GET.
    range_requests: Arc<Mutex<Vec<(usize, usize)>>>,
    requests: Arc<AtomicUsize>,
}

fn start_server(data: Vec<u8>) -> TestServer {
    start_server_with(data, Duration::ZERO)
}

fn start_server_with(data: Vec<u8>, delay: Duration) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    let authorizations = Arc::new(Mutex::new(Vec::new()));
    let range_requests = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    let auths = authorizations.clone();
    let ranges = range_requests.clone();
    let count = requests.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = data.clone();
            let auths = auths.clone();
            let ranges = ranges.clone();
            let count = count.clone();
            thread::spawn(move || handle(stream, &data, &auths, &ranges, &count, delay));
        }
    });
    TestServer {
        addr,
        authorizations,
        range_requests,
        requests,
    }
}

fn handle(
    mut stream: TcpStream,
    data: &[u8],
    auths: &Mutex<Vec<String>>,
    ranges: &Mutex<Vec<(usize, usize)>>,
    requests: &AtomicUsize,
    delay: Duration,
) {
    requests.fetch_add(1, Ordering::SeqCst);
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
        if start > end || start >= data.len() {
            ("416 Range Not Satisfiable", Vec::new(), None)
        } else {
            ranges.lock().unwrap().push((start, end + 1));
            (
                "206 Partial Content",
                data[start..=end].to_vec(),
                Some(format!("bytes {start}-{end}/{}", data.len())),
            )
        }
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
        if delay.is_zero() {
            let _ = stream.write_all(&body);
        } else {
            // Slow drip so a cancel can land while the body is in flight.
            for chunk in body.chunks(32 * 1024) {
                if stream.write_all(chunk).is_err() {
                    return;
                }
                thread::sleep(delay);
            }
        }
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
        network: None,
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

fn settings(addr: SocketAddr) -> serde_json::Value {
    serde_json::json!({
        "access_key": "AKIDEXAMPLE",
        "secret_key": "secret",
        "region": "us-east-1",
        "endpoint": format!("http://{addr}"),
        "path_style": true
    })
}

fn task_spec(task_id: &str, url: &str, addr: SocketAddr, checksum: Option<&[u8]>) -> TaskSpec {
    TaskSpec {
        task_id: task_id.into(),
        url: url.into(),
        filename: None,
        settings: settings(addr),
        plugin_state: None,
        expected_size: None,
        checksum: checksum.map(|data| Checksum {
            algorithm: ChecksumAlgorithm::Sha256,
            value: sha256_bytes(data),
        }),
        proxy: None,
    }
}

fn task_paths(staging: &Path) -> TaskPaths {
    TaskPaths {
        staging: staging.to_path_buf(),
        final_dir: PathBuf::from("/tmp/aether-final"),
    }
}

fn wait_for_completion(plugin: &mut PluginProcess, task_id: &str) {
    for _ in 0..200 {
        match plugin.recv_event(Duration::from_secs(2)) {
            Ok(PluginEvent::Completed { task_id: id, .. }) if id == task_id => return,
            Ok(PluginEvent::Failed { task_id: id, error }) if id == task_id => {
                panic!("task failed: {error:?}")
            }
            Ok(_) => {}
            Err(error) => panic!("event error: {error:?}"),
        }
    }
    panic!("task {task_id} did not complete");
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
            task_spec("task-1", &url, server.addr, Some(&data)),
            task_paths(&staging),
            Budget::default(),
        )
        .unwrap();

    wait_for_completion(&mut plugin, "task-1");
    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let auths = server.authorizations.lock().unwrap();
    assert!(!auths.is_empty());
    assert!(auths
        .iter()
        .all(|auth| auth.contains("Credential=AKIDEXAMPLE")));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn downloads_large_object_with_concurrent_ranges() {
    let data = patterned(MULTIPART_SIZE);
    let server = start_server(data.clone());
    let root = temp_dir("multipart");
    let url = "s3://bucket/path/file.bin";

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let staging = root.join("staging");
    plugin
        .start_task(
            task_spec("task-multipart", url, server.addr, Some(&data)),
            task_paths(&staging),
            Budget {
                connections: 4,
                ..Default::default()
            },
        )
        .unwrap();

    wait_for_completion(&mut plugin, "task-multipart");
    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);

    let ranges = server.range_requests.lock().unwrap().clone();
    assert!(
        ranges.len() >= 2,
        "expected multiple ranged GETs, got {ranges:?}"
    );
    let mut sorted = ranges.clone();
    sorted.sort_unstable();
    let mut expected_start = 0;
    for (start, end) in &sorted {
        assert_eq!(*start, expected_start, "range gap in {sorted:?}");
        expected_start = *end;
    }
    assert_eq!(
        expected_start,
        data.len(),
        "ranges did not cover the object"
    );
    assert!(server.requests.load(Ordering::SeqCst) > ranges.len());

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn resume_requests_only_missing_ranges() {
    let data = patterned(RESUME_SIZE);
    let server = start_server(data.clone());
    let root = temp_dir("resume");
    let url = "s3://bucket/path/file.bin";
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    let prefix = 2 * 1024 * 1024 + 7;
    fs::write(staging.join("file.bin"), &data[..prefix]).unwrap();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    plugin
        .start_task(
            task_spec("task-resume", url, server.addr, Some(&data)),
            task_paths(&staging),
            Budget {
                connections: 4,
                ..Default::default()
            },
        )
        .unwrap();

    wait_for_completion(&mut plugin, "task-resume");
    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);

    let ranges = server.range_requests.lock().unwrap().clone();
    assert!(!ranges.is_empty(), "resume sent no ranged GETs");
    assert!(
        ranges.iter().all(|(start, _)| *start >= prefix),
        "resume re-requested completed bytes: {ranges:?}"
    );
    let covered: usize = ranges.iter().map(|(start, end)| end - start).sum();
    assert_eq!(covered, data.len() - prefix, "resume coverage: {ranges:?}");

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn cancel_stops_ranged_downloads_without_completing() {
    let data = patterned(CANCEL_SIZE);
    let server = start_server_with(data.clone(), Duration::from_millis(4));
    let root = temp_dir("cancel");
    let url = "s3://bucket/path/file.bin";
    let staging = root.join("staging");

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    plugin
        .start_task(
            task_spec("task-cancel", url, server.addr, None),
            task_paths(&staging),
            Budget {
                connections: 4,
                ..Default::default()
            },
        )
        .unwrap();

    let mut saw_progress = false;
    let progress_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < progress_deadline {
        match plugin.recv_event(Duration::from_millis(200)) {
            Ok(PluginEvent::Progress { task_id, .. }) if task_id == "task-cancel" => {
                saw_progress = true;
                break;
            }
            Ok(_) => {}
            Err(error) => panic!("event error: {error:?}"),
        }
    }
    assert!(saw_progress, "no progress before cancel");
    plugin.cancel("task-cancel").unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut completed = false;
    while Instant::now() < deadline {
        match plugin.recv_event(Duration::from_millis(200)) {
            Ok(PluginEvent::Completed { task_id, .. }) if task_id == "task-cancel" => {
                completed = true;
                break;
            }
            Ok(PluginEvent::Failed { task_id, error }) if task_id == "task-cancel" => {
                panic!("canceled task reported failure: {error:?}")
            }
            Ok(_) | Err(_) => {}
        }
    }
    assert!(!completed, "task completed after cancel");
    assert!(staging.join("file.bin").exists());

    let after_cancel = server.requests.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        after_cancel,
        "canceled task kept issuing requests"
    );

    // The plugin loop is still healthy after the worker stopped.
    let probe = plugin.probe(url).unwrap();
    assert!(probe.supported);

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
