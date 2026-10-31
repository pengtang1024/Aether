//! The BitTorrent plugin state machine.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Init, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
    TaskPaths, TaskSpec,
};
use librqbit::limits::LimitsConfig;
use librqbit::{Session, SessionOptions};
use url::Url;

use crate::bt;

const PLUGIN_ID: &str = "bt";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct BtPlugin {
    runtime: Option<Arc<tokio::runtime::Runtime>>,
    session: Option<Arc<Session>>,
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl BtPlugin {
    pub fn new() -> Self {
        Self {
            runtime: None,
            session: None,
            tasks: HashMap::new(),
        }
    }

    fn start(&mut self, ctx: &mut PluginContext, task: TaskSpec, paths: TaskPaths) {
        let (Some(runtime), Some(session)) = (self.runtime.clone(), self.session.clone()) else {
            let _ = ctx.send(&PluginEvent::Failed {
                task_id: task.task_id.clone(),
                error: aether_plugin_sdk::PluginError::new(
                    aether_plugin_sdk::ErrorKind::Internal,
                    "BitTorrent session is not initialized",
                ),
            });
            return;
        };

        let task_id = task.task_id.clone();
        let sender = ctx.sender();
        let control = Arc::new(TaskControl::new());
        let progress = shared_progress();
        let thread_control = control.clone();
        let thread_progress = progress.clone();

        thread::spawn(move || {
            bt::run(
                sender,
                runtime,
                session,
                task,
                paths,
                thread_control,
                thread_progress,
            );
        });

        self.tasks.insert(task_id, TaskRecord { control, progress });
    }
}

impl Default for BtPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for BtPlugin {
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
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| SdkError::Protocol(format!("tokio runtime: {error}")))?;

        let network = init.network.as_ref();
        // The daemon's settings win; the boot-time environment fills what the
        // settings leave out.
        let disable_dht = match network.and_then(|config| config.bt_dht) {
            Some(enabled) => !enabled,
            None => std::env::var_os("AETHER_BT_DISABLE_DHT").is_some(),
        };
        let listen_port = network
            .and_then(|config| config.bt_listen_port)
            .or_else(|| {
                std::env::var("AETHER_BT_LISTEN_PORT")
                    .ok()
                    .and_then(|value| value.parse::<u16>().ok())
            });
        let upload_bps = network
            .and_then(|config| config.bt_upload_bps)
            .and_then(|bps| u32::try_from(bps).ok())
            .and_then(NonZeroU32::new);
        let trackers = network
            .and_then(|config| config.bt_trackers.as_ref())
            .map(|list| {
                list.iter()
                    .filter_map(|entry| match Url::parse(entry) {
                        Ok(url) => Some(url),
                        Err(error) => {
                            eprintln!("aether-plugin-bt: skipping tracker {entry:?}: {error}");
                            None
                        }
                    })
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let blocklist_url = network
            .and_then(|config| config.bt_blocklist.as_deref())
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                // A bare path becomes a file:// URL the engine loads directly.
                if entry.starts_with("http://")
                    || entry.starts_with("https://")
                    || entry.starts_with("file://")
                {
                    entry.to_string()
                } else {
                    Url::from_file_path(entry)
                        .map(|url| url.to_string())
                        .unwrap_or_else(|()| entry.to_string())
                }
            });
        let options = SessionOptions {
            disable_dht,
            disable_dht_persistence: true,
            listen_port_range: listen_port.map(|port| port..port + 1),
            ratelimits: LimitsConfig {
                upload_bps,
                download_bps: None,
            },
            blocklist_url,
            trackers,
            ..Default::default()
        };
        let session = runtime
            .block_on(Session::new_with_opts(
                PathBuf::from(&init.data_dir),
                options,
            ))
            .map_err(|error| SdkError::Protocol(format!("BitTorrent session: {error}")))?;

        self.runtime = Some(Arc::new(runtime));
        self.session = Some(session);
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
                let probe = bt::probe(&input);
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: probe.supported,
                    protocol: probe.supported.then(|| "bt".to_string()),
                    priority: 60,
                    metadata: probe.supported.then(|| Metadata {
                        final_url: input.clone(),
                        size: None,
                        etag: None,
                        content_type: Some("application/x-bittorrent".into()),
                        files: vec![],
                    }),
                    suggested_filename: probe.filename,
                    reason: probe.reason,
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
                    connections: 0,
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
