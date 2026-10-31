//! HLS plugin tests with a local playlist/segment server.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes::Aes128;
use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, ErrorKind, Init, LogLevel,
    PluginEvent, PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};

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
    let Some(path) = read_request_path(&mut stream) else {
        return;
    };
    let (content_type, body) = routes
        .get(&path)
        .cloned()
        .unwrap_or_else(|| ("text/plain".into(), b"not found".to_vec()));
    let status = if routes.contains_key(&path) {
        "200 OK"
    } else {
        "404 Not Found"
    };
    write_response(&mut stream, status, &content_type, &body);
}

fn read_request_path(stream: &mut TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(read) => read,
        };
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let text = String::from_utf8_lossy(&request);
    text.lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .split('?')
        .next()
        .map(str::to_string)
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// A server whose `/live.m3u8` response changes on every request: version N
/// is returned for the Nth hit, and the last version is repeated afterwards.
struct DynamicServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    playlist_hits: Arc<AtomicUsize>,
}

impl DynamicServer {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn segment_requests(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|path| path != "/live.m3u8")
            .collect()
    }

    fn playlist_hits(&self) -> usize {
        self.playlist_hits.load(Ordering::SeqCst)
    }
}

fn start_dynamic_server(versions: Vec<String>, segments: Vec<(String, Vec<u8>)>) -> DynamicServer {
    assert!(!versions.is_empty(), "need at least one playlist version");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let versions = Arc::new(versions);
    let segments: Arc<HashMap<String, Vec<u8>>> = Arc::new(segments.into_iter().collect());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let playlist_hits = Arc::new(AtomicUsize::new(0));

    let thread_versions = versions.clone();
    let thread_segments = segments.clone();
    let thread_requests = requests.clone();
    let thread_hits = playlist_hits.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let versions = thread_versions.clone();
            let segments = thread_segments.clone();
            let requests = thread_requests.clone();
            let hits = thread_hits.clone();
            thread::spawn(move || {
                let Some(path) = read_request_path(&mut stream) else {
                    return;
                };
                requests.lock().unwrap().push(path.clone());
                if path == "/live.m3u8" {
                    let index = hits.fetch_add(1, Ordering::SeqCst);
                    let version = &versions[index.min(versions.len() - 1)];
                    write_response(
                        &mut stream,
                        "200 OK",
                        "application/vnd.apple.mpegurl",
                        version.as_bytes(),
                    );
                } else if let Some(body) = segments.get(&path) {
                    write_response(&mut stream, "200 OK", "video/mp2t", body);
                } else {
                    write_response(&mut stream, "404 Not Found", "text/plain", b"not found");
                }
            });
        }
    });

    DynamicServer {
        addr,
        requests,
        playlist_hits,
    }
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-media-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "media",
        "name": "Media plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["hls"],
        "capabilities": ["network", "filesystem:task", "external-process"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-media"))
}

fn start_task(plugin: &mut PluginProcess, url: String, staging: PathBuf, expected: &[u8]) {
    start_task_with(plugin, url, staging, serde_json::json!({}), Some(expected));
}

fn start_task_with(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    settings: serde_json::Value,
    expected: Option<&[u8]>,
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
                checksum: expected.map(|bytes| Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: sha256_bytes(bytes),
                }),
                proxy: None,
            },
            TaskPaths {
                staging,
                final_dir: PathBuf::from("/tmp/aether-final"),
            },
            Budget::default(),
        )
        .unwrap();
}

fn wait_for_completed(plugin: &mut PluginProcess) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "task did not complete within 20 seconds"
        );
        let timeout = (deadline - Instant::now()).min(Duration::from_secs(2));
        match plugin.recv_event(timeout).unwrap() {
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => return,
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            _ => {}
        }
    }
}

fn media_playlist(segments: &[&str], extra: &str) -> String {
    let mut text = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    text.push_str(extra);
    for segment in segments {
        text.push_str("#EXTINF:2.0,\n");
        text.push_str(segment);
        text.push('\n');
    }
    text.push_str("#EXT-X-ENDLIST\n");
    text
}

