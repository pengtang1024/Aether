//! Minimal newline-JSON client for the daemon's trusted local endpoint.
//!
//! The compatibility plugin never opens its own listening socket: the daemon
//! forwards aria2 requests over the plugin IPC channel, and this client turns
//! them into native daemon calls.

use std::io::{BufRead, BufReader, Read, Write};

use serde_json::{json, Value};

pub struct DaemonClient {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    next_id: u64,
}

impl DaemonClient {
    pub fn connect(socket: &str) -> Result<Self, String> {
        let (reader, writer) = connect_stream(socket)
            .map_err(|error| format!("cannot reach the daemon at {socket}: {error}"))?;
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    /// One request/response round-trip. `extra` holds the optional request
    /// fields (`url`, `plugin`, `dir`, `task_id`, `priority`, `settings`).
    pub fn call(&mut self, method: &str, extra: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut request = json!({ "id": id, "method": method });
        if let (Some(object), Some(extra)) = (request.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        let mut line = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("daemon write failed: {error}"))?;

        let mut response = String::new();
        self.reader
            .read_line(&mut response)
            .map_err(|error| format!("daemon read failed: {error}"))?;
        if response.trim().is_empty() {
            return Err("daemon closed the connection".into());
        }
        let response: Value =
            serde_json::from_str(&response).map_err(|error| format!("daemon reply: {error}"))?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon request failed")
            .to_string())
    }

    pub fn list(&mut self) -> Result<(Vec<Value>, String), String> {
        self.list_with_files(true)
    }

    /// Same rows without the per-task `task_files` query (queue/history
    /// listings only need filenames and state).
    pub fn list_basic(&mut self) -> Result<(Vec<Value>, String), String> {
        self.list_with_files(false)
    }

    fn list_with_files(&mut self, include_files: bool) -> Result<(Vec<Value>, String), String> {
        // aria2 clients can page through the full history; the UI keeps its
        // own 100-row default because it calls `list` without a limit.
        let result = self.call(
            "list",
            json!({ "settings": { "limit": 10_000, "include_files": include_files } }),
        )?;
        let tasks = result
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let dir = result
            .get("download_dir")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok((tasks, dir))
    }

    pub fn task(&mut self, task_id: &str) -> Result<Value, String> {
        let result = self.call("get", json!({ "task_id": task_id }))?;
        result
            .get("task")
            .cloned()
            .ok_or_else(|| format!("task {task_id} disappeared"))
    }

    pub fn add(
        &mut self,
        url: &str,
        plugin: Option<&str>,
        dir: Option<&str>,
        settings: Value,
    ) -> Result<String, String> {
        let mut extra = json!({ "url": url, "settings": settings });
        if let Some(plugin) = plugin {
            extra["plugin"] = json!(plugin);
        }
        if let Some(dir) = dir {
            extra["dir"] = json!(dir);
        }
        let result = self.call("add", extra)?;
        result
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "daemon did not return a task id".into())
    }

    pub fn action(&mut self, method: &str, task_id: &str) -> Result<(), String> {
        self.call(method, json!({ "task_id": task_id })).map(|_| ())
    }
}

#[cfg(unix)]
fn connect_stream(socket: &str) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    let stream = std::os::unix::net::UnixStream::connect(socket)?;
    let reader = stream.try_clone()?;
    Ok((Box::new(reader), Box::new(stream)))
}

#[cfg(windows)]
fn connect_stream(socket: &str) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(socket)?;
    let reader = file.try_clone()?;
    Ok((Box::new(reader), Box::new(file)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn size_suffixes_are_parsed() {
        assert_eq!(super::super::mapping::parse_size("1024"), Some(1024));
        assert_eq!(super::super::mapping::parse_size("2K"), Some(2048));
        assert_eq!(
            super::super::mapping::parse_size("1.5M"),
            Some(1024 * 1024 * 3 / 2)
        );
        assert_eq!(super::super::mapping::parse_size("bad"), None);
    }
}
