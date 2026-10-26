//! WebDAV plugin for Aether. Reuses the HTTP download engine with a Basic
//! Authorization header and the `webdav(s)://` URL convention.

use std::sync::Arc;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use url::Url;

const PLUGIN_ID: &str = "webdav";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

pub struct WebdavPlugin {
    tasks: std::collections::HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl WebdavPlugin {
    pub fn new() -> Self {
        Self {
            tasks: std::collections::HashMap::new(),
        }
    }
}

impl Default for WebdavPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for WebdavPlugin {
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
                let target = match parse_target(&input) {
                    Ok(target) => target,
                    Err(error) => {
                        ctx.send(&PluginEvent::ProbeResult {
                            request_id,
                            supported: false,
                            protocol: None,
                            priority: 90,
                            metadata: None,
                            suggested_filename: None,
                            reason: Some(error),
                        })?;
                        return Ok(());
                    }
                };
                let client = aether_plugin_http::client::build_client_with_auth(
                    init_from_ctx(ctx)?,
                    target.auth(),
                )
                .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let info = aether_plugin_http::client::probe(&client, &target.url);
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: info.supported,
                    protocol: Some("webdav".into()),
                    priority: 90,
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
            HostCommand::Start { task, paths, .. } => {
                let target = match parse_target(&task.url) {
                    Ok(target) => target,
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
                let init = init_from_ctx(ctx)?.clone();
                let client =
                    aether_plugin_http::client::build_client_with_auth(&init, target.auth())
                        .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let mut task = task;
                task.url = target.url.clone();
                if task.filename.is_none() {
                    task.filename = target.filename();
                }
                let task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                let paths = paths.clone();
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

fn init_from_ctx(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("WebDAV command before Init".into()))
}

struct DavTarget {
    url: String,
    username: Option<String>,
    password: Option<String>,
}

impl DavTarget {
    fn auth(&self) -> Option<(&str, &str)> {
        Some((self.username.as_deref()?, self.password.as_deref()?))
    }

    fn filename(&self) -> Option<String> {
        Url::parse(&self.url)
            .ok()
            .and_then(|url| url.path_segments()?.next_back().map(ToOwned::to_owned))
            .and_then(|name| aether_plugin_sdk::fsutil::sanitize_filename(&name))
    }
}

fn parse_target(input: &str) -> Result<DavTarget, String> {
    let mut url = Url::parse(input).map_err(|error| error.to_string())?;
    let scheme = match url.scheme() {
        "webdav" | "dav" => "http",
        "webdavs" | "davs" => "https",
        other => return Err(format!("unsupported WebDAV scheme {other}")),
    };
    let username = (!url.username().is_empty()).then(|| url.username().to_string());
    let password = url.password().map(ToOwned::to_owned);
    url.set_username("")
        .map_err(|_| "invalid username".to_string())?;
    url.set_password(None)
        .map_err(|_| "invalid password".to_string())?;
    let rest = url
        .as_str()
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| "invalid WebDAV URL".to_string())?;
    Ok(DavTarget {
        url: format!("{scheme}://{rest}"),
        username,
        password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_webdav_scheme_and_auth() {
        let target = parse_target("webdavs://user:pass@example.com/dav/file.bin").unwrap();
        assert_eq!(target.url, "https://example.com/dav/file.bin");
        assert_eq!(target.auth(), Some(("user", "pass")));
        assert_eq!(target.username.as_deref(), Some("user"));
    }
}
