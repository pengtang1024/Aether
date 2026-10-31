//! aria2 JSON-RPC compatibility on the daemon's TCP listener.
//!
//! The listener is shared with Aether's native line protocol: the first line
//! decides which one it is. HTTP requests are parsed here and forwarded to
//! the `aria2` service plugin (`kind: "service"`), which translates them into
//! daemon calls. Keeping the token check and the transport in the daemon means
//! the plugin never sees the secret and never opens a second port.
//!
//! Supported transports: `POST /jsonrpc`, `GET /jsonrpc?...`, and
//! `ws://.../jsonrpc`, all with CORS so browser front-ends such as AriaNg can
//! talk to it. The WebSocket path serves JSON-RPC calls and pushes
//! `aria2.onDownload*` notifications from the daemon's task event stream.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::service::Bridge;
use crate::transport::Conn;

/// Maximum request body accepted from a client (aria2 clients stay far below
/// this; the cap only keeps a rogue local process from allocating freely).
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Maximum number of request headers read before giving up.
const MAX_HEADERS: usize = 128;
/// Maximum WebSocket frame accepted from a client (addTorrent payloads are
/// base64 and can be large; the HTTP body uses the same ceiling).
const MAX_WS_FRAME: usize = 16 * 1024 * 1024;
/// RFC 6455 handshake GUID.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// True when the first line of a connection is an HTTP request line.
pub(crate) fn is_http_request(first_line: &str) -> bool {
    let mut parts = first_line.split_whitespace();
    matches!(
        parts.next(),
        Some("GET" | "POST" | "OPTIONS" | "HEAD" | "PUT" | "DELETE" | "PATCH")
    ) && parts.next().is_some()
}

/// Serve one HTTP connection. The first request line was already consumed by
/// the caller so it can decide between HTTP and the native protocol.
pub(crate) fn serve_http(
    mut reader: BufReader<Conn>,
    writer: &mut Conn,
    first_line: &str,
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
    commands: &Sender<crate::server::Command>,
    blob_dir: &std::path::Path,
) {
    let Some((method, target, version)) = parse_request_line(first_line) else {
        let _ = write_json(
            writer,
            "400 Bad Request",
            &rpc_error(&Value::Null, -32600, "invalid request line"),
            false,
        );
        return;
    };
    let mut method = method.to_string();
    let mut target = target.to_string();
    let mut version = version.to_string();
    let mut headers = match read_headers(&mut reader) {
        Ok(headers) => headers,
        Err(error) => {
            let _ = write_json(
                writer,
                "400 Bad Request",
                &rpc_error(&Value::Null, -32600, &format!("invalid headers: {error}")),
                false,
            );
            return;
        }
    };

    // WebSocket upgrade: aria2 clients (AriaNg among them) use it for command
    // calls and real-time notifications on the same /jsonrpc endpoint.
    if is_websocket(&headers) {
        serve_websocket(reader, writer, &headers, bridge, secret, commands);
        return;
    }

    // Keep-alive is useful for AriaNg's polling and for the local plugins
    // fetching blobs. An idle connection is closed after 60 s.
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(60)));

    loop {
        let keep_alive = http_keep_alive(&version, &headers);
        if !handle_http_request(
            &mut reader,
            writer,
            &method,
            &target,
            &headers,
            bridge,
            secret,
            blob_dir,
            keep_alive,
        ) {
            break;
        }
        if !keep_alive {
            break;
        }

        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let Some((next_method, next_target, next_version)) = parse_request_line(&line) else {
            break;
        };
        method = next_method.to_string();
        target = next_target.to_string();
        version = next_version.to_string();
        headers = match read_headers(&mut reader) {
            Ok(headers) => headers,
            Err(_) => break,
        };
        // Upgrade is only valid on a fresh connection.
        if is_websocket(&headers) {
            break;
        }
    }
}

