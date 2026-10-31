//! Local BitTorrent seed/leech contract test.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::checksum::sha256_bytes;
use aether_plugin_sdk::{
    current_platform_key, Budget, Checksum, ChecksumAlgorithm, Init, LogLevel, NetworkConfig,
    PluginEvent, PluginManifest, PluginProcess, TaskPaths, TaskSpec, API_VERSION,
};
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ManagedTorrent, Session, SessionOptions,
};

const FILE_SIZE: usize = 256 * 1024;
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(180);

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 233) as u8).collect()
}

fn other_data() -> Vec<u8> {
    (0..FILE_SIZE)
        .map(|index| (index % 197 + 31) as u8)
        .collect()
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
    init_with_network(data_dir, None)
}

/// Tests that need a fixed listen port pass it through `Init.network`
/// instead of `AETHER_BT_LISTEN_PORT`: the environment is process-wide and
/// parallel tests inheriting it would all bind the same port.
fn init_with_network(data_dir: &Path, network: Option<NetworkConfig>) -> Init {
    Init {
        network,
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

/// Build a single-file/dir torrent with librqbit 9's async API.
fn make_torrent(runtime: &tokio::runtime::Runtime, path: &Path, name: &str) -> (Vec<u8>, String) {
    let torrent = runtime
        .block_on(async {
            librqbit::create_torrent(
                path,
                CreateTorrentOptions {
                    name: Some(name),
                    trackers: Vec::new(),
                    piece_length: Some(16 * 1024),
                },
                &librqbit::spawn_utils::BlockingSpawner::new(1),
            )
            .await
        })
        .unwrap();
    let info_hash = torrent.info_hash().as_string();
    (torrent.as_bytes().unwrap().to_vec(), info_hash)
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Start a session that serves `torrent_bytes` from `output_folder` and
/// return its listen port.
fn session_options(port: Option<u16>) -> SessionOptions {
    SessionOptions {
        dht: None,
        listen: port.map(|port| librqbit::ListenerOptions {
            listen_addr: (std::net::Ipv4Addr::UNSPECIFIED, port).into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn start_seeder(
    runtime: &tokio::runtime::Runtime,
    output_folder: &Path,
    torrent_bytes: &[u8],
) -> (Arc<Session>, Arc<ManagedTorrent>, u16) {
    let peer_port = free_port();
    let seeder = runtime
        .block_on(Session::new_with_opts(
            output_folder.to_path_buf(),
            session_options(Some(peer_port)),
        ))
        .unwrap();
    assert_eq!(
        seeder.listen_addr().map(|addr| addr.port()),
        Some(peer_port)
    );
    let seed_handle = runtime.block_on(async {
        seeder
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.to_vec()),
                Some(AddTorrentOptions {
                    output_folder: Some(output_folder.to_string_lossy().into_owned()),
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
    (seeder, seed_handle, peer_port)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn start_task(
    plugin: &mut PluginProcess,
    url: String,
    staging: PathBuf,
    settings: serde_json::Value,
    expected: &[u8],
) {
    plugin
        .start_task(
            TaskSpec {
                task_id: "task-1".into(),
                url,
                filename: None,
                settings,
                plugin_state: None,
                expected_size: Some(expected.len() as u64),
                checksum: Some(Checksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: sha256_bytes(expected),
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

fn next_event(plugin: &mut PluginProcess, deadline: Instant) -> PluginEvent {
    assert!(
        Instant::now() < deadline,
        "task did not reach the expected state before the deadline"
    );
    plugin.recv_event(Duration::from_secs(2)).unwrap()
}

fn wait_for_metadata(plugin: &mut PluginProcess) {
    let deadline = Instant::now() + COMPLETE_TIMEOUT;
    loop {
        match next_event(plugin, deadline) {
            PluginEvent::Metadata { task_id, .. } if task_id == "task-1" => return,
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            _ => {}
        }
    }
}

fn wait_for_completed(
    plugin: &mut PluginProcess,
) -> (Vec<aether_plugin_sdk::FileInfo>, Option<serde_json::Value>) {
    let deadline = Instant::now() + COMPLETE_TIMEOUT;
    loop {
        match next_event(plugin, deadline) {
            PluginEvent::Completed {
                task_id,
                files,
                plugin_state,
            } if task_id == "task-1" => return (files, plugin_state),
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => {
                panic!("task failed: {error:?}")
            }
            PluginEvent::Progress {
                downloaded, total, ..
            } => {
                eprintln!("progress {downloaded}/{:?}", total);
            }
            PluginEvent::Detail { data, .. } => eprintln!("detail {data}"),
            other => eprintln!("event {other:?}"),
        }
    }
}

fn wait_for_failed(plugin: &mut PluginProcess) -> aether_plugin_sdk::PluginError {
    let deadline = Instant::now() + COMPLETE_TIMEOUT;
    loop {
        match next_event(plugin, deadline) {
            PluginEvent::Failed { task_id, error } if task_id == "task-1" => return error,
            PluginEvent::Completed { task_id, .. } if task_id == "task-1" => {
                panic!("task completed unexpectedly")
            }
            other => eprintln!("event {other:?}"),
        }
    }
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

    let runtime = runtime();
    let (torrent_bytes, _info_hash) = make_torrent(&runtime, &seed_file, "seed.bin");

    let (seeder, seed_handle, peer_port) = start_seeder(&runtime, &seed_root, &torrent_bytes);
    let server = serve_torrent(torrent_bytes);
    let root = temp_dir("download");
    let url = format!("http://{}/test.torrent", server.addr);
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        url,
        staging.clone(),
        serde_json::json!({ "initial_peers": [format!("127.0.0.1:{peer_port}")] }),
        &data,
    );
    let (files, _) = wait_for_completed(&mut plugin);
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

#[test]
fn downloads_from_a_magnet_with_a_file_path() {
    std::env::set_var("AETHER_BT_DISABLE_DHT", "1");
    let data = test_data();
    let seed_root = temp_dir("seed-magnet");
    let seed_file = seed_root.join("seed.bin");
    fs::write(&seed_file, &data).unwrap();

    let runtime = runtime();
    let (torrent_bytes, info_hash) = make_torrent(&runtime, &seed_file, "seed.bin");
    let magnet = format!("magnet:?xt=urn:btih:{info_hash}&dn=seed.bin");

    let (seeder, seed_handle, peer_port) = start_seeder(&runtime, &seed_root, &torrent_bytes);
    let root = temp_dir("download-magnet");
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        magnet,
        staging.clone(),
        serde_json::json!({
            "initial_peers": [format!("127.0.0.1:{peer_port}")],
            "bt_files": [0],
        }),
        &data,
    );
    let (files, _) = wait_for_completed(&mut plugin);
    assert_eq!(files.len(), 1);
    assert_eq!(
        fs::read(staging.join(&files[0].relative_path)).unwrap(),
        data
    );

    plugin.shutdown().unwrap();
    drop(seed_handle);
    drop(seeder);
    drop(runtime);
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(seed_root).ok();
}

#[test]
fn downloads_only_the_selected_files() {
    std::env::set_var("AETHER_BT_DISABLE_DHT", "1");
    let a = test_data();
    let b = other_data();
    let seed_root = temp_dir("seed-multi");
    let pack = seed_root.join("pack");
    fs::create_dir_all(&pack).unwrap();
    fs::write(pack.join("a.bin"), &a).unwrap();
    fs::write(pack.join("b.bin"), &b).unwrap();

    let runtime = runtime();
    let (torrent_bytes, _info_hash) = make_torrent(&runtime, &pack, "pack");

    let (seeder, seed_handle, peer_port) = start_seeder(&runtime, &pack, &torrent_bytes);
    let server = serve_torrent(torrent_bytes);
    let root = temp_dir("download-multi");
    let url = format!("http://{}/test.torrent", server.addr);
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        url,
        staging.clone(),
        serde_json::json!({
            "initial_peers": [format!("127.0.0.1:{peer_port}")],
            "bt_files": "a.bin",
        }),
        &a,
    );
    let (files, _) = wait_for_completed(&mut plugin);
    assert_eq!(files.len(), 1, "only the selected file may be reported");
    assert_eq!(files[0].relative_path, "a.bin");
    assert_eq!(files[0].size, a.len() as u64);
    assert_eq!(fs::read(staging.join("a.bin")).unwrap(), a);
    assert!(
        !staging.join("b.bin").exists(),
        "unselected files must not be left behind"
    );

    plugin.shutdown().unwrap();
    drop(seed_handle);
    drop(seeder);
    drop(runtime);
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(seed_root).ok();
}

#[test]
fn missing_selected_path_fails_the_task() {
    std::env::set_var("AETHER_BT_DISABLE_DHT", "1");
    let data = test_data();
    let seed_root = temp_dir("seed-missing");
    let seed_file = seed_root.join("seed.bin");
    fs::write(&seed_file, &data).unwrap();

    let runtime = runtime();
    let (torrent_bytes, _info_hash) = make_torrent(&runtime, &seed_file, "seed.bin");
    let server = serve_torrent(torrent_bytes);

    let root = temp_dir("download-missing");
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task(
        &mut plugin,
        format!("http://{}/test.torrent", server.addr),
        root.join("staging"),
        serde_json::json!({ "bt_files": "missing.bin" }),
        &data,
    );
    let error = wait_for_failed(&mut plugin);
    assert_eq!(error.kind, aether_plugin_sdk::ErrorKind::Protocol);
    assert!(error.message.contains("missing.bin"), "{}", error.message);

    plugin.shutdown().unwrap();
    drop(runtime);
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(seed_root).ok();
}

/// After the download the plugin seeds until `bt_seed_ratio` is reached. A
/// second session in this test plays the leecher and actually pulls pieces
/// from the plugin, so the ratio target is what ends the seeding phase.
#[test]
fn seeds_until_the_ratio_is_reached() {
    std::env::set_var("AETHER_BT_DISABLE_DHT", "1");
    let data = test_data();
    let seed_root = temp_dir("seed-ratio");
    let seed_file = seed_root.join("seed.bin");
    fs::write(&seed_file, &data).unwrap();

    let runtime = runtime();
    let (torrent_bytes, _info_hash) = make_torrent(&runtime, &seed_file, "seed.bin");

    let (seeder, seed_handle, peer_port) = start_seeder(&runtime, &seed_root, &torrent_bytes);
    let server = serve_torrent(torrent_bytes.clone());

    let plugin_port = free_port();
    let root = temp_dir("download-ratio");
    let network = NetworkConfig {
        bt_listen_port: Some(plugin_port),
        ..Default::default()
    };
    let mut plugin = PluginProcess::spawn(
        manifest(),
        &executable(),
        init_with_network(&root, Some(network)),
    )
    .unwrap();

    let staging = root.join("staging");
    start_task(
        &mut plugin,
        format!("http://{}/test.torrent", server.addr),
        staging,
        serde_json::json!({
            "initial_peers": [format!("127.0.0.1:{peer_port}")],
            "bt_seed_ratio": 0.5,
        }),
        &data,
    );
    wait_for_metadata(&mut plugin);

    // Leech the same torrent back from the plugin, which is what makes its
    // uploaded bytes (and therefore the ratio) grow.
    let leech_root = temp_dir("leech");
    let leecher = runtime
        .block_on(Session::new_with_opts(
            leech_root.clone(),
            session_options(None),
        ))
        .unwrap();
    let leech_handle = runtime.block_on(async {
        leecher
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    output_folder: Some(leech_root.to_string_lossy().into_owned()),
                    overwrite: true,
                    initial_peers: Some(vec![format!("127.0.0.1:{plugin_port}").parse().unwrap()]),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap()
    });
    runtime
        .block_on(leech_handle.wait_until_initialized())
        .unwrap();

    let (files, plugin_state) = wait_for_completed(&mut plugin);
    assert_eq!(files.len(), 1);
    assert_eq!(
        fs::read(root.join("staging").join(&files[0].relative_path)).unwrap(),
        data
    );

    let state = plugin_state.expect("completed carries the seed report");
    let ratio = state["ratio"].as_f64().expect("ratio in plugin_state");
    let uploaded = state["uploaded_bytes"]
        .as_u64()
        .expect("uploaded_bytes in plugin_state");
    assert!(uploaded > 0, "the plugin never uploaded anything");
    assert!(ratio >= 0.5, "seeding stopped before the ratio: {ratio}");

    plugin.shutdown().unwrap();
    drop(leech_handle);
    drop(leecher);
    drop(seed_handle);
    drop(seeder);
    drop(runtime);
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(seed_root).ok();
    fs::remove_dir_all(leech_root).ok();
}
