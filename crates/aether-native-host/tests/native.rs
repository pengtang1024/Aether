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
        &json!({"type": "add", "url": "https://example.com/file.bin"}),
    )
    .unwrap();
    drop(stdin);

    let response = read_message(&mut stdout).unwrap().expect("response");
    assert_eq!(response["ok"], true);
    assert_eq!(response["task_id"], "task-1");
    assert!(child.wait().unwrap().success());
    daemon.join().unwrap();
    std::fs::remove_file(socket).ok();
}