fn http_keep_alive(version: &str, headers: &[(String, String)]) -> bool {
    let connection = headers
        .iter()
        .find(|(name, _)| name == "connection")
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default();
    if connection
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
    {
        return false;
    }
    if version.eq_ignore_ascii_case("HTTP/1.0") {
        return connection
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("keep-alive"));
    }
    true
}

/// Handle one request on a connection; returns whether the connection can be
/// reused (the request body was consumed and the response was written).
#[allow(clippy::too_many_arguments)]
fn handle_http_request(
    reader: &mut BufReader<Conn>,
    writer: &mut Conn,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
    blob_dir: &std::path::Path,
    keep_alive: bool,
) -> bool {
    // Persisted addTorrent/addMetalink bytes served to the local download
    // plugins. The random token is the capability.
    if method == "GET" && target.starts_with("/aria2-blobs/") {
        serve_blob(writer, target, blob_dir, keep_alive);
        return keep_alive;
    }

    // CORS preflight. A browser front-end sends OPTIONS before JSON-RPC.
    if method == "OPTIONS" {
        return write_empty(writer, "204 No Content", keep_alive).is_ok() && keep_alive;
    }
    if method == "HEAD" {
        return write_empty(writer, "200 OK", keep_alive).is_ok() && keep_alive;
    }
    if method == "POST"
        && headers
            .iter()
            .any(|(name, value)| name == "expect" && value.eq_ignore_ascii_case("100-continue"))
    {
        if writer
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| writer.flush())
            .is_err()
        {
            return false;
        }
    }

    let request = match method {
        "POST" => match read_body(reader, headers) {
            Ok(body) => match serde_json::from_slice::<Value>(&body) {
                Ok(value) => value,
                Err(error) => {
                    let _ = write_json(
                        writer,
                        "400 Bad Request",
                        &rpc_error(&Value::Null, -32700, &format!("parse error: {error}")),
                        keep_alive,
                    );
                    return keep_alive;
                }
            },
            Err(error) => {
                // The body was not consumed; the connection is desynchronised.
                let _ = write_json(
                    writer,
                    "400 Bad Request",
                    &rpc_error(&Value::Null, -32600, &format!("cannot read body: {error}")),
                    false,
                );
                return false;
            }
        },
        "GET" => match request_from_query(target) {
            Some(value) => value,
            None => {
                let _ = write_json(
                    writer,
                    "400 Bad Request",
                    &rpc_error(&Value::Null, -32602, "GET needs a method parameter"),
                    keep_alive,
                );
                return keep_alive;
            }
        },
        _ => {
            let _ = write_json(
                writer,
                "405 Method Not Allowed",
                &rpc_error(&Value::Null, -32600, "only GET and POST are supported"),
                false,
            );
            return false;
        }
    };

    let response = process_request(&request, bridge, secret);
    write_json(writer, "200 OK", &response, keep_alive).is_ok() && keep_alive
}

/// Validate one JSON-RPC request and turn it into a full JSON-RPC response.
fn process_request(
    request: &Value,
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return rpc_error(&id, -32600, "missing method");
    };
    if !(method.starts_with("aria2.") || method.starts_with("system.")) {
        return rpc_error(&id, -32601, &format!("method not found: {method}"));
    }
    let params = request.get("params").cloned().unwrap_or_else(|| json!([]));
    let Some(params) = authorized_params(method, params, secret) else {
        return rpc_error(&id, 1, "Unauthorized: invalid or missing token");
    };
    let Some(bridge) = bridge else {
        return rpc_error(
            &id,
            1,
            "aria2 compatibility plugin is not installed or enabled",
        );
    };
    match bridge.call(json!({ "method": method, "params": params })) {
        Ok(reply) => {
            if let Some(error) = reply.get("error") {
                json!({ "jsonrpc": "2.0", "id": id, "error": error })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": reply.get("result").cloned().unwrap_or(Value::Null),
                })
            }
        }
        Err(message) => rpc_error(&id, 1, &message),
    }
}

