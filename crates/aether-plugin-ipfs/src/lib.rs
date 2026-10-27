//! IPFS/IPNS gateway plugin: resolves content to an HTTP gateway and reuses
//! the HTTP download engine.

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use url::Url;

const PLUGIN_ID: &str = "ipfs";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct IpfsPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl IpfsPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }
}

impl Default for IpfsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for IpfsPlugin {
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
                let gateway = gateway(&serde_json::Value::Null);
                let (supported, url, filename, reason) = match resolve(&input, &gateway) {
                    Ok(url) => {
                        let client = aether_plugin_http::client::build_client(init(ctx)?)
                            .map_err(|error| SdkError::Protocol(error.to_string()))?;
                        let probe = aether_plugin_http::client::probe(&client, &url);
                        (probe.supported, Some(url), probe.filename, probe.reason)
                    }
                    Err(error) => (false, None, None, Some(error)),
                };
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol: supported.then(|| "ipfs".into()),
                    priority: 50,
                    metadata: supported.then(|| Metadata {
                        final_url: url.clone().unwrap_or(input),
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
                let gateway = gateway(&task.settings);
                let Ok(url) = resolve(&task.url, &gateway) else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Protocol,
                            "invalid ipfs/ipns URL",
                        ),
                    })?;
                    return Ok(());
                };
                let client = aether_plugin_http::client::build_client(init(ctx)?)
                    .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let mut task = task;
                task.url = url;
                let task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                std::thread::spawn(move || {
                    aether_plugin_http::download::run(
                        sender,
                        client,
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

fn init(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("ipfs command before Init".into()))
}

fn gateway(settings: &serde_json::Value) -> String {
    settings
        .get("gateway")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("AETHER_IPFS_GATEWAY").ok())
        .unwrap_or_else(|| "https://ipfs.io".into())
}

pub fn resolve(input: &str, gateway: &str) -> Result<String, String> {
    let url = Url::parse(input).map_err(|error| error.to_string())?;
    let (ns, name) = match url.scheme() {
        "ipfs" => ("ipfs", url.host_str().unwrap_or_default()),
        "ipns" => ("ipns", url.host_str().unwrap_or_default()),
        other => return Err(format!("unsupported IPFS scheme {other}")),
    };
    if name.is_empty() {
        return Err("IPFS URL has no CID/name".into());
    }
    let path = url.path().trim_start_matches('/');
    let base = gateway.trim_end_matches('/');
    if path.is_empty() {
        Ok(format!("{base}/{ns}/{name}"))
    } else {
        Ok(format!("{base}/{ns}/{name}/{path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_gateway_urls() {
        assert_eq!(
            resolve("ipfs://bafy/file.bin", "https://gateway.example").unwrap(),
            "https://gateway.example/ipfs/bafy/file.bin"
        );
        assert_eq!(
            resolve("ipns://example.com", "https://gateway.example/").unwrap(),
            "https://gateway.example/ipns/example.com"
        );
    }
}
