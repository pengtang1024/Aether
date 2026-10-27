//! Minimal synchronous client for the daemon socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::proto::{Request, Response};

#[derive(Debug, Clone, Deserialize)]
pub struct AddResult {
    pub task_id: String,
    pub plugin_id: String,
    /// `false` when the task started immediately, `true` when it waits for a
    /// concurrency slot.
    #[serde(default)]
    pub queued: bool,
    /// Directory the files will land in (absolute).
    #[serde(default)]
    pub dir: Option<String>,
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Self, DaemonError> {
        let stream = UnixStream::connect(socket)?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
        })
    }

    pub fn call(
        &mut self,
        method: &str,
        url: Option<&str>,
        plugin: Option<&str>,
        dir: Option<&str>,
        task_id: Option<&str>,
        settings: Value,
    ) -> Result<Value, DaemonError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let request = Request {
            id,
            method: method.to_string(),
            url: url.map(ToOwned::to_owned),
            plugin: plugin.map(ToOwned::to_owned),
            dir: dir.map(ToOwned::to_owned),
            task_id: task_id.map(ToOwned::to_owned),
            settings,
        };
        let line = serde_json::to_string(&request)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        let mut response = String::new();
        if self.reader.read_line(&mut response)? == 0 {
            return Err(DaemonError::Protocol("daemon closed the socket".into()));
        }
        let response: Response = serde_json::from_str(&response)?;
        if !response.ok {
            return Err(DaemonError::Protocol(
                response.error.unwrap_or_else(|| "request failed".into()),
            ));
        }
        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn ping(&mut self) -> Result<(), DaemonError> {
        self.call("ping", None, None, None, None, json!({}))?;
        Ok(())
    }

    pub fn add(
        &mut self,
        url: &str,
        dir: Option<&Path>,
        plugin: Option<&str>,
    ) -> Result<Value, DaemonError> {
        self.add_with_settings(url, dir, plugin, json!({}))
    }

    pub fn add_with_settings(
        &mut self,
        url: &str,
        dir: Option<&Path>,
        plugin: Option<&str>,
        settings: Value,
    ) -> Result<Value, DaemonError> {
        let dir = dir.map(|path| path.to_string_lossy().into_owned());
        self.call("add", Some(url), plugin, dir.as_deref(), None, settings)
    }

    pub fn list(&mut self) -> Result<Value, DaemonError> {
        self.call("list", None, None, None, None, json!({}))
    }

    pub fn list_tasks(&mut self) -> Result<Vec<aether_host::StoredTask>, DaemonError> {
        let value = self.list()?;
        Ok(serde_json::from_value(
            value
                .get("tasks")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        )?)
    }

    pub fn task(&mut self, task_id: &str) -> Result<Value, DaemonError> {
        self.call("get", None, None, None, Some(task_id), json!({}))
    }

    pub fn task_record(&mut self, task_id: &str) -> Result<aether_host::StoredTask, DaemonError> {
        let value = self.task(task_id)?;
        Ok(serde_json::from_value(
            value.get("task").cloned().unwrap_or(Value::Null),
        )?)
    }

    /// Task log lines, oldest first (this is what the UI shows in a task
    /// detail pane). `limit` keeps the newest lines.
    pub fn logs(
        &mut self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<aether_host::TaskLog>, DaemonError> {
        let value = self.call(
            "logs",
            None,
            None,
            None,
            Some(task_id),
            json!({ "limit": limit }),
        )?;
        Ok(serde_json::from_value(
            value
                .get("logs")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        )?)
    }

    pub fn add_task(
        &mut self,
        url: &str,
        dir: Option<&Path>,
        plugin: Option<&str>,
        settings: Value,
    ) -> Result<AddResult, DaemonError> {
        let value = self.add_with_settings(url, dir, plugin, settings)?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn pause(&mut self, task_id: &str) -> Result<Value, DaemonError> {
        self.call("pause", None, None, None, Some(task_id), json!({}))
    }

    pub fn resume(&mut self, task_id: &str) -> Result<Value, DaemonError> {
        self.call("resume", None, None, None, Some(task_id), json!({}))
    }

    pub fn cancel(&mut self, task_id: &str) -> Result<Value, DaemonError> {
        self.call("cancel", None, None, None, Some(task_id), json!({}))
    }

    /// Delete a terminal task (record + staging); downloaded files stay.
    pub fn delete(&mut self, task_id: &str) -> Result<Value, DaemonError> {
        self.call("delete", None, None, None, Some(task_id), json!({}))
    }

    pub fn shutdown(&mut self) -> Result<(), DaemonError> {
        self.call("shutdown", None, None, None, None, json!({}))?;
        Ok(())
    }

    /// Daemon settings, currently `{"proxy": {...}}` (or an empty object when
    /// no proxy is configured).
    pub fn settings(&mut self) -> Result<Value, DaemonError> {
        let value = self.call("get_settings", None, None, None, None, json!({}))?;
        Ok(value.get("settings").cloned().unwrap_or(Value::Null))
    }

    /// Ask the daemon which proxy it would use for `url` (PAC diagnostics).
    pub fn resolve_proxy(&mut self, url: &str) -> Result<Value, DaemonError> {
        self.call("resolve_proxy", Some(url), None, None, None, json!({}))
    }

    /// Partial settings update: only the keys present change. Pass
    /// `{"proxy": null}` to clear the proxy. Returns the stored settings.
    pub fn set_settings(&mut self, settings: Value) -> Result<Value, DaemonError> {
        let value = self.call("set_settings", None, None, None, None, settings)?;
        Ok(value.get("settings").cloned().unwrap_or(Value::Null))
    }

    pub fn update_status(&mut self) -> Result<Value, DaemonError> {
        self.call("update_status", None, None, None, None, json!({}))
    }

    /// Consume the client and open a live event subscription.
    pub fn subscribe(mut self) -> Result<Subscription, DaemonError> {
        let id = self.next_id;
        let request = Request {
            id,
            method: "subscribe".into(),
            url: None,
            plugin: None,
            dir: None,
            task_id: None,
            settings: json!({}),
        };
        let line = serde_json::to_string(&request)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        let mut response = String::new();
        if self.reader.read_line(&mut response)? == 0 {
            return Err(DaemonError::Protocol("daemon closed the socket".into()));
        }
        let response: Response = serde_json::from_str(&response)?;
        if !response.ok {
            return Err(DaemonError::Protocol(
                response.error.unwrap_or_else(|| "subscribe failed".into()),
            ));
        }

        let (sender, events) = mpsc::channel();
        let reader = self.reader;
        thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Subscription {
            _writer: self.writer,
            events,
        })
    }
}

pub struct Subscription {
    _writer: UnixStream,
    events: Receiver<Value>,
}

impl Subscription {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Value, DaemonError> {
        self.events
            .recv_timeout(timeout)
            .map_err(|_| DaemonError::Protocol("subscription timeout".into()))
    }
}

pub fn default_socket() -> PathBuf {
    if let Ok(value) = std::env::var("AETHER_SOCKET") {
        return PathBuf::from(value);
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("aether").join("daemon.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    std::env::temp_dir().join(format!("aether-{user}.sock"))
}
