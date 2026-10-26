//! Local BitTorrent seed/leech contract test.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, PluginEvent,
    PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};
use librqbit::{AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions};

const FILE_SIZE: usize = 256 * 1024;

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 233) as u8).collect()
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("aether-bt-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct TorrentServer {
    addr: SocketAddr,
}

fn serve_torrent(bytes: Vec<u8>) -> TorrentServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let bytes = bytes.clone();
            thread::spawn(move || serve_one(stream, &bytes));
        }
    });
    TorrentServer { addr }
}

fn serve_one(mut stream: TcpStream, bytes: &[u8]) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 512];
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
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(bytes);
    let _ = stream.flush();
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "bt",
        "name": "BitTorrent plugin",
        "version": "0.1.0",
        "api": API_VERSION,
        "protocols": ["bt", "magnet"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-bt"))
}

fn start_task(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    peers: Vec<String>,
    expected: &[u8],
) {
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings: serde_json::json!({ "initial_peers": peers }),
                plugin_state: None,
                expected_size: Some(expected.len() as u64),
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

fn wait_for_completed(plugin: &mut PluginProcess) -> Vec<aether_plugin_sdk::FileInfo> {
    for _ in 0..300 {
        match plugin.recv_event(Duration::from_secs(2)).unwrap() {
            PluginEvent::Completed { task_id, files, .. } if task_id == "task-1" => return files,
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            PluginEvent::Progress {
                downloaded, total, ..
            } => {
                eprintln!("progress {downloaded}/{:?}", total);
            }
            other => eprintln!("event {other:?}"),
        }
    }
    panic!("task did not complete");
}

#[test]
fn manifest_file_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json");
    let manifest = PluginManifest::from_path(&path).unwrap();
    assert_eq!(manifest.id, "bt");
    manifest.validate().unwrap();
}

#[test]
fn downloads_from_a_local_seed() {
    std::env::set_var("AETHER_BT_DISABLE_DHT", "1");
    let data = test_data();
    let seed_root = temp_dir("seed");
    let seed_file = seed_root.join("seed.bin");
    fs::write(&seed_file, &data).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let torrent = runtime
        .block_on(librqbit::create_torrent(
            &seed_file,
            CreateTorrentOptions {
                name: Some("seed.bin"),
                piece_length: Some(16 * 1024),
            },
        ))
        .unwrap();
    let torrent_bytes = torrent.as_bytes().unwrap().to_vec();

    let probe_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer_port = probe_listener.local_addr().unwrap().port();
    drop(probe_listener);
    let seeder = runtime
        .block_on(Session::new_with_opts(
            seed_root.clone(),
            SessionOptions {
                disable_dht: true,
                disable_dht_persistence: true,
                listen_port_range: Some(peer_port..peer_port + 1),
                ..Default::default()
            },
        ))
        .unwrap();
    assert_eq!(seeder.tcp_listen_port(), Some(peer_port));
    let seed_handle = runtime.block_on(async {
        seeder
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    output_folder: Some(seed_root.to_string_lossy().into_owned()),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap()
    });
    runtime
        .block_on(seed_handle.wait_until_initialized())
        .unwrap();
    let seed_stats = seed_handle.stats();
    eprintln!(
        "seeder port={peer_port} state={:?} finished={} progress={} total={}",
        seed_stats.state, seed_stats.finished, seed_stats.progress_bytes, seed_stats.total_bytes
    );

    let server = serve_torrent(torrent_bytes);
    let root = temp_dir("download");
    let url = format!("http://{}/test.torrent", server.addr);
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        url,
        staging.clone(),
        vec![format!("127.0.0.1:{peer_port}")],
        &data,
    );
    let files = wait_for_completed(&mut plugin);
    assert_eq!(files.len(), 1);
    let downloaded = staging.join(&files[0].relative_path);
    assert_eq!(fs::read(downloaded).unwrap(), data);

    plugin.shutdown().unwrap();
    drop(seed_handle);
    drop(seeder);
    drop(runtime);
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(seed_root).ok();
}