fn serve_blob(writer: &mut Conn, target: &str, directory: &std::path::Path, keep_alive: bool) {
    let name = target.trim_start_matches("/aria2-blobs/");
    let safe =
        !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..");
    let bytes = if safe {
        std::fs::read(directory.join(name)).ok()
    } else {
        None
    };
    let Some(bytes) = bytes else {
        let _ = write_bytes(
            writer,
            "404 Not Found",
            "text/plain",
            b"not found",
            keep_alive,
        );
        return;
    };
    let content_type = if name.ends_with(".torrent") {
        "application/x-bittorrent"
    } else if name.ends_with(".metalink") || name.ends_with(".meta4") {
        "application/metalink4+xml"
    } else {
        "application/octet-stream"
    };
    let _ = write_bytes(writer, "200 OK", content_type, &bytes, keep_alive);
}

fn is_websocket(headers: &[(String, String)]) -> bool {
    let upgrade = headers
        .iter()
        .any(|(name, value)| name == "upgrade" && value.eq_ignore_ascii_case("websocket"));
    let connection = headers.iter().any(|(name, value)| {
        name == "connection" && value.to_ascii_lowercase().contains("upgrade")
    });
    upgrade && connection
}

enum WsIn {
    Text(String),
    Ping(Vec<u8>),
    Close,
}

/// Upgrade one connection to WebSocket and serve aria2 JSON-RPC over it.
fn serve_websocket(
    reader: BufReader<Conn>,
    writer: &mut Conn,
    headers: &[(String, String)],
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
    commands: &Sender<crate::server::Command>,
) {
    let Some(key) = header_value(headers, "sec-websocket-key") else {
        let _ = write_json(
            writer,
            "400 Bad Request",
            &rpc_error(&Value::Null, -32600, "missing Sec-WebSocket-Key"),
            false,
        );
        return;
    };
    let accept = ws_accept(key);
    if write!(
        writer,
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    )
    .and_then(|()| writer.flush())
    .is_err()
    {
        return;
    }

    let Some(events) = crate::server::subscribe_events(commands) else {
        let _ = write_close(writer);
        return;
    };

    // The reader thread only parses frames; the handler thread owns the
    // writer so responses and notifications never interleave mid-frame.
    let (inbound_tx, inbound_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = reader;
        let mut fragments: Vec<u8> = Vec::new();
        let mut fragment_opcode = 0u8;
        loop {
            let frame = match read_frame(&mut reader) {
                Ok(Some(frame)) => frame,
                Ok(None) | Err(_) => {
                    let _ = inbound_tx.send(WsIn::Close);
                    break;
                }
            };
            // Control frames run side effects through the channel, so clippy's
            // match-guard suggestion does not fit here.
            #[allow(clippy::collapsible_match)]
            match frame.opcode {
                0x0 => {
                    // Continuation of a fragmented text message (browsers
                    // split large addTorrent payloads).
                    if fragments.is_empty() || fragment_opcode != 0x1 {
                        continue;
                    }
                    if fragments.len().saturating_add(frame.payload.len()) > MAX_WS_FRAME {
                        let _ = inbound_tx.send(WsIn::Close);
                        break;
                    }
                    fragments.extend_from_slice(&frame.payload);
                    if frame.fin {
                        let text = match String::from_utf8(std::mem::take(&mut fragments)) {
                            Ok(text) => text,
                            Err(_) => {
                                let _ = inbound_tx.send(WsIn::Close);
                                break;
                            }
                        };
                        fragment_opcode = 0;
                        if inbound_tx.send(WsIn::Text(text)).is_err() {
                            break;
                        }
                    }
                }
                0x1 => {
                    if frame.fin {
                        let text = match String::from_utf8(frame.payload) {
                            Ok(text) => text,
                            Err(_) => {
                                let _ = inbound_tx.send(WsIn::Close);
                                break;
                            }
                        };
                        if inbound_tx.send(WsIn::Text(text)).is_err() {
                            break;
                        }
                    } else {
                        fragment_opcode = 0x1;
                        fragments = frame.payload;
                    }
                }
                0x8 => {
                    let _ = inbound_tx.send(WsIn::Close);
                    break;
                }
                0x9 => {
                    if inbound_tx.send(WsIn::Ping(frame.payload)).is_err() {
                        break;
                    }
                }
                _ => {}
            }
        }
    });

    loop {
        match inbound_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WsIn::Text(text)) => {
                let reply = match serde_json::from_str::<Value>(&text) {
                    Ok(request) => process_request(&request, bridge, secret),
                    Err(error) => rpc_error(&Value::Null, -32700, &format!("parse error: {error}")),
                };
                if write_text(writer, &reply.to_string()).is_err() {
                    break;
                }
            }
            Ok(WsIn::Ping(payload)) => {
                if write_frame(writer, 0xA, &payload).is_err() {
                    break;
                }
            }
            Ok(WsIn::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(event) = events.try_recv() {
            if let Some(note) = notification(&event) {
                if write_text(writer, &note).is_err() {
                    return;
                }
            }
        }
    }
    let _ = write_close(writer);
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn ws_accept(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

struct WsFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn read_frame(reader: &mut BufReader<Conn>) -> std::io::Result<Option<WsFrame>> {
    let mut header = [0u8; 2];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let fin = header[0] & 0x80 != 0;
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut length = u64::from(header[1] & 0x7f);
    if length == 126 {
        let mut extended = [0u8; 2];
        reader.read_exact(&mut extended)?;
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0u8; 8];
        reader.read_exact(&mut extended)?;
        length = u64::from_be_bytes(extended);
    }
    if length > MAX_WS_FRAME as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "websocket frame too large",
        ));
    }
    let mut mask = [0u8; 4];
    if masked {
        reader.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload)?;
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Ok(Some(WsFrame {
        fin,
        opcode,
        payload,
    }))
}