fn live_playlist(segments: &[&str], endlist: bool) -> String {
    let mut text = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for segment in segments {
        text.push_str("#EXTINF:2.0,\n");
        text.push_str(segment);
        text.push('\n');
    }
    if endlist {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    text
}

fn encrypt(data: &[u8], key: &[u8], iv: &[u8; 16]) -> Vec<u8> {
    let mut buffer = data.to_vec();
    let length = buffer.len();
    buffer.resize(length + 16, 0);
    let encryptor = cbc::Encryptor::<Aes128>::new_from_slices(key, iv).unwrap();
    encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, length)
        .unwrap()
        .to_vec()
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    assert_eq!(manifest.id, "media");
    manifest.validate().unwrap();
}

#[test]
fn downloads_master_playlist_segments() {
    let seg0 = vec![1_u8; 1000];
    let seg1 = vec![2_u8; 1200];
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nmedia.m3u8\n";
    let media = media_playlist(&["seg0.ts", "seg1.ts"], "");
    let server = start_server(vec![
        (
            "/master.m3u8".into(),
            "application/vnd.apple.mpegurl".into(),
            master.as_bytes().to_vec(),
        ),
        (
            "/media.m3u8".into(),
            "application/vnd.apple.mpegurl".into(),
            media.as_bytes().to_vec(),
        ),
        ("/seg0.ts".into(), "video/mp2t".into(), seg0.clone()),
        ("/seg1.ts".into(), "video/mp2t".into(), seg1.clone()),
    ]);
    let root = temp_dir("plain");
    let url = format!("http://{}/master.m3u8", server.addr);

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("media.ts"));

    let staging = root.join("staging");
    let expected = [seg0.clone(), seg1.clone()].concat();
    start_task(&mut plugin, url, staging.clone(), &expected);
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("media.ts")).unwrap(), expected);
    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn decrypts_aes128_segments() {
    let key = [0x11_u8; 16];
    let iv = [0x22_u8; 16];
    let plain0 = vec![3_u8; 640];
    let plain1 = vec![4_u8; 512];
    let seg0 = encrypt(&plain0, &key, &iv);
    let seg1 = encrypt(&plain1, &key, &iv);
    let media = media_playlist(
        &["e0.ts", "e1.ts"],
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x22222222222222222222222222222222\n",
    );
    let server = start_server(vec![
        (
            "/enc.m3u8".into(),
            "application/vnd.apple.mpegurl".into(),
            media.as_bytes().to_vec(),
        ),
        (
            "/key.bin".into(),
            "application/octet-stream".into(),
            key.to_vec(),
        ),
        ("/e0.ts".into(), "video/mp2t".into(), seg0),
        ("/e1.ts".into(), "video/mp2t".into(), seg1),
    ]);
    let root = temp_dir("aes");
    let url = format!("http://{}/enc.m3u8", server.addr);
    let expected = [plain0, plain1].concat();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let staging = root.join("staging");
    start_task(&mut plugin, url, staging.clone(), &expected);
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("enc.ts")).unwrap(), expected);
    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn downloads_dash_segment_template() {
    let init_segment = vec![9_u8; 100];
    let segments = [vec![1_u8; 300], vec![2_u8; 300], vec![3_u8; 300]];
    let mpd = r#"<MPD type="static" mediaPresentationDuration="PT6S">
        <Period>
            <AdaptationSet>
                <SegmentTemplate timescale="1" duration="2" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
                <Representation id="v1" bandwidth="1000"/>
            </AdaptationSet>
        </Period>
    </MPD>"#;

    let mut routes = vec![
        (
            "/manifest.mpd".into(),
            "application/dash+xml".into(),
            mpd.as_bytes().to_vec(),
        ),
        ("/init.mp4".into(), "video/mp4".into(), init_segment.clone()),
    ];
    for (index, segment) in segments.iter().enumerate() {
        routes.push((
            format!("/seg-{}.m4s", index + 1),
            "video/iso.segment".into(),
            segment.clone(),
        ));
    }
    let server = start_server(routes);
    let root = temp_dir("dash");
    let url = format!("http://{}/manifest.mpd", server.addr);
    let expected = [init_segment, segments.concat()].concat();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("manifest.mp4"));
    assert_eq!(probe.protocol.as_deref(), Some("dash"));

    let staging = root.join("staging");
    start_task(&mut plugin, url, staging.clone(), &expected);
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("manifest.mp4")).unwrap(), expected);
    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn downloads_live_segments_once_until_endlist() {
    let bodies: Vec<(&str, Vec<u8>)> = vec![
        ("/live0.ts", vec![10_u8; 800]),
        ("/live1.ts", vec![11_u8; 400]),
        ("/live2.ts", vec![12_u8; 600]),
        ("/live3.ts", vec![13_u8; 200]),
    ];
    let versions = vec![
        // Probe consumes version 0.
        live_playlist(&["live0.ts", "live1.ts"], false),
        live_playlist(&["live0.ts", "live1.ts"], false),
        live_playlist(&["live0.ts", "live1.ts", "live2.ts"], false),
        live_playlist(&["live0.ts", "live1.ts", "live2.ts", "live3.ts"], true),
    ];
    let server = start_dynamic_server(
        versions,
        bodies
            .iter()
            .map(|(path, body)| ((*path).to_string(), body.clone()))
            .collect(),
    );
    let root = temp_dir("live");
    let url = format!("http://{}/live.m3u8", server.addr);
    let expected = bodies
        .iter()
        .flat_map(|(_, body)| body.clone())
        .collect::<Vec<u8>>();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(probe.suggested_filename.as_deref(), Some("live.ts"));

    let staging = root.join("staging");
    start_task_with(
        &mut plugin,
        url,
        staging.clone(),
        serde_json::json!({
            "hls_live_interval_ms": 500,
            "hls_live_idle_polls": 2,
        }),
        Some(&expected),
    );
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("live.ts")).unwrap(), expected);
    assert_eq!(
        server.segment_requests(),
        vec![
            "/live0.ts".to_string(),
            "/live1.ts".to_string(),
            "/live2.ts".to_string(),
            "/live3.ts".to_string(),
        ],
        "every live segment must be downloaded once, in order"
    );
    assert!(
        server.playlist_hits() >= 4,
        "playlist should be refreshed until ENDLIST, got {} requests",
        server.playlist_hits()
    );

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn canceling_live_task_stops_polling() {
    let bodies = vec![
        ("/c0.ts".to_string(), vec![21_u8; 300]),
        ("/c1.ts".to_string(), vec![22_u8; 300]),
    ];
    // One version only: it is repeated forever, so only cancellation can
    // stop the stream.
    let server = start_dynamic_server(
        vec![live_playlist(&["c0.ts", "c1.ts"], false)],
        bodies.clone(),
    );
    let root = temp_dir("live-cancel");
    let url = format!("http://{}/live.m3u8", server.addr);
    let expected = [bodies[0].1.clone(), bodies[1].1.clone()].concat();

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let staging = root.join("staging");
    start_task_with(
        &mut plugin,
        url,
        staging.clone(),
        serde_json::json!({ "hls_live_interval_ms": 500 }),
        None,
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "live task did not report progress"
        );
        let timeout = (deadline - Instant::now()).min(Duration::from_secs(2));
        match plugin.recv_event(timeout).unwrap() {
            PluginEvent::Progress { downloaded, .. } if downloaded > 0 => break,
            PluginEvent::Failed { error, .. } => panic!("live task failed early: {error:?}"),
            _ => {}
        }
    }

    plugin.cancel("task-1").unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "canceled live task did not report a terminal event"
        );
        let timeout = (deadline - Instant::now()).min(Duration::from_secs(2));
        match plugin.recv_event(timeout).unwrap() {
            PluginEvent::Failed { task_id, error }
                if task_id == "task-1" && error.kind == ErrorKind::Canceled =>
            {
                break;
            }
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => {
                panic!("canceled live task completed")
            }
            _ => {}
        }
    }

    assert_eq!(
        fs::read(staging.join("live.ts")).unwrap(),
        expected,
        "segments downloaded before cancel should stay in the output file"
    );

    let requests = server.requests().len();
    thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        server.requests().len(),
        requests,
        "plugin kept polling after the live task was canceled"
    );

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
