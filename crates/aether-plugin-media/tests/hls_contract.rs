//! HLS plugin tests with a local playlist/segment server.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes::Aes128;
use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
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
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let (content_type, body) = routes
        .get(&path)
        .cloned()
        .unwrap_or_else(|| ("text/plain".into(), b"not found".to_vec()));
    let status = if routes.contains_key(&path) {
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-media"))
}

fn start_task(plugin: &mut PluginProcess, url: String, staging: PathBuf, expected: &[u8]) {
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
                    value: sha256_bytes(expected),
                }),
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