fn write_frame(writer: &mut Conn, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode);
    let length = payload.len();
    if length < 126 {
        header.push(length as u8);
    } else if let Ok(length) = u16::try_from(length) {
        header.push(126);
        header.extend_from_slice(&length.to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(length as u64).to_be_bytes());
    }
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    writer.flush()
}

fn write_text(writer: &mut Conn, text: &str) -> std::io::Result<()> {
    write_frame(writer, 0x1, text.as_bytes())
}

fn write_close(writer: &mut Conn) -> std::io::Result<()> {
    write_frame(writer, 0x8, &[])
}

/// Translate a daemon task event into an aria2 notification.
fn notification(event: &Value) -> Option<String> {
    let task_id = event.get("task_id").and_then(Value::as_str)?;
    let method = match event.get("event").and_then(Value::as_str)? {
        "task_state" => match event.get("state").and_then(Value::as_str)? {
            "running" => "aria2.onDownloadStart",
            "paused" => "aria2.onDownloadPause",
            "canceled" => "aria2.onDownloadStop",
            _ => return None,
        },
        "completed" => "aria2.onDownloadComplete",
        "failed" => "aria2.onDownloadError",
        "task_deleted" => "aria2.onDownloadStop",
        _ => return None,
    };
    Some(
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": [{ "gid": gid(task_id) }],
        })
        .to_string(),
    )
}

fn gid(task_id: &str) -> String {
    if let Some(suffix) = task_id.strip_prefix("task-") {
        if let Ok(value) = suffix.parse::<u64>() {
            return format!("{value:016x}");
        }
    }
    format!("{:016x}", fnv1a(task_id.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn parse_request_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    Some((method, target, version))
}

fn read_headers(reader: &mut BufReader<Conn>) -> std::io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed in headers",
            ));
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= MAX_HEADERS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
        if let Some((name, value)) = text.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
}

