//! aria2 JSON-RPC compatibility on the daemon's TCP listener.
//!
//! The listener is shared with Aether's native line protocol: the first line
//! decides which one it is. HTTP requests are parsed here and forwarded to
//! the `aria2` service plugin (`kind: "service"`), which translates them into
//! daemon calls. The token check and the transport stay in the daemon, so the
//! plugin never opens a second port; it only receives the RPC address and token
//! to derive the per-blob capabilities its `addTorrent` URLs carry.
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

use aether_plugin_sdk::blob_capability;
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
// The compat entry point mirrors the daemon's original wiring; splitting it
// just to satisfy the argument count would obscure the call chain.
#[allow(clippy::too_many_arguments)]
pub(crate) fn serve_http(
    mut reader: BufReader<Conn>,
    writer: &mut Conn,
    first_line: &str,
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
    commands: &Sender<crate::server::Command>,
    blob_dir: &std::path::Path,
    policy: &HttpPolicy,
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

    // Browser-facing hardening: this port is reachable from any page on the
    // machine, so a request aimed at another host (DNS rebinding) or issued
    // by a web origin that is not this listener is rejected before it can
    // reach any handler.
    if let Err(reason) = policy.check(&headers) {
        let _ = write_json(
            writer,
            "403 Forbidden",
            &rpc_error(&Value::Null, 1, &reason),
            false,
        );
        return;
    }

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
        if let Err(reason) = policy.check(&headers) {
            let _ = write_json(
                writer,
                "403 Forbidden",
                &rpc_error(&Value::Null, 1, &reason),
                false,
            );
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
    // plugins. The URL carries the RPC token; the name alone is not a
    // credential (other local users can list the directory).
    if method == "GET" && target.starts_with("/aria2-blobs/") {
        serve_blob(writer, target, blob_dir, secret, keep_alive);
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
        && writer
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| writer.flush())
            .is_err()
    {
        return false;
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

    // `aria2.shutdown` stops the daemon from inside the plugin call, so the
    // body must already be on the wire: otherwise the process exits between
    // the headers and the body and clients see a truncated response. This
    // applies to `system.multicall` too, where the stopping call can sit
    // anywhere in the array.
    let (response, pending) = prepare_reply(&request, bridge, secret);
    let written = write_json(writer, "200 OK", &response, keep_alive).is_ok();
    if let Some(stop) = pending {
        let _ = process_request(&stop, bridge, secret);
        return false;
    }
    written && keep_alive
}

/// Turn one JSON-RPC request into the reply to write.
///
/// When the request stops the daemon, the second value is the call to run
/// *after* the reply is on the wire (the process may exit inside it).
fn prepare_reply(
    request: &Value,
    bridge: Option<&dyn Bridge>,
    secret: &Arc<Mutex<Option<String>>>,
) -> (Value, Option<Value>) {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(plan) = shutdown_plan(request, bridge.is_some(), secret) else {
        return (process_request(request, bridge, secret), None);
    };
    match plan {
        ShutdownPlan::Direct { response, execute } => {
            let pending = execute.then(|| request.clone());
            (response, pending)
        }
        ShutdownPlan::Multicall {
            prefix,
            entry,
            method,
            params,
            execute,
        } => {
            // Calls before the shutdown run normally; calls after it are not
            // executed (aria2 would exit before reaching them).
            let mut response = match prefix {
                Some(prefix) => process_request(&prefix, bridge, secret),
                None => json!({ "jsonrpc": "2.0", "id": id, "result": [] }),
            };
            let appended = response
                .get_mut("result")
                .and_then(Value::as_array_mut)
                .map(|results| results.push(entry))
                .is_some();
            let pending = (appended && execute).then(|| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params,
                })
            });
            (response, pending)
        }
    }
}

