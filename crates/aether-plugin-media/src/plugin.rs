//! The media plugin state machine (HLS now, DASH next).

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError, TaskPaths,
    TaskSpec,
};
use url::Url;

use crate::dash;
use crate::error::MediaError;
use crate::hls;
use crate::http;

const PLUGIN_ID: &str = "media";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct MediaPlugin {
    client: Option<reqwest::blocking::Client>,
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl MediaPlugin {
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

    fn start(&mut self, ctx: &mut PluginContext, task: TaskSpec, paths: TaskPaths) {
        let task_id = task.task_id.clone();
        let sender = ctx.sender();
        let control = Arc::new(TaskControl::new());
        let progress = shared_progress();
        let thread_control = control.clone();
        let thread_progress = progress.clone();
        let client = self.client.clone();

        thread::spawn(move || {
            let Some(client) = client else {
                let _ = sender.send(&PluginEvent::Failed {
                    task_id: task.task_id.clone(),
                    error: MediaError::Protocol("media client is not initialized".into())
                        .to_plugin_error(),
                });
                return;
            };
            let url = Url::parse(&task.url);
            match url {
                Ok(url) if hls::is_hls_url(&url) => {
                    hls::download(sender, client, task, paths, thread_control, thread_progress);
                }
                Ok(url) if dash::is_dash_url(&url) => {
                    dash::download(sender, client, task, paths, thread_control, thread_progress);
                }
                _ => {
                    let _ = sender.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: MediaError::Protocol(
                            "unsupported media URL (expected .m3u8 or .mpd)".into(),
                        )
                        .to_plugin_error(),
                    });
                }
            }
        });

        self.tasks.insert(task_id, TaskRecord { control, progress });
    }
}

impl Default for MediaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for MediaPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    fn on_init(
        &mut self,
        _ctx: &mut PluginContext,
        init: &aether_plugin_sdk::Init,
    ) -> Result<(), SdkError> {
        self.client = Some(
            http::build_client(init)
                .map_err(|error| SdkError::Protocol(format!("media client init: {error}")))?,
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
                let (supported, protocol, filename, reason, content_type) =
                    if let Ok(url) = Url::parse(&input) {
                        if hls::is_hls_url(&url) {
                            let probe = hls::probe(&client, &input);
                            (
                                probe.supported,
                                Some("hls".to_string()),
                                probe.filename,
                                probe.reason,
                                "application/vnd.apple.mpegurl",
                            )
                        } else if dash::is_dash_url(&url) {
                            let probe = dash::probe(&client, &input);
                            (
                                probe.supported,
                                Some("dash".to_string()),
                                probe.filename,
                                probe.reason,
                                "application/dash+xml",
                            )
                        } else {
                            (false, None, None, None, "application/octet-stream")
                        }
                    } else {
                        (
                            false,
                            None,
                            None,
                            Some("invalid URL".into()),
                            "application/octet-stream",
                        )
                    };

                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol,
                    priority: 120,
                    metadata: supported.then(|| Metadata {
                        final_url: input.clone(),
                        size: None,
                        etag: None,
                        content_type: Some(content_type.into()),
                        files: vec![],
                    }),
                    suggested_filename: filename,
                    reason,
                })?;
            }
            HostCommand::Start { task, paths, .. } => self.start(ctx, task, paths),
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
