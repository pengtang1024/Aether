//! The FTP plugin state machine.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Init, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
    TaskPaths, TaskSpec,
};

use crate::ftp;

const PLUGIN_ID: &str = "ftp";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct FtpPlugin {
    tasks: HashMap<String, TaskRecord>,
    /// Data connection mode from the daemon's `NetworkConfig`; passive is the
    /// default because active mode needs an inbound path to the client.
    passive: bool,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl FtpPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            passive: true,
        }
    }

    fn start(&mut self, ctx: &mut PluginContext, task: TaskSpec, paths: TaskPaths) {
        let task_id = task.task_id.clone();
        let sender = ctx.sender();
        let control = Arc::new(TaskControl::new());
        let progress = shared_progress();
        let thread_control = control.clone();
        let thread_progress = progress.clone();
        let passive = self.passive;

        thread::spawn(move || {
            ftp::run(
                sender,
                task,
                paths,
                thread_control,
                thread_progress,
                passive,
            );
        });

        self.tasks.insert(task_id, TaskRecord { control, progress });
    }
}

impl Default for FtpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for FtpPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    fn on_init(&mut self, _ctx: &mut PluginContext, init: &Init) -> Result<(), SdkError> {
        self.passive = init
            .network
            .as_ref()
            .and_then(|network| network.ftp_passive)
            .unwrap_or(true);
        Ok(())
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
                let probe = ftp::probe(&input, self.passive);
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: probe.supported,
                    protocol: probe.protocol,
                    priority: 80,
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
                self.start(ctx, task, paths);
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
                    chunks_total: Some(1),
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
