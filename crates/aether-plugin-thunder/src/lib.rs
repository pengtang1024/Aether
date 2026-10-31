//! Decodes thunder:// links and delegates the real transfer to the HTTP engine.

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use base64::Engine;
use url::Url;

const PLUGIN_ID: &str = "thunder";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct ThunderPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl ThunderPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }
}

impl Default for ThunderPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ThunderPlugin {
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
                let (supported, url, filename, reason) = match decode(&input) {
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
                    protocol: supported.then(|| "thunder".into()),
                    priority: 30,
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
                let Ok(url) = decode(&task.url) else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Protocol,
                            "invalid thunder:// link",
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
            // Service/bridge commands target `kind: "service"` plugins
            // (aria2 compatibility); download plugins must not accept them.
            HostCommand::ServiceInit { .. } | HostCommand::BridgeCall { .. } => {
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

fn init(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("thunder command before Init".into()))
}

pub fn decode(input: &str) -> Result<String, String> {
    let payload = input
        .strip_prefix("thunder://")
        .ok_or_else(|| "not a thunder:// link".to_string())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload.trim()))
        .map_err(|error| format!("base64: {error}"))?;
    let text = String::from_utf8(decoded).map_err(|error| format!("utf-8: {error}"))?;
    let inner = text
        .strip_prefix("AA")
        .and_then(|value| value.strip_suffix("ZZ"))
        .unwrap_or(&text);
    let url = Url::parse(inner).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("thunder link does not contain an http(s) URL".into());
    }
    Ok(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_wrapped_thunder_url() {
        let original = "http://example.com/file.bin";
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("AA{original}ZZ"));
        assert_eq!(decode(&format!("thunder://{encoded}")).unwrap(), original);
    }
}