fn read_body(
    reader: &mut BufReader<Conn>,
    headers: &[(String, String)],
) -> std::io::Result<Vec<u8>> {
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing content-length")
        })?;
    if length > MAX_BODY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body too large",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// Parse `?method=...&params=...&id=...`, percent-decoding the values.
fn request_from_query(target: &str) -> Option<Value> {
    let query = target.split_once('?')?.1;
    let mut method = None;
    let mut params = None;
    let mut id = Value::String("aether".into());
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key {
            "method" => method = Some(value),
            "params" => {
                params = Some(
                    serde_json::from_str::<Value>(&value).unwrap_or(Value::Array(
                        value
                            .split(',')
                            .filter(|entry| !entry.is_empty())
                            .map(|entry| Value::String(entry.to_string()))
                            .collect(),
                    )),
                )
            }
            "id" => id = Value::String(value),
            _ => {}
        }
    }
    Some(json!({
        "method": method?,
        "params": params.unwrap_or_else(|| json!([])),
        "id": id,
    }))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = (bytes[index + 1] as char).to_digit(16);
                let low = (bytes[index + 2] as char).to_digit(16);
                if let (Some(high), Some(low)) = (high, low) {
                    out.push((high * 16 + low) as u8);
                    index += 2;
                } else {
                    out.push(bytes[index]);
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Validate the aria2 token convention: the first parameter is
/// `"token:<rpc_secret>"`. Returns the params with that element removed.
///
/// `system.multicall` nests the token inside every call, so each entry is
/// checked and stripped independently.
fn authorized_params(
    method: &str,
    params: Value,
    secret: &Arc<Mutex<Option<String>>>,
) -> Option<Value> {
    // A poisoned lock means the daemon already lost an auth thread: deny
    // rather than silently turning the token check off.
    let Ok(guard) = secret.lock() else {
        return None;
    };
    let expected = guard.clone().filter(|secret| !secret.is_empty());
    drop(guard);

    if method == "system.multicall" {
        let calls = params.as_array()?.first()?.as_array()?;
        let cleaned = calls
            .iter()
            .map(|call| {
                let call_params = call.get("params").cloned().unwrap_or_else(|| json!([]));
                let call_params = strip_token(call_params, expected.as_deref())?;
                let mut call = call.clone();
                call["params"] = call_params;
                Some(call)
            })
            .collect::<Option<Vec<_>>>()?;
        return Some(json!([cleaned]));
    }

    strip_token(params, expected.as_deref())
}

fn strip_token(params: Value, expected: Option<&str>) -> Option<Value> {
    let mut items = params.as_array()?.clone();
    let candidate = items.first().and_then(Value::as_str).map(str::to_string);
    let provided = candidate
        .as_deref()
        .and_then(|value| value.strip_prefix("token:"));
    match (expected, provided) {
        (Some(expected), Some(provided)) => {
            if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                return None;
            }
            items.remove(0);
        }
        (Some(_), None) => return None,
        (None, Some(_)) => {
            // No token configured: accept (and drop) whatever the client sent.
            items.remove(0);
        }
        (None, None) => {}
    }
    Some(Value::Array(items))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn write_bytes(
    writer: &mut Conn,
    status: &str,
    content_type: &str,
    body: &[u8],
    keep_alive: bool,
) -> std::io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: {}\r\n\
         \r\n",
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    )?;
    writer.write_all(body)?;
    writer.flush()
}

fn write_json(
    writer: &mut Conn,
    status: &str,
    body: &Value,
    keep_alive: bool,
) -> std::io::Result<()> {
    let raw = serde_json::to_vec(body)?;
    write_response(writer, status, Some(&raw), keep_alive)
}

fn write_empty(writer: &mut Conn, status: &str, keep_alive: bool) -> std::io::Result<()> {
    write_response(writer, status, None, keep_alive)
}

