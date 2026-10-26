//! The HTTP plugin state machine.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use aether_plugin_sdk::{
    Budget, Capability, HostCommand, Init, LogLevel, Metadata, Plugin, PluginContext, PluginEvent,
    SdkError, TaskPaths, TaskSpec,
};

use crate::client;
use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};

use crate::download;

const PLUGIN_ID: &str = "http";
const PLUGIN_VERSION: &str = "0.1.0";

const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct HttpPlugin {
    client: Option<reqwest::blocking::Client>,
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
    budget: Budget,
}

impl HttpPlugin {
    pub fn new() -> Self {
        Self {
            client: None,
            tasks: HashMap::new(),
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, SdkError> {
        self.client
            .clone()
            .ok_or_else(|| SdkError::Protocol("plugin received a command before Init".into()))
    }

    fn start(
        &mut self,
        ctx: &mut PluginContext,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    ) -> Result<(), SdkError> {
        let client = self.client()?;
        let task_id = task.task_id.clone();
        let sender = ctx.sender();
        let control = Arc::new(TaskControl::new());
        let progress: SharedProgress = shared_progress();

        let thread_control = control.clone();
        let thread_progress = progress.clone();
        thread::spawn(move || {
            download::run(
                sender,
                client,
                task,
                paths,
                budget,
                thread_control,
                thread_progress,
            );
        });

        self.tasks.insert(
            task_id,
            TaskRecord {
                control,
                progress,
                budget,
            },
        );
        Ok(())
    }
}

impl Default for HttpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for HttpPlugin {
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
        self.client = Some(
            client::build_client(init)
                .map_err(|error| SdkError::Protocol(format!("http client init: {error}")))?,
        );
        Ok(())
    }

    fn on_command(
        &mut self,
        ctx: &mut PluginContext,
        command: HostCommand,
    ) -> Result<(), SdkError> {
        match command {
            HostCommand::Probe { request_id, input } => {
                let client = self.client()?;
                let info = client::probe(&client, &input);

                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: info.supported,
                    protocol: info.protocol,
                    priority: 100,
                    metadata: info.supported.then(|| Metadata {
                        final_url: info.final_url,
                        size: info.size,
                        etag: info.etag,
                        content_type: info.content_type,
                        files: vec![],
                    }),
                    suggested_filename: info.filename,
                    reason: info.reason,
                })?;
            }
            HostCommand::Start {
                task,
                paths,
                budget,
                ..
            } => self.start(ctx, task, paths, budget)?,
            HostCommand::Pause { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.pause();
                    log(ctx, format!("paused {task_id}"))?;
                }
            }
            HostCommand::Resume { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.resume();
                    log(ctx, format!("resumed {task_id}"))?;
                }
            }
            HostCommand::Cancel { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.cancel();
                    log(ctx, format!("canceling {task_id}"))?;
                }
            }
            HostCommand::SetBudget { task_id, budget } => {
                if let Some(record) = self.tasks.get_mut(&task_id) {
                    record.budget = budget;
                    log(
                        ctx,
                        format!(
                            "budget for {task_id}: {} connections, {} B/s",
                            budget.connections, budget.download_bps
                        ),
                    )?;
                }
            }
            HostCommand::Snapshot {
                request_id,
                task_id,
            } => {
                let Some(record) = self.tasks.get(&task_id) else {
                    log(ctx, format!("snapshot for unknown task {task_id}"))?;
                    return Ok(());
                };
                let snapshot = record.progress.lock().unwrap().clone();
                ctx.send(&PluginEvent::Progress {
                    task_id: task_id.clone(),
                    seq: snapshot.seq,
                    downloaded: snapshot.downloaded,
                    total: snapshot.total,
                    speed_bps: snapshot.speed_bps,
                    eta_secs: if snapshot.speed_bps == 0 {
                        None
                    } else {
                        snapshot.total.map(|total| {
                            total.saturating_sub(snapshot.downloaded) / snapshot.speed_bps
                        })
                    },
                    connections: snapshot.connections,
                    chunks_done: Some(snapshot.chunks_done),
                    chunks_total: Some(snapshot.chunks_total),
                })?;
                log(ctx, format!("snapshot {request_id} for {task_id}"))?;
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

fn log(ctx: &mut PluginContext, message: String) -> Result<(), SdkError> {
    ctx.send(&PluginEvent::Log {
        level: LogLevel::Debug,
        message,
        fields: Default::default(),
    })
}