/// A request that would stop the daemon before its HTTP body is written.
enum ShutdownPlan {
    /// `aria2.shutdown` / `aria2.forceShutdown` called directly.
    Direct { response: Value, execute: bool },
    /// `system.multicall` whose call array stops the daemon at the first
    /// stopping entry.
    Multicall {
        /// The calls before the stopping one, wrapped as their own
        /// `system.multicall` request (`None` when it is the first call).
        prefix: Option<Value>,
        /// The response slot reserved for the stopping call.
        entry: Value,
        method: String,
        params: Value,
        execute: bool,
    },
}

fn is_shutdown_method(method: &str) -> bool {
    matches!(method, "aria2.shutdown" | "aria2.forceShutdown")
}

fn shutdown_plan(
    request: &Value,
    bridge_present: bool,
    secret: &Arc<Mutex<Option<String>>>,
) -> Option<ShutdownPlan> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str)?;
    let params = request.get("params").cloned().unwrap_or_else(|| json!([]));

    if is_shutdown_method(method) {
        let authorized = authorized_params(method, params, secret).is_some();
        let response = match (authorized, bridge_present) {
            (true, true) => json!({ "jsonrpc": "2.0", "id": id, "result": "OK" }),
            (false, _) => rpc_error(&id, 1, "Unauthorized: invalid or missing token"),
            (true, false) => rpc_error(
                &id,
                1,
                "aria2 compatibility plugin is not installed or enabled",
            ),
        };
        return Some(ShutdownPlan::Direct {
            response,
            execute: authorized && bridge_present,
        });
    }

    if method != "system.multicall" {
        return None;
    }
    let calls = params.as_array()?.first()?.as_array()?;
    let index = calls.iter().position(|call| {
        call.get("methodName")
            .and_then(Value::as_str)
            .is_some_and(is_shutdown_method)
    })?;
    let call = &calls[index];
    let method = call.get("methodName").and_then(Value::as_str)?.to_string();
    let params = call
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let authorized = authorized_params(&method, Value::Array(params.clone()), secret).is_some();
    let entry = match (authorized, bridge_present) {
        (true, true) => json!({ "result": "OK" }),
        (false, _) => json!({
            "error": { "code": 1, "message": "Unauthorized: invalid or missing token" },
        }),
        (true, false) => json!({
            "error": {
                "code": 1,
                "message": "aria2 compatibility plugin is not installed or enabled",
            },
        }),
    };
    let prefix = (index > 0).then(|| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "system.multicall",
            "params": [Value::Array(calls[..index].to_vec())],
        })
    });
    Some(ShutdownPlan::Multicall {
        prefix,
        entry,
        method,
        params: Value::Array(params),
        execute: authorized && bridge_present,
    })
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