fn write_response(
    writer: &mut Conn,
    status: &str,
    body: Option<&[u8]>,
    keep_alive: bool,
) -> std::io::Result<()> {
    let length = body.map_or(0, |body| body.len());
    write!(
        writer,
        "HTTP/1.1 {status}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {length}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n\
         Access-Control-Allow-Headers: content-type, authorization\r\n\
         Access-Control-Max-Age: 86400\r\n\
         Connection: {}\r\n\
         \r\n",
        if keep_alive { "keep-alive" } else { "close" }
    )?;
    if let Some(body) = body {
        writer.write_all(body)?;
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(value: Option<&str>) -> Arc<Mutex<Option<String>>> {
        Arc::new(Mutex::new(value.map(str::to_string)))
    }

    #[test]
    fn recognises_http_request_lines() {
        assert!(is_http_request("POST /jsonrpc HTTP/1.1\r\n"));
        assert!(is_http_request(
            "GET /jsonrpc?method=aria2.getVersion HTTP/1.1\r\n"
        ));
        assert!(!is_http_request("{\"id\":1,\"method\":\"auth\"}\r\n"));
    }

    #[test]
    fn strips_and_checks_the_aria2_token() {
        let token = secret(Some("s3cret"));
        let params = json!(["token:s3cret", ["https://example.test/a"]]);
        let cleaned = authorized_params("aria2.addUri", params, &token).unwrap();
        assert_eq!(cleaned, json!([["https://example.test/a"]]));

        let missing = json!([["https://example.test/a"]]);
        assert!(authorized_params("aria2.addUri", missing, &token).is_none());

        let wrong = json!(["token:nope", []]);
        assert!(authorized_params("aria2.addUri", wrong, &token).is_none());

        // No configured token: the client's token is optional and dropped.
        let open = secret(None);
        assert_eq!(
            authorized_params("aria2.getVersion", json!(["token:any"]), &open).unwrap(),
            json!([])
        );
        assert_eq!(
            authorized_params("aria2.getVersion", json!([]), &open).unwrap(),
            json!([])
        );
    }

    #[test]
    fn multicall_tokens_are_checked_per_call() {
        let secret = secret(Some("s3cret"));
        let params = json!([[
            { "methodName": "aria2.getVersion", "params": ["token:s3cret"] },
            { "methodName": "aria2.getGlobalStat", "params": ["token:s3cret"] },
        ]]);
        let cleaned = authorized_params("system.multicall", params, &secret).unwrap();
        assert_eq!(
            cleaned,
            json!([[
                { "methodName": "aria2.getVersion", "params": [] },
                { "methodName": "aria2.getGlobalStat", "params": [] },
            ]])
        );

        let bad = json!([[
            { "methodName": "aria2.getVersion", "params": ["token:s3cret"] },
            { "methodName": "aria2.getGlobalStat", "params": ["token:wrong"] },
        ]]);
        assert!(authorized_params("system.multicall", bad, &secret).is_none());
    }

    #[test]
    fn computes_the_rfc6455_accept_key() {
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn maps_task_events_to_aria2_notifications() {
        let note = notification(&json!({
            "event": "task_state",
            "task_id": "task-1",
            "state": "running",
        }))
        .unwrap();
        let value: Value = serde_json::from_str(&note).unwrap();
        assert_eq!(value["method"], "aria2.onDownloadStart");
        assert_eq!(value["params"][0]["gid"], "0000000000000001");

        assert!(notification(&json!({
            "event": "progress",
            "task_id": "task-1",
        }))
        .is_none());
    }

    #[test]
    fn parses_get_queries() {
        let request = request_from_query(
            "/jsonrpc?method=aria2.addUri&params=%5B%22https%3A%2F%2Fexample.test%2Fa%22%5D&id=7",
        )
        .unwrap();
        assert_eq!(request["method"], "aria2.addUri");
        assert_eq!(request["params"], json!(["https://example.test/a"]));
        assert_eq!(request["id"], "7");
    }
}
