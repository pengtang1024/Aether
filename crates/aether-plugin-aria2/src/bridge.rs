//! aria2 method dispatch. The daemon has already validated the token; this
//! module only translates parameters and daemon replies.

use base64::Engine;
use serde_json::{json, Value};

use crate::client::DaemonClient;
use crate::host::TorrentHost;
use crate::mapping;
use crate::query;

#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<String> for RpcError {
    fn from(message: String) -> Self {
        Self::new(1, message)
    }
}

pub type RpcResult = Result<Value, RpcError>;

pub fn handle(
    client: &mut DaemonClient,
    host: Option<&TorrentHost>,
    session: &str,
    request: &Value,
) -> Value {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match dispatch(client, host, session, method, &params) {
        Ok(result) => json!({ "result": result }),
        Err(error) => json!({
            "error": { "code": error.code, "message": error.message },
        }),
    }
}

fn dispatch(
    client: &mut DaemonClient,
    host: Option<&TorrentHost>,
    session: &str,
    method: &str,
    params: &[Value],
) -> RpcResult {
    match method {
        "aria2.addUri" => add_uri(client, params),
        "aria2.addTorrent" => add_torrent(client, host, params),
        "aria2.addMetalink" => add_metalink(client, host, params),
        "aria2.tellStatus" => query::tell_status(client, params),
        "aria2.tellActive" => query::tell_active(client, params),
        "aria2.tellWaiting" => query::tell_waiting(client, params),
        "aria2.tellStopped" => query::tell_stopped(client, params),
        "aria2.getGlobalStat" => query::global_stat(client),
        "aria2.getFiles" => query::files(client, params),
        "aria2.getUris" => query::uris(client, params),
        "aria2.getPeers" | "aria2.getServers" => Ok(json!([])),
        "aria2.getVersion" => Ok(json!({
            "version": "1.37.0",
            "enabledFeatures": ["BitTorrent", "Async DNS"],
        })),
        "aria2.getSessionInfo" => Ok(json!({ "sessionId": session })),
        "aria2.pause" | "aria2.forcePause" => control(client, params, "pause"),
        "aria2.unpause" => control(client, params, "resume"),
        "aria2.remove" | "aria2.forceRemove" => control(client, params, "cancel"),
        "aria2.pauseAll" | "aria2.forcePauseAll" => control_all(client, "pause"),
        "aria2.unpauseAll" => control_all(client, "resume"),
        "aria2.purgeDownloadResult" => purge(client),
        "aria2.removeDownloadResult" => remove_result(client, params),
        "aria2.getGlobalOption" => global_option(client),
        "aria2.changeGlobalOption" => change_global_option(client, params),
        "aria2.getOption" => query::option(client, params),
        "aria2.changeOption" => Err(RpcError::new(
            1,
            "changeOption is not supported; the limit was fixed when the task was added",
        )),
        "aria2.changePosition" => Err(RpcError::new(1, "changePosition is not supported")),
        "aria2.saveSession" => Ok(json!("OK")),
        "aria2.shutdown" | "aria2.forceShutdown" => {
            client.call("shutdown", json!({}))?;
            Ok(json!("OK"))
        }
        "system.multicall" => multicall(client, host, session, params),
        "system.listMethods" => Ok(json!(METHODS)),
        "system.listNotifications" => Ok(json!([
            "aria2.onDownloadStart",
            "aria2.onDownloadPause",
            "aria2.onDownloadStop",
            "aria2.onDownloadComplete",
            "aria2.onDownloadError",
        ])),
        other => Err(RpcError::new(-32601, format!("method not found: {other}"))),
    }
}

const METHODS: &[&str] = &[
    "aria2.addUri",
    "aria2.addTorrent",
    "aria2.addMetalink",
    "aria2.tellStatus",
    "aria2.tellActive",
    "aria2.tellWaiting",
    "aria2.tellStopped",
    "aria2.getGlobalStat",
    "aria2.getFiles",
    "aria2.getUris",
    "aria2.getPeers",
    "aria2.getServers",
    "aria2.getVersion",
    "aria2.getSessionInfo",
    "aria2.pause",
    "aria2.forcePause",
    "aria2.unpause",
    "aria2.remove",
    "aria2.forceRemove",
    "aria2.pauseAll",
    "aria2.forcePauseAll",
    "aria2.unpauseAll",
    "aria2.purgeDownloadResult",
    "aria2.removeDownloadResult",
    "aria2.getGlobalOption",
    "aria2.changeGlobalOption",
    "aria2.getOption",
    "aria2.saveSession",
    "aria2.shutdown",
    "aria2.forceShutdown",
    "system.multicall",
    "system.listMethods",
    "system.listNotifications",
];

