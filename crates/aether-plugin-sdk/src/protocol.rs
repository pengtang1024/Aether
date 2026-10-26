//! Typed Host <-> plugin messages.
//!
//! Every message is serialized as a MessagePack map with a `type` tag. The
//! framing (length prefix) lives in [`crate::ipc`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// IPC/API major version. Bump only for breaking protocol changes.
pub const API_VERSION: u16 = 1;

/// Host-to-plugin commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostCommand {
    /// Sent once after the plugin's `Hello`. Carries host-wide configuration.
    Init {
        host_version: String,
        data_dir: String,
        log_level: LogLevel,
        proxy: Option<ProxyConfig>,
        limits: Limits,
    },
    /// Ask whether a URL / file is supported.
    Probe {
        request_id: u64,
        input: String,
    },
    /// Start a task. `task.plugin_state` is used for resume.
    Start {
        request_id: u64,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    },
    Pause {
        task_id: String,
    },
    Resume {
        task_id: String,
    },
    Cancel {
        task_id: String,
    },
    SetBudget {
        task_id: String,
        budget: Budget,
    },
    /// Ask for a fresh progress/detail snapshot.
    Snapshot {
        request_id: u64,
        task_id: String,
    },
    Ping {
        request_id: u64,
    },
    Shutdown,
}

/// Plugin-to-host events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginEvent {
    /// First frame after process start.
    Hello {
        api: u16,
        plugin_id: String,
        version: String,
        capabilities: Vec<Capability>,
    },
    /// Handshake complete, after `Init` has been applied.
    Ready {
        api: u16,
    },
    /// Response to [`HostCommand::Probe`].
    ProbeResult {
        request_id: u64,
        supported: bool,
        protocol: Option<String>,
        priority: i32,
        metadata: Option<Metadata>,
        suggested_filename: Option<String>,
        reason: Option<String>,
    },
    /// Final metadata after redirects / content negotiation.
    Metadata {
        task_id: String,
        metadata: Metadata,
    },
    Progress {
        task_id: String,
        seq: u64,
        downloaded: u64,
        total: Option<u64>,
        speed_bps: u64,
        eta_secs: Option<u64>,
        connections: u32,
        chunks_done: Option<u32>,
        chunks_total: Option<u32>,
    },
    /// Protocol-specific detail data, rendered by the Host using the manifest's
    /// `detail_schema`.
    Detail {
        task_id: String,
        data: serde_json::Value,
    },
    Log {
        level: LogLevel,
        message: String,
        fields: BTreeMap<String, serde_json::Value>,
    },
    Completed {
        task_id: String,
        files: Vec<FileInfo>,
        plugin_state: Option<serde_json::Value>,
    },
    Failed {
        task_id: String,
        error: PluginError,
    },
    Heartbeat {
        seq: u64,
        unix_ms: u64,
    },
    Pong {
        request_id: u64,
        unix_ms: u64,
    },
}

/// Capabilities a plugin must declare in its manifest. The Host uses this to
/// decide whether a plugin may use the network, task storage or external
/// processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "filesystem:task")]
    FilesystemTask,
    #[serde(rename = "external-process")]
    ExternalProcess,
    #[serde(rename = "clipboard")]
    Clipboard,
    #[serde(rename = "http3")]
    Http3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    System,
    #[serde(rename = "none")]
    Direct,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Init {
    pub host_version: String,
    pub data_dir: String,
    pub log_level: LogLevel,
    pub proxy: Option<ProxyConfig>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_connections: u32,
    pub download_bps: u64,
    pub upload_bps: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 8,
            download_bps: 0,
            upload_bps: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub connections: u32,
    pub per_host_connections: u32,
    pub download_bps: u64,
    pub upload_bps: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            connections: 4,
            per_host_connections: 4,
            download_bps: 0,
            upload_bps: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task_id: String,
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub settings: serde_json::Value,
    #[serde(default)]
    pub plugin_state: Option<serde_json::Value>,
    #[serde(default)]
    pub expected_size: Option<u64>,
    #[serde(default)]
    pub checksum: Option<Checksum>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPaths {
    /// The only directory a plugin may write to.
    pub staging: PathBuf,
    /// Host-owned final directory; the plugin must not write here.
    pub final_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// Path relative to the task's staging directory.
    pub relative_path: String,
    pub size: u64,
    #[serde(default)]
    pub checksum: Option<Checksum>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algorithm: ChecksumAlgorithm,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChecksumAlgorithm {
    Sha256,
    Blake3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub final_url: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub diagnostic_id: Option<String>,
}

impl PluginError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            http_status: None,
            retryable: false,
            diagnostic_id: None,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Dns,
    Connect,
    Tls,
    Timeout,
    HttpStatus,
    Auth,
    RangeMismatch,
    Checksum,
    DiskFull,
    Permission,
    Protocol,
    Canceled,
    RemoteChanged,
    Internal,
}

/// Convenience response type for [`HostCommand::Probe`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub request_id: u64,
    pub supported: bool,
    pub protocol: Option<String>,
    pub priority: i32,
    pub metadata: Option<Metadata>,
    pub suggested_filename: Option<String>,
    pub reason: Option<String>,
}

impl From<ProbeResult> for PluginEvent {
    fn from(result: ProbeResult) -> Self {
        Self::ProbeResult {
            request_id: result.request_id,
            supported: result.supported,
            protocol: result.protocol,
            priority: result.priority,
            metadata: result.metadata,
            suggested_filename: result.suggested_filename,
            reason: result.reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_command_roundtrips_through_messagepack() {
        let command = HostCommand::Start {
            request_id: 7,
            task: TaskSpec {
                task_id: "t1".into(),
                url: "https://example.com/file.bin".into(),
                filename: Some("file.bin".into()),
                settings: serde_json::json!({"connections": 4}),
                plugin_state: None,
                expected_size: Some(1024),
                checksum: None,
            },
            paths: TaskPaths {
                staging: PathBuf::from("/tmp/staging"),
                final_dir: PathBuf::from("/tmp/final"),
            },
            budget: Budget::default(),
        };

        let encoded = rmp_serde::to_vec_named(&command).unwrap();
        let decoded: HostCommand = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, command);
    }

    #[test]
    fn plugin_event_roundtrips_through_messagepack() {
        let event = PluginEvent::Progress {
            task_id: "t1".into(),
            seq: 3,
            downloaded: 512,
            total: Some(1024),
            speed_bps: 4096,
            eta_secs: Some(1),
            connections: 2,
            chunks_done: Some(1),
            chunks_total: Some(4),
        };

        let encoded = rmp_serde::to_vec_named(&event).unwrap();
        let decoded: PluginEvent = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn capability_uses_manifest_notation() {
        let encoded = rmp_serde::to_vec_named(&Capability::FilesystemTask).unwrap();
        let decoded: Capability = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, Capability::FilesystemTask);
        assert_eq!(
            serde_json::to_string(&Capability::FilesystemTask).unwrap(),
            "\"filesystem:task\""
        );
    }
}
