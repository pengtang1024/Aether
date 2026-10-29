//! Transport loopback: one client line in, one response line out.
//!
//! Runs on every platform (Unix socket on Unix, named pipe on Windows) —
//! this is the acceptance test for the Windows named-pipe transport.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_daemon::transport::{Conn, Listener};

fn endpoint(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    #[cfg(unix)]
    {
        std::env::temp_dir().join(format!(
            "aether-transport-{label}-{}-{nanos}.sock",
            std::process::id()
        ))
    }
    #[cfg(windows)]
    {
        let _ = (label, nanos);
        PathBuf::from(format!(
            r"\\.\pipe\aether-transport-test-{}",
            std::process::id()
        ))
    }
}

#[test]
fn roundtrips_lines() {
    let endpoint = endpoint("roundtrip");
    let listener = Listener::bind(&endpoint).unwrap();

    let server = thread::spawn(move || {
        let conn = listener.accept().unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut conn = conn;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"method":"ping"}"#);
        conn.write_all(b"{\"ok\":true}\n").unwrap();
        conn.flush().unwrap();
        conn.shutdown();
    });

    let mut client = Conn::connect(&endpoint).unwrap();
    client.write_all(b"{\"method\":\"ping\"}\n").unwrap();
    client.flush().unwrap();
    let mut response = String::new();
    BufReader::new(client.try_clone().unwrap())
        .read_line(&mut response)
        .unwrap();
    assert_eq!(response.trim(), r#"{"ok":true}"#);
    server.join().unwrap();
}

#[test]
fn second_listener_on_a_live_endpoint_fails() {
    let endpoint = endpoint("exclusive");
    let _listener = Listener::bind(&endpoint).unwrap();
    // Unix: the socket file exists, bind errors. Windows: every accept
    // creates a fresh pipe instance, so both binds succeed — exclusivity
    // there comes from the daemon's FILE-flag-free probe plus the lock
    // file, not from the listener. Only assert on Unix.
    #[cfg(unix)]
    {
        let second = Listener::bind(&endpoint);
        assert!(second.is_err(), "a live socket file must not rebind");
    }
}
