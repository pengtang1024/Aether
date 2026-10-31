//! Translation helpers between Aether task records and aria2 status objects.

use std::path::Path;

use serde_json::{json, Map, Value};

/// aria2 sends sizes as strings with optional `K`/`M`/`G` suffixes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, scale) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1024_u64),
        'm' | 'M' => (&text[..text.len() - 1], 1024_u64.pow(2)),
        'g' | 'G' => (&text[..text.len() - 1], 1024_u64.pow(3)),
        _ => (text, 1),
    };
    let value = number.parse::<f64>().ok()?;
    (value >= 0.0).then_some((value * scale as f64) as u64)
}

/// Aether task ids are `task-<nanoseconds>`; aria2 GIDs are 16 hex chars.
/// The suffix fits a u64, so the mapping is stable and reversible.
pub fn gid(task_id: &str) -> String {
    if let Some(suffix) = task_id.strip_prefix("task-") {
        if let Ok(value) = suffix.parse::<u64>() {
            return format!("{value:016x}");
        }
    }
    format!("{:016x}", fnv1a(task_id.as_bytes()))
}

pub fn task_id_from_gid(gid: &str) -> Option<String> {
    u64::from_str_radix(gid, 16)
        .ok()
        .map(|value| format!("task-{value}"))
}

pub fn status(state: &str) -> &'static str {
    match state {
        "running" => "active",
        "queued" => "waiting",
        "paused" => "paused",
        "completed" => "complete",
        "failed" => "error",
        "canceled" => "removed",
        _ => "waiting",
    }
}

/// Build the aria2 `tellStatus`-shaped object. `keys` matches aria2's
/// projection parameter: absent means the full object.
pub fn task_object(task: &Value, keys: Option<&[String]>, download_dir: &str) -> Value {
    let id = task.get("id").and_then(Value::as_str).unwrap_or_default();
    let state = task
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("queued");
    let filename = task
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let dir = task
        .get("target_dir")
        .and_then(Value::as_str)
        .filter(|dir| !dir.is_empty())
        .unwrap_or(download_dir);
    let files = task.get("files").and_then(Value::as_array);
    let total = task
        .get("total")
        .and_then(Value::as_u64)
        .or_else(|| {
            files.map(|files| {
                files
                    .iter()
                    .map(|file| file.get("size").and_then(Value::as_u64).unwrap_or(0))
                    .sum()
            })
        })
        .unwrap_or(0);
    let completed = task.get("downloaded").and_then(Value::as_u64).unwrap_or(0);
    let url = task.get("url").and_then(Value::as_str).unwrap_or_default();
    let completed_state = state == "completed";

    let file_objects = match files {
        Some(files) if !files.is_empty() => files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let relative = file
                    .get("relative_path")
                    .and_then(Value::as_str)
                    .unwrap_or(filename);
                let size = file.get("size").and_then(Value::as_u64).unwrap_or(0);
                json!({
                    "index": (index + 1).to_string(),
                    "path": Path::new(dir).join(relative).to_string_lossy(),
                    "length": size.to_string(),
                    "completedLength": if completed_state { size.to_string() } else { "0".into() },
                    "selected": "true",
                    "uris": [{ "uri": url, "status": "used" }],
                })
            })
            .collect::<Vec<_>>(),
        _ => vec![json!({
            "index": "1",
            "path": Path::new(dir).join(filename).to_string_lossy(),
            "length": total.to_string(),
            "completedLength": completed.to_string(),
            "selected": "true",
            "uris": [{ "uri": url, "status": "used" }],
        })],
    };

    let mut object = Map::new();
    object.insert("gid".into(), json!(gid(id)));
    object.insert("status".into(), json!(status(state)));
    object.insert("totalLength".into(), json!(total.to_string()));
    object.insert("completedLength".into(), json!(completed.to_string()));
    object.insert("uploadLength".into(), json!("0"));
    object.insert(
        "downloadSpeed".into(),
        json!(task
            .get("speed_bps")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .to_string()),
    );
    object.insert("uploadSpeed".into(), json!("0"));
    object.insert(
        "connections".into(),
        json!(task
            .get("settings")
            .and_then(|settings| settings.get("connections"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .to_string()),
    );
    object.insert("numSeeders".into(), json!("0"));
    object.insert("seeder".into(), json!("false"));
    object.insert("pieceLength".into(), json!("0"));
    object.insert("numPieces".into(), json!("0"));
    object.insert("bitfield".into(), json!(""));
    object.insert("dir".into(), json!(dir));
    object.insert("files".into(), Value::Array(file_objects));
    object.insert("followedBy".into(), json!([]));
    object.insert("following".into(), json!(""));
    object.insert("belongsTo".into(), json!(""));

    if let Some(message) = task.get("error_message").and_then(Value::as_str) {
        if !message.is_empty() {
            object.insert("errorCode".into(), json!("1"));
            object.insert("errorMessage".into(), json!(message));
        }
    }
    if task.get("plugin_id").and_then(Value::as_str) == Some("bt") {
        object.insert(
            "bittorrent".into(),
            json!({
                "announceList": [],
                "comment": "",
                "creationDate": "0",
                "mode": "multi",
                "info": { "name": filename },
            }),
        );
    }

    match keys {
        Some(keys) => {
            let wanted = keys
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            Value::Object(
                object
                    .into_iter()
                    .filter(|(key, _)| wanted.contains(key.as_str()))
                    .collect(),
            )
        }
        None => Value::Object(object),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
