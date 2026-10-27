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
    /// Ask whether a URL / file is supported. `proxy` is the Host-resolved
    /// proxy for this input (PAC), so probes use the same route as the task.
    Probe {
        request_id: u64,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy: Option<ProxyConfig>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

impl ProxyMode {
    /// Parse the mode names used by settings files and `AETHER_PROXY`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" => Ok(Self::System),
            "none" | "direct" => Ok(Self::Direct),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "unknown proxy mode {other:?} (expected system, none or a proxy URL)"
            )),
        }
    }
}

impl ProxyConfig {
    pub fn system() -> Self {
        Self {
            mode: ProxyMode::System,
            url: None,
            username: None,
            password: None,
        }
    }

    /// No proxy at all; unlike `system` this ignores `HTTP_PROXY`-style
    /// environment variables.
    pub fn direct() -> Self {
        Self {
            mode: ProxyMode::Direct,
            url: None,
            username: None,
            password: None,
        }
    }

    /// Reject configurations that would only fail later inside a plugin.
    pub fn validate(&self) -> Result<(), String> {
        let url = match self.mode {
            ProxyMode::Custom => self
                .url
                .as_deref()
                .ok_or_else(|| "custom proxy needs a URL".to_string())?,
            ProxyMode::System | ProxyMode::Direct => {
                if self.url.is_some() {
                    return Err("system/none proxies must not carry a URL".into());
                }
                return Ok(());
            }
        };
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| format!("proxy URL needs a scheme: {url:?}"))?;
        if !matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "socks5" | "socks5h"
        ) {
            return Err(format!("unsupported proxy scheme {scheme:?}"));
        }
        if rest.is_empty() || !rest.contains(':') || rest.contains('@') {
            return Err(format!("proxy URL needs host:port: {url:?}"));
        }
        Ok(())
    }

    /// Parse a one-line proxy spec:
    ///
    /// - `system` — honor the platform environment (`HTTP_PROXY`, `ALL_PROXY`, …)；
    /// - `none` / `direct` — never use a proxy；
    /// - `scheme://[user:password@]host:port` — an explicit proxy. Supported
    ///   schemes are `http`, `https`, `socks5` and `socks5h`.
    ///
    /// Credentials are split out of the URL because plugins hand them to
    /// reqwest separately. Percent-encoding in credentials is *not* decoded:
    /// use a settings object with `username`/`password` for exotic passwords.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty proxy spec".into());
        }
        if !spec.contains("://") {
            return match ProxyMode::parse(spec)? {
                ProxyMode::System => Ok(Self::system()),
                ProxyMode::Direct => Ok(Self::direct()),
                // "custom" without a URL is meaningless.
                ProxyMode::Custom => Err("custom proxy needs a URL".into()),
            };
        }

        let (scheme, rest) = spec
            .split_once("://")
            .ok_or_else(|| format!("invalid proxy URL {spec:?}"))?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h") {
            return Err(format!("unsupported proxy scheme {scheme:?}"));
        }
        if rest.contains('/') || rest.contains('?') || rest.contains('#') {
            return Err(format!(
                "proxy URL must not contain a path, query or fragment: {spec:?}"
            ));
        }

        let (credentials, authority) = match rest.rsplit_once('@') {
            Some((credentials, authority)) => (Some(credentials), authority),
            None => (None, rest),
        };
        if authority.is_empty() || !authority.contains(':') {
            return Err(format!("proxy URL needs host:port: {spec:?}"));
        }
        let (username, password) = match credentials {
            Some(credentials) => match credentials.split_once(':') {
                Some((username, password)) => {
                    if username.is_empty() {
                        return Err("proxy username must not be empty".into());
                    }
                    (Some(username.to_string()), Some(password.to_string()))
                }
                None => (Some(credentials.to_string()), None),
            },
            None => (None, None),
        };

        let config = Self {
            mode: ProxyMode::Custom,
            url: Some(format!("{scheme}://{authority}")),
            username,
            password,
        };
        config.validate()?;
        Ok(config)
    }
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
    /// Proxy resolved by the Host for this task (PAC evaluation). Wins over the
    /// process-level `Init::proxy`; `None` keeps the process default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
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
                proxy: None,
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
    fn parses_proxy_specs() {
        assert_eq!(ProxyConfig::parse("system").unwrap(), ProxyConfig::system());
        assert_eq!(ProxyConfig::parse(" NONE ").unwrap(), ProxyConfig::direct());
        assert_eq!(ProxyConfig::parse("direct").unwrap(), ProxyConfig::direct());

        let http = ProxyConfig::parse("http://10.0.0.1:3128").unwrap();
        assert_eq!(http.mode, ProxyMode::Custom);
        assert_eq!(http.url.as_deref(), Some("http://10.0.0.1:3128"));
        assert_eq!(http.username, None);

        let socks = ProxyConfig::parse("SOCKS5://user:p%40ss@127.0.0.1:1080").unwrap();
        assert_eq!(socks.mode, ProxyMode::Custom);
        assert_eq!(socks.url.as_deref(), Some("socks5://127.0.0.1:1080"));
        assert_eq!(socks.username.as_deref(), Some("user"));
        // Credentials are passed through verbatim; percent-decoding is the
        // caller's job (settings objects carry raw username/password).
        assert_eq!(socks.password.as_deref(), Some("p%40ss"));

        assert!(ProxyConfig::parse("").is_err());
        assert!(ProxyConfig::parse("custom").is_err());
        assert!(ProxyConfig::parse("ftp://10.0.0.1:21").is_err());
        assert!(ProxyConfig::parse("http://10.0.0.1").is_err());
        assert!(ProxyConfig::parse("http://10.0.0.1:3128/pac").is_err());
        assert!(ProxyConfig::parse("http://:pass@10.0.0.1:3128").is_err());

        // Handler-level validation catches settings objects that bypass parse.
        assert!(ProxyConfig::system().validate().is_ok());
        assert!(ProxyConfig::direct().validate().is_ok());
        let mut custom = ProxyConfig::parse("http://10.0.0.1:3128").unwrap();
        assert!(custom.validate().is_ok());
        custom.url = None;
        assert!(custom.validate().is_err());
        custom.url = Some("http://10.0.0.1:3128".into());
        custom.mode = ProxyMode::System;
        assert!(custom.validate().is_err());
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
