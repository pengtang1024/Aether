//! Aliyun Drive (official API) download plugin.
//!
//! Input: `aliyunpan://<drive_id>/<file_id>`. OAuth `access_token` comes from
//! settings/env. Calls `openFile/getDownloadUrl` and downloads the returned URL.
//! Shared-link scraping and private endpoints are not implemented.

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin_http::client;
use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};

const PLUGIN_ID: &str = "aliyunpan";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];
const DEFAULT_API: &str = "https://openapi.alipan.com";

pub struct AliyunPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl AliyunPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }
}

impl Default for AliyunPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for AliyunPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    fn on_command(
        &mut self,
        ctx: &mut PluginContext,
        command: HostCommand,
    ) -> Result<(), SdkError> {
        match command {
            HostCommand::Probe { request_id, input } => {
                let target = parse_target(&input);
                let token = env_token();
                let (supported, filename, reason) = match (target, token) {
                    (Some((drive_id, file_id)), Some(token)) => {
                        let client = client::build_client(init(ctx)?)
                            .map_err(|error| SdkError::Protocol(error.to_string()))?;
                        let api = std::env::var("ALIYUNPAN_API_BASE")
                            .unwrap_or_else(|_| DEFAULT_API.to_string());
                        match resolve(&client, &api, &drive_id, &file_id, &token) {
                            Ok(info) => (true, Some(info.name), None),
                            Err(error) => (true, None, Some(error)),
                        }
                    }
                    (Some(_), None) => (true, None, Some("Aliyun access_token is required".into())),
                    (None, _) => (
                        false,
                        None,
                        Some("expected aliyunpan://<drive_id>/<file_id>".into()),
                    ),
                };
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol: supported.then(|| "aliyunpan".into()),
                    priority: 55,
                    metadata: supported.then(|| Metadata {
                        final_url: input.clone(),
                        size: None,
                        etag: None,
                        content_type: None,
                        files: vec![],
                    }),
                    suggested_filename: filename,
                    reason,
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                let Some((drive_id, file_id)) = parse_target(&task.url) else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Protocol,
                            "invalid aliyunpan:// link",
                        ),
                    })?;
                    return Ok(());
                };
                let token = task
                    .settings
                    .get("access_token")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
                    .or_else(env_token);
                let Some(token) = token else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Auth,
                            "Aliyun access_token is required",
                        ),
                    })?;
                    return Ok(());
                };
                let api = task
                    .settings
                    .get("api_base")
                    .and_then(|value| value.as_str())
                    .unwrap_or(DEFAULT_API)
                    .to_string();
                let init = init(ctx)?.clone();
                let client = client::build_client(&init)
                    .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let resolved = match resolve(&client, &api, &drive_id, &file_id, &token) {
                    Ok(info) => info,
                    Err(error) => {
                        ctx.send(&PluginEvent::Failed {
                            task_id: task.task_id.clone(),
                            error: aether_plugin_sdk::PluginError::new(
                                aether_plugin_sdk::ErrorKind::Protocol,
                                error,
                            ),
                        })?;
                        return Ok(());
                    }
                };
                let download_client = client::build_client(&init)
                    .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let mut task = task;
                task.url = resolved.url;
                task.filename = Some(resolved.name);
                let task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                std::thread::spawn(move || {
                    aether_plugin_http::download::run(
                        sender,
                        download_client,
                        task,
                        paths,
                        aether_plugin_sdk::Budget::default(),
                        thread_control,
                        thread_progress,
                    );
                });
                self.tasks.insert(task_id, TaskRecord { control, progress });
            }
            HostCommand::Pause { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.pause();
                }
            }
            HostCommand::Resume { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.resume();
                }
            }
            HostCommand::Cancel { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.cancel();
                }
            }
            HostCommand::SetBudget { .. } => {}
            HostCommand::Snapshot { task_id, .. } => {
                let Some(record) = self.tasks.get(&task_id) else {
                    return Ok(());
                };
                let snapshot = record.progress.lock().unwrap().clone();
                ctx.send(&PluginEvent::Progress {
                    task_id,
                    seq: snapshot.seq,
                    downloaded: snapshot.downloaded,
                    total: snapshot.total,
                    speed_bps: snapshot.speed_bps,
                    eta_secs: None,
                    connections: snapshot.connections,
                    chunks_done: Some(snapshot.chunks_done),
                    chunks_total: Some(snapshot.chunks_total),
                })?;
            }
            HostCommand::Init { .. } => {
                return Err(SdkError::Protocol(
                    "init must be handled by the SDK runner".into(),
                ));
            }
            HostCommand::Ping { .. } | HostCommand::Shutdown => {
                return Err(SdkError::Protocol(
                    "ping/shutdown must be handled by the SDK runner".into(),
                ));
            }
        }
        Ok(())
    }

    fn on_shutdown(&mut self, _ctx: &mut PluginContext) -> Result<(), SdkError> {
        for record in self.tasks.values() {
            record.control.cancel();
        }
        Ok(())
    }
}

struct Resolved {
    url: String,
    name: String,
}

fn resolve(
    client: &reqwest::blocking::Client,
    api: &str,
    drive_id: &str,
    file_id: &str,
    token: &str,
) -> Result<Resolved, String> {
    let endpoint = format!(
        "{}/adrive/v1.0/openFile/getDownloadUrl",
        api.trim_end_matches('/')
    );
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "drive_id": drive_id,
            "file_id": file_id,
        }))
        .send()
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("Aliyun API returned no URL: {value}"))?;
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("aliyun-file.bin")
        .to_string();
    Ok(Resolved {
        url: url.to_string(),
        name,
    })
}

fn parse_target(input: &str) -> Option<(String, String)> {
    let value = input.strip_prefix("aliyunpan://")?;
    let (drive_id, file_id) = value.split_once('/')?;
    (!drive_id.is_empty() && !file_id.is_empty())
        .then(|| (drive_id.to_string(), file_id.to_string()))
}

fn env_token() -> Option<String> {
    std::env::var("ALIYUNPAN_TOKEN").ok()
}

fn init(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("aliyunpan command before Init".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target() {
        assert_eq!(
            parse_target("aliyunpan://drive/file").unwrap(),
            ("drive".into(), "file".into())
        );
        assert!(parse_target("https://alipan.com/s/x").is_none());
    }
}
