//! Extractor plugin state machine.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError, TaskSpec,
};

use crate::extractor;

const PLUGIN_ID: &str = "extractor";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[
    Capability::Network,
    Capability::FilesystemTask,
    Capability::ExternalProcess,
];

pub struct ExtractorPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl ExtractorPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    fn yt_dlp(&self, task: &TaskSpec) -> String {
        task.settings
            .get("yt_dlp_path")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("AETHER_YTDLP").ok())
            .unwrap_or_else(|| "yt-dlp".into())
    }
}

impl Default for ExtractorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ExtractorPlugin {
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
                let executable = std::env::var("AETHER_YTDLP").unwrap_or_else(|_| "yt-dlp".into());
                let probe = extractor::probe(&input, &executable);
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: probe.supported,
                    protocol: probe.supported.then(|| "extractor".into()),
                    priority: 40,
                    metadata: probe.supported.then(|| Metadata {
                        final_url: input.clone(),
                        size: probe.size,
                        etag: None,
                        content_type: None,
                        files: vec![],
                    }),
                    suggested_filename: probe.filename,
                    reason: probe.reason,
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                let executable = self.yt_dlp(&task);
                let task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                thread::spawn(move || {
                    extractor::run(
                        sender,
                        task,
                        paths,
                        thread_control,
                        thread_progress,
                        executable,
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
                    connections: 1,
                    chunks_done: None,
                    chunks_total: None,
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