fn add_uri(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let uris = params
        .first()
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::new(-32602, "addUri needs a URI array"))?;
    let url = uris
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "addUri needs at least one URI"))?;
    let (settings, dir, pause) = add_options(params.get(1).and_then(Value::as_object));
    let task_id = client.add(url, None, dir.as_deref(), settings)?;
    if pause {
        client.action("pause", &task_id)?;
    }
    Ok(json!(mapping::gid(&task_id)))
}

fn add_torrent(
    client: &mut DaemonClient,
    host: Option<&TorrentHost>,
    params: &[Value],
) -> RpcResult {
    let host = host.ok_or_else(|| RpcError::new(1, "torrent source unavailable"))?;
    let bytes = decode_payload(params.first(), "addTorrent needs a base64 torrent")?;
    let url = host
        .publish(bytes, "torrent", "application/x-bittorrent")
        .map_err(RpcError::from)?;
    // aria2's second parameter is a list of web-seed URIs; Aether adds the
    // inline torrent and ignores extra mirrors.
    let (settings, dir, pause) = add_options(params.get(2).and_then(Value::as_object));
    // Force the BT plugin: the generic HTTP plugin would otherwise grab the
    // .torrent URL and download the file instead of starting the torrent.
    let task_id = client.add(&url, Some("bt"), dir.as_deref(), settings)?;
    if pause {
        client.action("pause", &task_id)?;
    }
    Ok(json!(mapping::gid(&task_id)))
}

fn add_metalink(
    client: &mut DaemonClient,
    host: Option<&TorrentHost>,
    params: &[Value],
) -> RpcResult {
    let host = host.ok_or_else(|| RpcError::new(1, "torrent source unavailable"))?;
    let bytes = decode_payload(params.first(), "addMetalink needs base64 XML")?;
    let url = host
        .publish(bytes, "metalink", "application/metalink4+xml")
        .map_err(RpcError::from)?;
    let (settings, dir, pause) = add_options(params.get(1).and_then(Value::as_object));
    let task_id = client.add(&url, Some("http"), dir.as_deref(), settings)?;
    if pause {
        client.action("pause", &task_id)?;
    }
    Ok(json!(mapping::gid(&task_id)))
}

fn decode_payload(payload: Option<&Value>, message: &str) -> Result<Vec<u8>, RpcError> {
    let payload = payload
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, message))?;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|error| RpcError::new(1, format!("invalid base64 payload: {error}")))
}

fn add_options(options: Option<&serde_json::Map<String, Value>>) -> (Value, Option<String>, bool) {
    let mut settings = serde_json::Map::new();
    let mut dir = None;
    let mut pause = false;
    let Some(options) = options else {
        return (Value::Object(settings), dir, pause);
    };
    for (key, value) in options {
        match key.as_str() {
            "dir" => dir = value.as_str().map(str::to_string),
            "max-download-limit" => {
                if let Some(limit) = value.as_str().and_then(mapping::parse_size) {
                    settings.insert("max_speed_bps".into(), json!(limit));
                }
            }
            "referer" | "referrer" => {
                if let Some(referrer) = value.as_str() {
                    settings.insert("referrer".into(), json!(referrer));
                }
            }
            "pause" => pause = value.as_str() == Some("true"),
            "header" => {
                for header in value.as_array().into_iter().flatten() {
                    if let Some(header) = header.as_str() {
                        apply_header(&mut settings, header);
                    }
                }
            }
            _ => {}
        }
    }
    (Value::Object(settings), dir, pause)
}

fn apply_header(settings: &mut serde_json::Map<String, Value>, header: &str) {
    let Some((name, value)) = header.split_once(':') else {
        return;
    };
    match name.trim().to_ascii_lowercase().as_str() {
        "cookie" => {
            settings.insert("cookies".into(), json!(value.trim()));
        }
        "referer" | "referrer" => {
            settings.insert("referrer".into(), json!(value.trim()));
        }
        _ => {}
    }
}

fn control(client: &mut DaemonClient, params: &[Value], method: &str) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let task = query::resolve(client, gid)?;
    let task_id = task
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(1, format!("GID {gid} not found")))?;
    client.action(method, task_id)?;
    Ok(json!(gid))
}

