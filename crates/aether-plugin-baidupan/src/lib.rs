//! Baidu Netdisk (official API) download plugin.
//!
//! Input: `baidupan://<fs_id>` (the numeric file id). OAuth `access_token` is
//! supplied via settings/env. The plugin calls `filemetas` with `dlink=1`,
//! then downloads the returned dlink with the required `pan.baidu.com` UA.
//! Shared-link scraping and private APIs are intentionally not implemented.

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin_http::client;
use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use url::Url;

const PLUGIN_ID: &str = "baidupan";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];
const DEFAULT_API: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";

pub struct BaiduPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl BaiduPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }
}

impl Default for BaiduPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for BaiduPlugin {
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
            HostCommand::Probe {
                request_id, input, ..
            } => {
                let fs_id = parse_fs_id(&input);
                let token = env_token();
                let (supported, filename, size, reason) = match (fs_id, token) {
                    (Some(fs_id), Some(token)) => {
                        let client = client::build_client(init(ctx)?)
                            .map_err(|error| SdkError::Protocol(error.to_string()))?;
                        let api = std::env::var("BAIDU_API_BASE")
                            .unwrap_or_else(|_| DEFAULT_API.to_string());
                        match resolve(&client, &fs_id, &token, &api) {
                            Ok(info) => (true, Some(info.name), Some(info.size), None),
                            Err(error) => (true, None, None, Some(error)),
                        }
                    }
                    (Some(_), None) => (
                        true,
                        None,
                        None,
                        Some("Baidu access_token is required".into()),
                    ),
                    (None, _) => (
                        false,
                        None,
                        None,
                        Some("expected baidupan://<fs_id>".into()),
                    ),
                };
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol: supported.then(|| "baidupan".into()),
                    priority: 55,
                    metadata: supported.then(|| Metadata {
                        final_url: input.clone(),
                        size,
                        etag: None,
                        content_type: None,
                        files: vec![],
                    }),
                    suggested_filename: filename,
                    reason,
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                let Some(fs_id) = parse_fs_id(&task.url) else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Protocol,
                            "invalid baidupan:// link",
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
                            "Baidu access_token is required",
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
                let resolved = match resolve(&client, &fs_id, &token, &api) {
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
                let download_client =
                    client::build_client_custom(&init, Some("pan.baidu.com"), None)
                        .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let mut task = task;
                task.url = with_token(&resolved.dlink, &token);
                task.filename = Some(resolved.name);
                task.expected_size = Some(resolved.size);

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
            // Service/bridge commands target `kind: "service"` plugins
            // (aria2 compatibility); download plugins must not accept them.
            HostCommand::ServiceInit { .. }
            | HostCommand::ServiceAddress { .. }
            | HostCommand::BridgeCall { .. } => {
                return Err(SdkError::Protocol(
                    "service commands are not supported by this plugin".into(),
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
    dlink: String,
    name: String,
    size: u64,
}

fn resolve(
    client: &reqwest::blocking::Client,
    fs_id: &str,
    token: &str,
    api: &str,
) -> Result<Resolved, String> {
    let mut url = Url::parse(api).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("method", "filemetas")
        .append_pair("access_token", token)
        .append_pair("fsids", &format!("[{fs_id}]"))
        .append_pair("dlink", "1");
    let response = client
        .get(url)
        .send()
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    if value
        .get("errno")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        != 0
    {
        return Err(format!("Baidu API error: {value}"));
    }
    let file = value
        .get("list")
        .and_then(serde_json::Value::as_array)
        .and_then(|list| list.first())
        .ok_or_else(|| "Baidu API returned no file".to_string())?;
    let dlink = file
        .get("dlink")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Baidu file has no dlink".to_string())?;
    let name = file
        .get("server_filename")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("baidu-file.bin")
        .to_string();
    let size = file
        .get("size")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(Resolved {
        dlink: dlink.to_string(),
        name,
        size,
    })
}

fn parse_fs_id(input: &str) -> Option<String> {
    let value = input.strip_prefix("baidupan://")?;
    (!value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())).then(|| value.to_string())
}

fn with_token(dlink: &str, token: &str) -> String {
    let separator = if dlink.contains('?') { '&' } else { '?' };
    format!("{dlink}{separator}access_token={token}")
}

fn env_token() -> Option<String> {
    std::env::var("BAIDU_ACCESS_TOKEN").ok()
}

fn init(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("baidupan command before Init".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fs_id() {
        assert_eq!(parse_fs_id("baidupan://123456").as_deref(), Some("123456"));
        assert!(parse_fs_id("https://pan.baidu.com/s/abc").is_none());
    }

    #[test]
    fn appends_access_token() {
        assert_eq!(
            with_token("https://d.pcs.baidu.com/file/x?a=1", "T"),
            "https://d.pcs.baidu.com/file/x?a=1&access_token=T"
        );
    }
}
