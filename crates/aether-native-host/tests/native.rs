#![cfg(unix)]
//! Native Messaging framing + daemon forwarding test.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_native_host::{read_message, write_message};
use serde_json::json;

fn temp_socket() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("aether-native-{}-{nanos}.sock", std::process::id()))
}

#[test]
fn forwards_add_to_daemon_over_native_messaging() {
    let socket = temp_socket();
    let listener = UnixListener::bind(&socket).unwrap();
    // The mock daemon records the request so the test can assert that the
    // browser context (referrer, cookies, headers) survives the hop into
    // TaskSpec.settings — that passthrough is what makes authenticated
    // downloads work without engine changes.
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
    let captured = recorded.clone();
    let daemon = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let response = json!({
            "id": request["id"],
            "ok": true,
            "result": {"task_id": "task-1", "plugin_id": "http"}
        });
        writer
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        *captured.lock().unwrap() = Some(request);
    });

    std::env::set_var("AETHER_SOCKET", socket.to_string_lossy().as_ref());
    let mut child = Command::new(env!("CARGO_BIN_EXE_aether-native-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    write_message(
        &mut stdin,
        &json!({
            "type": "add",
            "url": "https://example.com/file.bin",
            "referrer": "https://example.com/page",
            "cookies": "session=abc; theme=dark",
            "headers": {"X-Token": "t1"},
            "filename": "file.bin"
        }),
    )
    .unwrap();
    drop(stdin);

    let response = read_message(&mut stdout).unwrap().expect("response");
    assert_eq!(response["ok"], true);
    assert_eq!(response["task_id"], "task-1");
    assert!(child.wait().unwrap().success());
    daemon.join().unwrap();

    let request = recorded.lock().unwrap().clone().expect("recorded request");
    assert_eq!(request["method"], "add");
    assert_eq!(request["url"], "https://example.com/file.bin");
    let settings = &request["settings"];
    assert_eq!(settings["referrer"], "https://example.com/page");
    assert_eq!(settings["cookies"], "session=abc; theme=dark");
    assert_eq!(settings["headers"]["X-Token"], "t1");
    assert_eq!(settings["filename"], "file.bin");
    std::fs::remove_file(socket).ok();
}

#[test]
fn rejects_unsafe_urls_before_touching_the_daemon() {
    // file:// must be refused by the host itself: it never reaches the
    // daemon, so a compromised page cannot read local files.
    let response = aether_native_host::handle_message(&json!({
        "type": "add",
        "url": "file:///etc/passwd"
    }));
    assert_eq!(response["ok"], false);
}
