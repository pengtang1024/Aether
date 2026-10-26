//! Signed update manifest + artifact tests against a local HTTP server.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_update::UpdateClient;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

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
        "aether-update-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn routes(
    addr: SocketAddr,
    artifact: &[u8],
    version: &str,
    tamper: bool,
) -> Vec<(String, Vec<u8>)> {
    let signing = SigningKey::from_bytes(&[7_u8; 32]);
    let manifest = serde_json::json!({
        "version": version,
        "channel": "stable",
        "assets": {
            "linux-x86_64": {
                "url": format!("http://{addr}/aether.AppImage"),
                "sha256": hex(&Sha256::digest(artifact)),
                "size": artifact.len(),
                "kind": "appimage"
            }
        }
    })
    .to_string();
    let manifest_bytes = manifest.into_bytes();
    let mut signature = signing.sign(&manifest_bytes).to_bytes().to_vec();
    if tamper {
        signature[0] ^= 1;
    }
    vec![
        ("/update.json".into(), manifest_bytes),
        (
            "/update.json.sig".into(),
            base64::engine::general_purpose::STANDARD
                .encode(signature)
                .into_bytes(),
        ),
        ("/aether.AppImage".into(), artifact.to_vec()),
    ]
}

fn public_key() -> String {
    let signing = SigningKey::from_bytes(&[7_u8; 32]);
    base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes())
}

#[test]
fn verifies_downloads_and_applies_appimage() {
    let artifact = b"new appimage bytes".to_vec();
    let server = start_server(|addr| routes(addr, &artifact, "0.2.0", false));
    let root = temp_dir("apply");
    let client = UpdateClient::new(
        &format!("http://{}/update.json", server.addr),
        &public_key(),
        "0.1.0",
        "linux-x86_64",
        &root,
    )
    .unwrap();
    let update = client.check().unwrap().expect("update available");
    let asset = client.asset_for_platform(&update).unwrap();
    let downloaded = client.download(&update, asset).unwrap();
    assert_eq!(fs::read(&downloaded).unwrap(), artifact);

    let target = root.join("Aether.AppImage");
    fs::write(&target, b"old").unwrap();
    client.apply_appimage(&downloaded, &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), artifact);
    assert_eq!(fs::read(root.join("Aether.AppImage.old")).unwrap(), b"old");
    fs::remove_dir_all(root).ok();
}

#[test]
fn rejects_tampered_signature() {
    let artifact = b"x".to_vec();
    let server = start_server(|addr| routes(addr, &artifact, "0.2.0", true));
    let root = temp_dir("tamper");
    let client = UpdateClient::new(
        &format!("http://{}/update.json", server.addr),
        &public_key(),
        "0.1.0",
        "linux-x86_64",
        &root,
    )
    .unwrap();
    assert!(client.check().is_err());
    fs::remove_dir_all(root).ok();
}

#[test]
fn rejects_checksum_mismatch() {
    let artifact = b"real bytes".to_vec();
    let server = start_server(|addr| {
        let mut routes = routes(addr, &artifact, "0.2.0", false);
        // Corrupt the served artifact, keeping the signed manifest hash.
        for (path, body) in &mut routes {
            if path == "/aether.AppImage" {
                body[0] ^= 1;
            }
        }
        routes
    });
    let root = temp_dir("checksum");
    let client = UpdateClient::new(
        &format!("http://{}/update.json", server.addr),
        &public_key(),
        "0.1.0",
        "linux-x86_64",
        &root,
    )
    .unwrap();
    let update = client.check().unwrap().expect("update available");
    let asset = client.asset_for_platform(&update).unwrap();
    assert!(client.download(&update, asset).is_err());
    fs::remove_dir_all(root).ok();
}
