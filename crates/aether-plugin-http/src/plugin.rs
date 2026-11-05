//! The HTTP plugin state machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use aether_plugin_sdk::{
    Budget, Capability, HostCommand, Init, LogLevel, Metadata, Plugin, PluginContext, PluginEvent,
    SdkError, TaskPaths, TaskSpec,
};

use crate::client::{self, HttpVersion};
use crate::gate::HostGate;
use crate::metalink;
use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};

use crate::download;

const PLUGIN_ID: &str = "http";
const PLUGIN_VERSION: &str = "0.1.0";

const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct HttpPlugin {
    client: Option<reqwest::blocking::Client>,
    init: Option<Init>,
    tasks: HashMap<String, TaskRecord>,
    /// Shared by every task in this process so `per_host_connections` is a
    /// host-wide cap rather than a per-task one.
    gate: Arc<HostGate>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
    /// Shared with the download threads so `SetBudget` (aria2
    /// `changeOption`) changes the live throttle without a restart.
    budget: Arc<Mutex<Budget>>,
}

impl HttpPlugin {
    pub fn new() -> Self {
        Self {
            client: None,
            init: None,
            tasks: HashMap::new(),
            gate: Arc::new(HostGate::default()),
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, SdkError> {
        self.client
            .clone()
            .ok_or_else(|| SdkError::Protocol("plugin received a command before Init".into()))
    }

    /// Client for one task: a proxy resolved by the Host (PAC) wins over the
    /// process-level client, so it gets its own connection pool. A task asking
    /// for more than one connection gets an HTTP/1.1 client so each shard owns
    /// a TCP connection instead of sharing one h2 stream multiplexer.
    fn client_with(
        &self,
        proxy: &Option<aether_plugin_sdk::ProxyConfig>,
        version: HttpVersion,
    ) -> Result<reqwest::blocking::Client, SdkError> {
        if version == HttpVersion::Auto
            && (proxy.is_none()
                || self.init.as_ref().and_then(|init| init.proxy.as_ref()) == proxy.as_ref())
        {
            return self.client();
        }
        let mut init = self
            .init
            .clone()
            .ok_or_else(|| SdkError::Protocol("plugin received a command before Init".into()))?;
        if proxy.is_some() {
            init.proxy = proxy.clone();
        }
        client::build_client_with_protocol(&init, None, None, version)
            .map_err(|error| SdkError::Protocol(format!("per-task http client: {error}")))
    }

    fn start(
        &mut self,
        ctx: &mut PluginContext,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    ) -> Result<(), SdkError> {
        // The protocol is picked once, when the task starts: segmented tasks
        // need one TCP per shard, single-connection tasks keep the shared
        // process client. A budget change cannot re-split ranges anyway.
        let (client, fallback) = if budget.connections > 1 {
            (
                self.client_with(&task.proxy, HttpVersion::Http1Only)?,
                Some(self.client_with(&task.proxy, HttpVersion::Auto)?),
            )
        } else {
            (self.client_with(&task.proxy, HttpVersion::Auto)?, None)
        };
        let task_id = task.task_id.clone();
        let sender = ctx.sender();
        let control = Arc::new(TaskControl::new());
        let progress: SharedProgress = shared_progress();
        let gate = self.gate.clone();

        let thread_control = control.clone();
        let thread_progress = progress.clone();
        let budget = Arc::new(Mutex::new(budget));
        let thread_budget = budget.clone();
        thread::spawn(move || {
            download::run_with_budget(
                sender,
                client,
                fallback,
                gate,
                task,
                paths,
                thread_budget,
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
        self.init = Some(init.clone());
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
            HostCommand::Probe {
                request_id,
                input,
                proxy,
            } => {
                let client = self.client_with(&proxy, HttpVersion::Auto)?;
                if metalink::is_metalink(&input) {
                    let (supported, filename, size, reason) =
                        match metalink::resolve(&client, &input) {
                            Ok(Some(info)) => (true, Some(info.filename), info.size, None),
                            Ok(None) => (false, None, None, Some("not a metalink".into())),
                            Err(error) => (true, None, None, Some(error.to_string())),
                        };
                    ctx.send(&PluginEvent::ProbeResult {
                        request_id,
                        supported,
                        protocol: supported.then(|| "metalink".into()),
                        priority: 110,
                        metadata: supported.then(|| Metadata {
                            final_url: input.clone(),
                            size,
                            etag: None,
                            content_type: Some("application/metalink4+xml".into()),
                            files: vec![],
                        }),
                        suggested_filename: filename,
                        reason,
                    })?;
                } else {
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
                    *record.budget.lock().unwrap() = budget;
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

fn log(ctx: &mut PluginContext, message: String) -> Result<(), SdkError> {
    ctx.send(&PluginEvent::Log {
        level: LogLevel::Debug,
        message,
        fields: Default::default(),
    })
}
