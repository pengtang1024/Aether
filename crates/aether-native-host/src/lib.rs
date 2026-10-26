//! Chrome/Firefox native messaging framing and request handling.
//!
//! Chrome and Firefox use a 4-byte length in *native* byte order followed by
//! UTF-8 JSON. The host never talks to the network itself: it forwards to the
//! local Aether daemon over its Unix socket.

use std::io::{Read, Write};

use serde_json::{json, Value};

pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

pub fn read_message<R: Read>(reader: &mut R) -> std::io::Result<Option<Value>> {
    let mut header = [0_u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_ne_bytes(header) as usize;
    if length == 0 || length > MAX_MESSAGE_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "native message has an invalid length",
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    let value = serde_json::from_slice(&payload)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(Some(value))
}

pub fn write_message<W: Write>(writer: &mut W, value: &Value) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "native message too large",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_ne_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

#[cfg(unix)]
pub fn handle_message(message: &Value) -> Value {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("add");
    match kind {
        "ping" => match connect() {
            Ok(mut client) => match client.ping() {
                Ok(()) => json!({"ok": true}),
                Err(error) => json!({"ok": false, "error": error.to_string()}),
            },
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        },
        "add" => handle_add(message),
        other => json!({"ok": false, "error": format!("unknown message type {other}")}),
    }
}

#[cfg(unix)]
fn handle_add(message: &Value) -> Value {
    let Some(url) = message.get("url").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "url is required"});
    };
    if !safe_scheme(url) {
        return json!({"ok": false, "error": "unsupported URL scheme"});
    }

    let mut settings = serde_json::Map::new();
    for key in ["referrer", "headers", "cookies", "filename"] {
        if let Some(value) = message.get(key) {
            settings.insert(key.to_string(), value.clone());
        }
    }

    match connect() {
        Ok(mut client) => match client.add_with_settings(
            url,
            None,
            message.get("plugin").and_then(Value::as_str),
            Value::Object(settings),
        ) {
            Ok(result) => json!({
                "ok": true,
                "task_id": result.get("task_id"),
                "plugin_id": result.get("plugin_id"),
            }),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        },
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    }
}

fn safe_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http"
            | "https"
            | "ftp"
            | "ftps"
            | "sftp"
            | "s3"
            | "ipfs"
            | "ipns"
            | "magnet"
            | "thunder"
            | "ed2k"
            | "rtmp"
            | "rtsp"
            | "aether"
            | "webdav"
            | "webdavs"
            | "gdrive"
            | "onedrive"
            | "dropbox"
            | "baidupan"
            | "aliyunpan"
    )
}

#[cfg(unix)]
fn connect() -> Result<aether_daemon::Client, String> {
    let socket = aether_daemon::client::default_socket();
    if let Ok(client) = aether_daemon::Client::connect(&socket) {
        return Ok(client);
    }

    let daemon = std::env::var("AETHER_DAEMON").unwrap_or_else(|_| {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("aether-daemon")))
            .filter(|path| path.is_file());
        sibling
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "aether-daemon".into())
    });
    let download_dir = std::env::var("AETHER_DOWNLOAD_DIR")
        .or_else(|_| std::env::var("HOME").map(|home| format!("{home}/Downloads")))
        .unwrap_or_else(|_| "downloads".into());
    let mut command = std::process::Command::new(daemon);
    command
        .arg("--download-dir")
        .arg(&download_dir)
        .arg("--socket")
        .arg(&socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn()
        .map_err(|error| format!("cannot start daemon: {error}"))?;

    for _ in 0..50 {
        if let Ok(client) = aether_daemon::Client::connect(&socket) {
            return Ok(client);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err("daemon did not become ready".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrips_native_messages() {
        let message = json!({"type": "add", "url": "https://example.com/file.bin"});
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).unwrap();
        let mut cursor = Cursor::new(buffer);
        assert_eq!(read_message(&mut cursor).unwrap(), Some(message));
        assert_eq!(read_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn rejects_unsafe_schemes() {
        assert!(safe_scheme("https://example.com/file"));
        assert!(safe_scheme("magnet:?xt=urn:btih:x"));
        assert!(!safe_scheme("javascript:alert(1)"));
        assert!(!safe_scheme("file:///etc/passwd"));
    }
}