fn serve_blob(
    writer: &mut Conn,
    target: &str,
    directory: &std::path::Path,
    secret: &Arc<Mutex<Option<String>>>,
    keep_alive: bool,
) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let name = path.trim_start_matches("/aria2-blobs/");
    let secret = secret.lock().ok().and_then(|secret| secret.clone());
    let provided = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        .map(percent_decode);
    if !blob_authorized(secret.as_deref(), name, provided.as_deref()) {
        let _ = write_json(
            writer,
            "403 Forbidden",
            &rpc_error(&Value::Null, 1, "Unauthorized: invalid or missing token"),
            keep_alive,
        );
        return;
    }
    let Some(bytes) = read_blob(directory, name) else {
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

/// A blob read is authorized by a capability derived for *this* name, never by
/// the RPC token: the URL is stored in the task record and shown to the user,
/// so accepting the raw token would hand the whole RPC API to anyone who reads
/// a task list, an export, or a screenshot.
fn blob_authorized(secret: Option<&str>, name: &str, provided: Option<&str>) -> bool {
    match (secret.filter(|secret| !secret.is_empty()), provided) {
        (Some(secret), Some(provided)) => constant_time_eq(
            provided.as_bytes(),
            blob_capability(secret, name).as_bytes(),
        ),
        _ => false,
    }
}

/// Resolve a blob without ever following a symbolic link. The blob directory
/// is writable by the daemon user; a planted link would turn "read this blob"
/// into "read any file the daemon can read" (including `settings.json`).
fn read_blob(directory: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    let safe =
        !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..");
    if !safe {
        return None;
    }
    let path = directory.join(name);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    // Belt and braces: even a hard-link-free name is resolved once more and
    // must stay inside the blob directory.
    let root = std::fs::canonicalize(directory).ok()?;
    let real = std::fs::canonicalize(&path).ok()?;
    if !real.starts_with(&root) {
        return None;
    }
    std::fs::read(real).ok()
}

/// Request gate for the browser-reachable RPC port.
///
/// Loopback is not a boundary: any page in a browser on this machine can send
/// requests to `127.0.0.1`, and DNS rebinding lets an attacker-controlled
/// domain resolve to it. Two checks close that:
///
/// * `Host` must name this listener (an IP literal, or `localhost`), which
///   makes a rebinding request visibly wrong.
/// * `Origin`, when present, must pass the same host test, so a page served
///   from an unrelated domain is not allowed to drive the API.
pub(crate) struct HttpPolicy {
    bind: std::net::SocketAddr,
}

impl HttpPolicy {
    pub(crate) fn new(bind: std::net::SocketAddr) -> Self {
        Self { bind }
    }

    fn check(&self, headers: &[(String, String)]) -> Result<(), String> {
        if let Some(host) = header_value(headers, "host") {
            if !self.host_allowed(host, true) {
                return Err("forbidden Host".into());
            }
        }
        if let Some(origin) = header_value(headers, "origin") {
            if !self.origin_allowed(origin) {
                return Err("forbidden Origin".into());
            }
        }
        Ok(())
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        let Some((scheme, rest)) = origin.split_once("://") else {
            return false;
        };
        if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
            return false;
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        // The origin's port is the *page's* port, not the listener's, so only
        // the host part is compared.
        self.host_allowed(authority, false)
    }

    fn host_allowed(&self, value: &str, check_port: bool) -> bool {
        let (name, port) = split_host_port(value.trim());
        if check_port {
            if let Some(port) = port {
                if self.bind.port() != 0 && port != self.bind.port() {
                    return false;
                }
            }
        }
        if name.eq_ignore_ascii_case("localhost") {
            return true;
        }
        let Ok(ip) = name.parse::<std::net::IpAddr>() else {
            // A domain name is exactly the DNS-rebinding shape: reject it.
            return false;
        };
        if self.bind.ip().is_unspecified() {
            // A wildcard listener is reachable under any local address; an
            // IP literal is still not a rebinding domain.
            return true;
        }
        ip == self.bind.ip() || (self.bind.ip().is_loopback() && ip.is_loopback())
    }
}

/// Split `host:port`, including bracketed IPv6 (`[::1]:6800`).
fn split_host_port(value: &str) -> (&str, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((name, tail)) => (
                name,
                tail.strip_prefix(':').and_then(|port| port.parse().ok()),
            ),
            None => (value, None),
        };
    }
    match value.rsplit_once(':') {
        Some((name, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (name, port.parse().ok())
        }
        _ => (value, None),
    }
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
                let (reply, pending) = match serde_json::from_str::<Value>(&text) {
                    Ok(request) => prepare_reply(&request, bridge, secret),
                    Err(error) => (
                        rpc_error(&Value::Null, -32700, &format!("parse error: {error}")),
                        None,
                    ),
                };
                if write_text(writer, &reply.to_string()).is_err() {
                    break;
                }
                // The frame is on the wire before the daemon stops, so a
                // WebSocket client also sees a complete reply.
                if let Some(stop) = pending {
                    let _ = process_request(&stop, bridge, secret);
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

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn rejects_rebinding_hosts_and_foreign_origins() {
        let policy = HttpPolicy::new("127.0.0.1:6800".parse().unwrap());
        // The listener itself, by IP or by name, is fine.
        assert!(policy
            .check(&headers(&[("host", "127.0.0.1:6800")]))
            .is_ok());
        assert!(policy
            .check(&headers(&[("host", "localhost:6800")]))
            .is_ok());
        // A domain name resolving to the listener is the rebinding shape.
        assert!(policy
            .check(&headers(&[("host", "evil.example:6800")]))
            .is_err());
        // A Host naming another port is not this listener.
        assert!(policy
            .check(&headers(&[("host", "127.0.0.1:9000")]))
            .is_err());

        // The WebSocket upgrade path uses the same gate.
        let upgrade = headers(&[
            ("host", "127.0.0.1:6800"),
            ("origin", "https://evil.example"),
            ("upgrade", "websocket"),
        ]);
        assert!(policy.check(&upgrade).is_err());
        // A page served from the machine itself is allowed.
        assert!(policy
            .check(&headers(&[
                ("host", "127.0.0.1:6800"),
                ("origin", "http://127.0.0.1:8080"),
            ]))
            .is_ok());
        // An opaque origin (`file://`, sandboxed iframe) is not trusted.
        assert!(policy
            .check(&headers(&[("host", "127.0.0.1:6800"), ("origin", "null"),]))
            .is_err());
    }

    #[test]
    fn wildcard_and_pinned_listeners_agree_with_lan_clients() {
        // A wildcard listener is reached under this machine's own addresses, so
        // a non-loopback IP literal has to pass; a domain still does not.
        let wild = HttpPolicy::new("0.0.0.0:6800".parse().unwrap());
        assert!(wild
            .check(&headers(&[("host", "192.168.1.34:6800")]))
            .is_ok());
        assert!(wild
            .check(&headers(&[("host", "evil.example:6800")]))
            .is_err());
        // The same rule covers a page hosted elsewhere on the LAN: an IP
        // literal is not a rebinding domain, and the token is still required.
        assert!(wild
            .check(&headers(&[
                ("host", "192.168.1.34:6800"),
                ("origin", "http://192.168.1.50:8080"),
            ]))
            .is_ok());

        // Pinning a LAN address keeps the listener to that address: neither
        // loopback nor another host may name it.
        let pinned = HttpPolicy::new("192.168.1.34:6800".parse().unwrap());
        assert!(pinned
            .check(&headers(&[("host", "192.168.1.34:6800")]))
            .is_ok());
        assert!(pinned
            .check(&headers(&[("host", "localhost:6800")]))
            .is_ok());
        assert!(pinned
            .check(&headers(&[("host", "127.0.0.1:6800")]))
            .is_err());
        assert!(pinned
            .check(&headers(&[
                ("host", "192.168.1.34:6800"),
                ("origin", "http://10.1.2.3:8080"),
            ]))
            .is_err());
    }

    #[test]
    fn blob_reads_need_a_per_name_capability() {
        let secret = "s3cret";
        let capability = blob_capability(secret, "one.torrent");
        assert!(blob_authorized(
            Some(secret),
            "one.torrent",
            Some(&capability)
        ));
        // The RPC token itself is what the old URL carried; it must not open
        // a blob any more, or a task record would leak the whole API.
        assert!(!blob_authorized(Some(secret), "one.torrent", Some(secret)));
        // A capability is scoped to its own payload.
        assert!(!blob_authorized(
            Some(secret),
            "two.torrent",
            Some(&capability)
        ));
        assert!(!blob_authorized(Some(secret), "one.torrent", None));
        assert!(!blob_authorized(None, "one.torrent", Some(&capability)));
    }

    #[test]
    fn blob_reader_rejects_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "aether-blob-gate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("blob.bin"), b"payload").unwrap();
        assert_eq!(read_blob(&root, "blob.bin").unwrap(), b"payload");

        // A planted link to an arbitrary file is not readable as a blob.
        let outside = root.with_extension("outside-secret");
        std::fs::write(&outside, b"secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        #[cfg(unix)]
        assert!(read_blob(&root, "link").is_none());
        // Path separators and traversal never resolve.
        assert!(read_blob(&root, "../outside-secret").is_none());
        assert!(read_blob(&root, "sub/blob.bin").is_none());

        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&root).ok();
    }
}