fn control_all(client: &mut DaemonClient, method: &str) -> RpcResult {
    let (tasks, _) = client.list()?;
    for task in tasks {
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let wanted = match method {
            "pause" => matches!(state, "running" | "queued"),
            _ => state == "paused",
        };
        if !wanted {
            continue;
        }
        if let Some(id) = task.get("id").and_then(Value::as_str) {
            let _ = client.action(method, id);
        }
    }
    Ok(json!("OK"))
}

fn purge(client: &mut DaemonClient) -> RpcResult {
    let (tasks, _) = client.list()?;
    for task in tasks {
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(state, "completed" | "failed" | "canceled") {
            if let Some(id) = task.get("id").and_then(Value::as_str) {
                let _ = client.action("delete", id);
            }
        }
    }
    Ok(json!("OK"))
}

fn remove_result(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let task = query::resolve(client, gid)?;
    let task_id = task
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(1, format!("GID {gid} not found")))?;
    client.action("delete", task_id)?;
    Ok(json!("OK"))
}

fn global_option(client: &mut DaemonClient) -> RpcResult {
    let settings = client.call("get_settings", json!({}))?;
    let (_, dir) = client.list()?;
    let settings = settings.get("settings").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "dir": dir,
        "max-concurrent-downloads": settings
            .get("max_concurrent")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .to_string(),
        "max-overall-download-limit": settings
            .get("max_speed_bps")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .to_string(),
        "max-overall-upload-limit": "0",
    }))
}

fn change_global_option(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let options = params
        .first()
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::new(-32602, "changeGlobalOption needs an options object"))?;
    let mut patch = serde_json::Map::new();
    for (key, value) in options {
        match key.as_str() {
            "max-overall-download-limit" => {
                let limit = value
                    .as_str()
                    .and_then(mapping::parse_size)
                    .ok_or_else(|| RpcError::new(1, "invalid max-overall-download-limit"))?;
                patch.insert("max_speed_bps".into(), json!(limit));
            }
            "max-concurrent-downloads" => {
                let count = value
                    .as_str()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|count| *count > 0)
                    .ok_or_else(|| RpcError::new(1, "invalid max-concurrent-downloads"))?;
                patch.insert("max_concurrent".into(), json!(count));
            }
            // AriaNg and other front-ends send a full option object. Accept
            // unknown keys for compatibility; only the mapped ones have an
            // effect here (documented in SUPPORT_MATRIX.md).
            _ => {}
        }
    }
    if !patch.is_empty() {
        client.call("set_settings", Value::Object(patch))?;
    }
    Ok(json!("OK"))
}

fn multicall(
    client: &mut DaemonClient,
    host: Option<&TorrentHost>,
    session: &str,
    params: &[Value],
) -> RpcResult {
    let calls = params
        .first()
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::new(-32602, "multicall needs a call array"))?;
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        let method = call
            .get("methodName")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let call_params = call
            .get("params")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if method == "system.multicall" {
            results.push(json!({
                "error": { "code": -32600, "message": "nested multicall is not allowed" },
            }));
            continue;
        }
        results.push(
            match dispatch(client, host, session, method, &call_params) {
                Ok(result) => json!({ "result": result }),
                Err(error) => json!({
                    "error": { "code": error.code, "message": error.message },
                }),
            },
        );
    }
    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_inline_torrent_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"torrent-bytes");
        assert_eq!(
            decode_payload(Some(&json!(encoded)), "x").unwrap(),
            b"torrent-bytes"
        );
        assert!(decode_payload(Some(&json!("!!!")), "x").is_err());
        assert!(decode_payload(None, "x").is_err());
    }

    #[test]
    fn maps_the_supported_add_options() {
        let options = json!({
            "dir": "/tmp/out",
            "max-download-limit": "2M",
            "pause": "true",
            "referer": "https://example.test/",
            "header": ["Cookie: a=b"],
            "split": "16",
        });
        let (settings, dir, pause) = add_options(options.as_object());
        assert_eq!(dir.as_deref(), Some("/tmp/out"));
        assert!(pause);
        assert_eq!(settings["max_speed_bps"], json!(2 * 1024 * 1024));
        assert_eq!(settings["referrer"], "https://example.test/");
        assert_eq!(settings["cookies"], "a=b");
        // Unknown options are accepted and ignored.
        assert!(settings.get("split").is_none());
    }
}
