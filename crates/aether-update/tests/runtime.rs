//! Installing a signed runtime bundle into user space, end to end against a
//! local HTTP server: the layout this test asserts is exactly what
//! `aether-launcher` re-verifies before it starts a program.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_update::UpdateClient;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const PROGRAM: &[u8] = b"new-aether-binary";
const DAEMON: &[u8] = b"new-daemon-binary";

struct TestServer {
    addr: SocketAddr,
}

fn start_server(build_routes: impl FnOnce(SocketAddr) -> Vec<(String, Vec<u8>)>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(build_routes(addr).into_iter().collect::<HashMap<_, _>>());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let routes = routes.clone();
            thread::spawn(move || handle(stream, &routes));
        }
    });
    TestServer { addr }
}

fn handle(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
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
    let body = routes
        .get(path)
        .cloned()
        .unwrap_or_else(|| b"not found".to_vec());
    let status = if routes.contains_key(path) {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        "aether-runtime-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn routes(addr: SocketAddr, version: &str) -> Vec<(String, Vec<u8>)> {
    let signing = SigningKey::from_bytes(&[7_u8; 32]);
    let manifest = serde_json::json!({
        "version": version,
        "channel": "stable",
        "assets": {
            "linux-x86_64": {
                "url": format!("http://{addr}/aether-{version}-linux-x86_64"),
                "sha256": hex(&Sha256::digest(PROGRAM)),
                "size": PROGRAM.len(),
                "kind": "binary"
            }
        },
        "runtimes": {
            "linux-x86_64": {
                "files": {
                    "aether": {
                        "url": format!("http://{addr}/aether-{version}-linux-x86_64"),
                        "sha256": hex(&Sha256::digest(PROGRAM)),
                        "size": PROGRAM.len(),
                        "kind": "binary"
                    },
                    "aether-daemon": {
                        "url": format!("http://{addr}/aether-daemon-{version}-linux-x86_64"),
                        "sha256": hex(&Sha256::digest(DAEMON)),
                        "size": DAEMON.len(),
                        "kind": "binary"
                    }
                }
            }
        }
    })
    .to_string();
    let manifest_bytes = manifest.into_bytes();
    let signature = signing.sign(&manifest_bytes).to_bytes().to_vec();
    vec![
        ("/update.json".into(), manifest_bytes),
        (
            "/update.json.sig".into(),
            base64::engine::general_purpose::STANDARD
                .encode(signature)
                .into_bytes(),
        ),
        (format!("/aether-{version}-linux-x86_64"), PROGRAM.to_vec()),
        (
            format!("/aether-daemon-{version}-linux-x86_64"),
            DAEMON.to_vec(),
        ),
    ]
}

fn client(addr: SocketAddr, state_dir: &Path, current: &str) -> UpdateClient {
    UpdateClient::new(
        &format!("http://{addr}/update.json"),
        &base64::engine::general_purpose::STANDARD.encode(
            SigningKey::from_bytes(&[7_u8; 32])
                .verifying_key()
                .to_bytes(),
        ),
        current,
        "linux-x86_64",
        state_dir,
    )
    .unwrap()
}

#[test]
fn installs_verified_runtime_bundle_for_the_launcher() {
    let server = start_server(|addr| routes(addr, "0.7.0"));
    let root = temp_dir("root");
    let state = temp_dir("state");
    let client = client(server.addr, &state, "0.6.5");

    let signed = client
        .check_signed()
        .unwrap()
        .expect("newer release is offered");
    let installed = client.install_runtime(&signed, &root).unwrap();

    assert_eq!(installed, root.join("versions/0.7.0"));
    assert_eq!(fs::read(installed.join("aether")).unwrap(), PROGRAM);
    assert_eq!(fs::read(installed.join("aether-daemon")).unwrap(), DAEMON);
    assert_eq!(
        fs::read_to_string(installed.join("version"))
            .unwrap()
            .trim(),
        "0.7.0"
    );
    // The proof the launcher re-checks travels with the copy.
    assert_eq!(
        fs::read(installed.join("update.json")).unwrap(),
        signed.bytes,
        "signed manifest is stored verbatim"
    );
    assert_eq!(
        fs::read(installed.join("update.json.sig")).unwrap(),
        signed.signature
    );
    // `current` is a relative symlink so the whole tree can move with $HOME.
    let link = root.join("versions/current");
    assert_eq!(
        fs::read_link(&link).unwrap(),
        PathBuf::from("0.7.0"),
        "current points at the installed version"
    );
    // Nothing from staging survives.
    let leftovers: Vec<_> = fs::read_dir(root.join("versions"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "stale staging: {leftovers:?}");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn refuses_a_runtime_bundle_with_unsafe_names() {
    let server = start_server(|addr| {
        let mut entries = routes(addr, "0.7.0");
        let signing = SigningKey::from_bytes(&[7_u8; 32]);
        let manifest = serde_json::json!({
            "version": "0.7.0",
            "assets": {
                "linux-x86_64": {
                    "url": format!("http://{addr}/aether-0.7.0-linux-x86_64"),
                    "sha256": hex(&Sha256::digest(PROGRAM)),
                    "size": PROGRAM.len(),
                    "kind": "binary"
                }
            },
            "runtimes": {
                "linux-x86_64": {
                    "files": {
                        "../escape": {
                            "url": format!("http://{addr}/aether-0.7.0-linux-x86_64"),
                            "sha256": hex(&Sha256::digest(PROGRAM)),
                            "size": PROGRAM.len(),
                            "kind": "binary"
                        }
                    }
                }
            }
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let signature = signing.sign(&bytes).to_bytes().to_vec();
        entries[0] = ("/update.json".into(), bytes);
        entries[1] = (
            "/update.json.sig".into(),
            base64::engine::general_purpose::STANDARD
                .encode(signature)
                .into_bytes(),
        );
        entries
    });
    let root = temp_dir("escape-root");
    let state = temp_dir("escape-state");
    let client = client(server.addr, &state, "0.6.5");

    let signed = client.check_signed().unwrap().expect("offered");
    let error = client
        .install_runtime(&signed, &root)
        .expect_err("path traversal refused");
    assert!(
        error.to_string().contains("invalid file name"),
        "unexpected error: {error}"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&state);
}
