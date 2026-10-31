//! Read-only aria2 queries over the daemon's task list.

use serde_json::{json, Value};

use crate::bridge::{RpcError, RpcResult};
use crate::client::DaemonClient;
use crate::mapping;

pub fn resolve(client: &mut DaemonClient, gid: &str) -> Result<Value, RpcError> {
    let (tasks, _) = client.list()?;
    if let Some(task) = tasks.iter().find(|task| {
        task.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| mapping::gid(id) == gid)
    }) {
        return Ok(task.clone());
    }
    // The list is capped; a deterministic GID still resolves older records.
    let task_id = mapping::task_id_from_gid(gid)
        .ok_or_else(|| RpcError::new(1, format!("GID {gid} not found")))?;
    client.task(&task_id).map_err(Into::into)
}

pub fn tell_status(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let keys = keys_param(params, 1);
    let task = resolve(client, gid)?;
    let (_, dir) = client.list()?;
    Ok(mapping::task_object(&task, keys.as_deref(), &dir))
}

pub fn tell_active(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    // tellActive's only argument is the optional key projection.
    let keys = keys_param(params, 0);
    let (tasks, dir) = client.list()?;
    Ok(Value::Array(
        tasks
            .iter()
            .filter(|task| task.get("state").and_then(Value::as_str) == Some("running"))
            .map(|task| mapping::task_object(task, keys.as_deref(), &dir))
            .collect(),
    ))
}

pub fn tell_waiting(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let offset = number(params, 0, 0);
    let num = number(params, 1, 1000);
    let keys = keys_param(params, 2);
    let (mut tasks, dir) = client.list()?;
    tasks.retain(|task| {
        matches!(
            task.get("state").and_then(Value::as_str),
            Some("queued" | "paused")
        )
    });
    tasks.sort_by(|left, right| {
        let priority = |task: &Value| task.get("priority").and_then(Value::as_i64).unwrap_or(0);
        let created = |task: &Value| task.get("created_at").and_then(Value::as_i64).unwrap_or(0);
        priority(right)
            .cmp(&priority(left))
            .then(created(left).cmp(&created(right)))
    });
    Ok(page(&tasks, offset, num, keys.as_deref(), &dir))
}

pub fn tell_stopped(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let offset = number(params, 0, 0);
    let num = number(params, 1, 1000);
    let keys = keys_param(params, 2);
    let (mut tasks, dir) = client.list()?;
    tasks.retain(|task| {
        matches!(
            task.get("state").and_then(Value::as_str),
            Some("completed" | "failed" | "canceled")
        )
    });
    tasks.sort_by_key(|task| {
        std::cmp::Reverse(task.get("updated_at").and_then(Value::as_i64).unwrap_or(0))
    });
    Ok(page(&tasks, offset, num, keys.as_deref(), &dir))
}

pub fn global_stat(client: &mut DaemonClient) -> RpcResult {
    let (tasks, _) = client.list()?;
    let count = |states: &[&str]| {
        tasks
            .iter()
            .filter(|task| {
                task.get("state")
                    .and_then(Value::as_str)
                    .is_some_and(|state| states.contains(&state))
            })
            .count()
            .to_string()
    };
    let speed: u64 = tasks
        .iter()
        .filter(|task| task.get("state").and_then(Value::as_str) == Some("running"))
        .filter_map(|task| task.get("speed_bps").and_then(Value::as_u64))
        .sum();
    Ok(json!({
        "downloadSpeed": speed.to_string(),
        "uploadSpeed": "0",
        "numActive": count(&["running"]),
        "numWaiting": count(&["queued", "paused"]),
        "numStopped": count(&["completed", "failed", "canceled"]),
        "numStoppedTotal": count(&["completed", "failed", "canceled"]),
    }))
}

pub fn files(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let task = resolve(client, gid)?;
    let (_, dir) = client.list()?;
    let object = mapping::task_object(&task, Some(&["files".to_string()]), &dir);
    Ok(object.get("files").cloned().unwrap_or_else(|| json!([])))
}

pub fn uris(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let task = resolve(client, gid)?;
    let url = task.get("url").and_then(Value::as_str).unwrap_or_default();
    Ok(json!([{ "uri": url, "status": "used" }]))
}

pub fn option(client: &mut DaemonClient, params: &[Value]) -> RpcResult {
    let gid = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing gid"))?;
    let task = resolve(client, gid)?;
    let settings = task.get("settings").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "max-download-limit": settings
            .get("max_speed_bps")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .to_string(),
        "max-upload-limit": "0",
    }))
}

fn page(tasks: &[Value], offset: usize, num: usize, keys: Option<&[String]>, dir: &str) -> Value {
    Value::Array(
        tasks
            .iter()
            .skip(offset)
            .take(num)
            .map(|task| mapping::task_object(task, keys, dir))
            .collect(),
    )
}

fn number(params: &[Value], index: usize, default: usize) -> usize {
    params
        .get(index)
        .and_then(|value| {
            value
                .as_u64()
                .map(|value| value as usize)
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(default)
}

fn keys_param(params: &[Value], index: usize) -> Option<Vec<String>> {
    params
        .get(index)
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .filter(|keys: &Vec<String>| !keys.is_empty())
}
