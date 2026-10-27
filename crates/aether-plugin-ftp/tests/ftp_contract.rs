//! End-to-end FTP plugin tests against a minimal local FTP server.

use std::fs;
use std::io::{BufRead, BufReader, Write};
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

const PLUGIN_ID: &str = "ftp";
const PLUGIN_VERSION: &str = "0.1.0";
const FILE_SIZE: usize = 256 * 1024;

struct TestServer {
    addr: SocketAddr,
    rest: Arc<Mutex<Option<u64>>>,
    commands: Arc<Mutex<Vec<String>>>,
}

fn test_data() -> Vec<u8> {
    (0..FILE_SIZE).map(|index| (index % 241) as u8).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    sha256_bytes(data)
}

fn start_server(data: Vec<u8>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data = Arc::new(data);
    let rest = Arc::new(Mutex::new(None));
    let commands = Arc::new(Mutex::new(Vec::new()));

    let thread_data = data.clone();
    let thread_rest = rest.clone();
    let thread_commands = commands.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let data = thread_data.clone();
            let rest = thread_rest.clone();
            let commands = thread_commands.clone();
            thread::spawn(move || handle_control(stream, &data, &rest, &commands));
        }
    });

    TestServer {
        addr,
        rest,
        commands,
    }
}

fn handle_control(
    mut stream: TcpStream,
    data: &[u8],
    rest: &Mutex<Option<u64>>,
    commands: &Mutex<Vec<String>>,
) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut pasv: Option<TcpListener> = None;

    let _ = stream.write_all(b"220 Aether test FTP\r\n");
    let _ = stream.flush();

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let command = line.trim_end().to_string();
        if command.is_empty() {
            continue;
        }
        commands.lock().unwrap().push(command.clone());
        let mut parts = command.split_whitespace();
        let verb = parts.next().unwrap_or_default().to_ascii_uppercase();
        let argument = parts.collect::<Vec<_>>().join(" ");

        match verb.as_str() {
            "USER" => reply(&mut stream, "331 Password required"),
            "PASS" => reply(&mut stream, "230 Logged in"),
            "TYPE" | "MODE" | "STRU" | "OPTS" | "NOOP" => reply(&mut stream, "200 OK"),
            "FEAT" => {
                let _ = stream.write_all(b"211-Features:\r\n SIZE\r\n REST STREAM\r\n211 End\r\n");
                let _ = stream.flush();
            }
            "PWD" => reply(&mut stream, "257 \"/\""),
            "SIZE" => reply(&mut stream, &format!("213 {}", data.len())),
            "REST" => {
                let offset = argument.parse().unwrap_or(0);
                *rest.lock().unwrap() = Some(offset);
                reply(&mut stream, "350 Restarting at offset");
            }
            "PASV" => match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => {
                    let port = listener.local_addr().unwrap().port();
                    let response = format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{}).",
                        port / 256,
                        port % 256
                    );
                    reply(&mut stream, &response);
                    pasv = Some(listener);
                }
                Err(_) => reply(&mut stream, "425 Can't open data connection"),
            },
            "RETR" => {
                reply(&mut stream, "150 Opening binary mode data connection");
                if let Some(listener) = pasv.take() {
                    if let Ok((mut data_stream, _)) = listener.accept() {
                        let offset = rest.lock().unwrap().unwrap_or(0) as usize;
                        let start = offset.min(data.len());
                        let _ = data_stream.write_all(&data[start..]);
                        let _ = data_stream.flush();
                    }
                }
                reply(&mut stream, "226 Transfer complete");
            }
            "QUIT" => {
                reply(&mut stream, "221 Goodbye");
                break;
            }
            _ => reply(&mut stream, "200 OK"),
        }
    }
}

fn reply(stream: &mut TcpStream, message: &str) {
    let _ = stream.write_all(format!("{message}\r\n").as_bytes());
    let _ = stream.flush();
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("aether-ftp-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": PLUGIN_ID,
        "name": "FTP plugin",
        "version": PLUGIN_VERSION,
        "api": API_VERSION,
        "protocols": ["ftp"],
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
    PathBuf::from(env!("CARGO_BIN_EXE_aether-plugin-ftp"))
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
    manifest.validate().unwrap();
}

#[test]
fn downloads_and_verifies_ftp_file() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("download");
    let url = format!(
        "ftp://user:pass@127.0.0.1:{}/pub/file.bin",
        server.addr.port()
    );

    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    let probe = plugin.probe(&url).unwrap();
    assert!(probe.supported);
    assert_eq!(
        probe.metadata.as_ref().and_then(|m| m.size),
        Some(FILE_SIZE as u64)
    );
    assert_eq!(probe.suggested_filename.as_deref(), Some("file.bin"));

    let staging = root.join("staging");
    start_task(&mut plugin, url, staging.clone(), sha256_hex(&data));
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    let commands = server.commands.lock().unwrap().clone();
    assert!(commands.iter().any(|command| command.starts_with("TYPE I")));
    assert!(commands.iter().any(|command| command.starts_with("PASV")));
    assert!(commands.iter().any(|command| command.starts_with("RETR ")));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}

#[test]
fn resumes_with_rest_from_existing_file_length() {
    let data = test_data();
    let server = start_server(data.clone());
    let root = temp_dir("resume");
    let staging = root.join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(&staging.join("file.bin"), &data[..100]).unwrap();

    let url = format!(
        "ftp://user:pass@127.0.0.1:{}/pub/file.bin",
        server.addr.port()
    );
    let mut plugin = PluginProcess::spawn(manifest(), &executable(), init(&root)).unwrap();
    start_task(&mut plugin, url, staging.clone(), sha256_hex(&data));
    wait_for_completed(&mut plugin);

    assert_eq!(fs::read(staging.join("file.bin")).unwrap(), data);
    assert_eq!(*server.rest.lock().unwrap(), Some(100));

    plugin.shutdown().unwrap();
    fs::remove_dir_all(root).ok();
}
