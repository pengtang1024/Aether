//! Cloud-drive shared-link plugins.
//!
//! GDrive / OneDrive / Dropbox resolve public share links to direct HTTP
//! downloads and reuse the HTTP engine. Baidu Pan / Aliyun Drive require
//! official API applications and are deliberately not implemented as
//! shared-link scrapers.

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use base64::Engine;
use url::Url;

const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

#[derive(Debug, Clone, Copy)]
pub enum Provider {
    GDrive,
    OneDrive,
    Dropbox,
}

impl Provider {
    fn id(self) -> &'static str {
        match self {
            Self::GDrive => "gdrive",
            Self::OneDrive => "onedrive",
            Self::Dropbox => "dropbox",
        }
    }
}

pub struct CloudPlugin {
    provider: Provider,
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl CloudPlugin {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            tasks: HashMap::new(),
        }
    }
}

impl Plugin for CloudPlugin {
    fn id(&self) -> &str {
        self.provider.id()
    }

    fn version(&self) -> &str {
        "0.1.0"
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
                let (supported, url, filename, reason) = match resolve(self.provider, &input) {
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
                    protocol: supported.then(|| self.provider.id().to_string()),
                    priority: 45,
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
                let Ok(url) = resolve(self.provider, &task.url) else {
                    ctx.send(&PluginEvent::Failed {
                        task_id: task.task_id.clone(),
                        error: aether_plugin_sdk::PluginError::new(
                            aether_plugin_sdk::ErrorKind::Protocol,
                            "unsupported cloud share link",
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
        .ok_or_else(|| SdkError::Protocol("cloud command before Init".into()))
}

pub fn resolve(provider: Provider, input: &str) -> Result<String, String> {
    match provider {
        Provider::GDrive => resolve_gdrive(input),
        Provider::OneDrive => resolve_onedrive(input),
        Provider::Dropbox => resolve_dropbox(input),
    }
}

fn resolve_gdrive(input: &str) -> Result<String, String> {
    let url = Url::parse(input).map_err(|error| error.to_string())?;
    if !url
        .host_str()
        .is_some_and(|host| host.contains("drive.google.com"))
    {
        return Err("not a Google Drive link".into());
    }
    let id = url
        .path_segments()
        .and_then(|segments| {
            let segments = segments.collect::<Vec<_>>();
            segments
                .iter()
                .position(|part| *part == "d")
                .and_then(|index| segments.get(index + 1))
                .map(|value| (*value).to_string())
        })
        .or_else(|| {
            url.query_pairs()
                .find(|(key, _)| key == "id")
                .map(|(_, value)| value.into_owned())
        })
        .ok_or_else(|| "Google Drive link has no file id".to_string())?;
    Ok(format!(
        "https://drive.google.com/uc?export=download&id={id}"
    ))
}

fn resolve_onedrive(input: &str) -> Result<String, String> {
    let url = Url::parse(input).map_err(|error| error.to_string())?;
    if !matches!(url.host_str(), Some("1drv.ms") | Some("onedrive.live.com")) {
        return Err("not a OneDrive share link".into());
    }
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_str());
    Ok(format!(
        "https://api.onedrive.com/v1.0/shares/u!{encoded}/root/content"
    ))
}

fn resolve_dropbox(input: &str) -> Result<String, String> {
    let mut url = Url::parse(input).map_err(|error| error.to_string())?;
    if !url
        .host_str()
        .is_some_and(|host| host.contains("dropbox.com"))
    {
        return Err("not a Dropbox share link".into());
    }
    url.set_host(Some("dl.dropboxusercontent.com"))
        .map_err(|_| "cannot rewrite Dropbox host".to_string())?;
    let mut pairs = url.query_pairs_mut();
    pairs.clear();
    pairs.append_pair("dl", "1");
    drop(pairs);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_gdrive_file_links() {
        assert_eq!(
            resolve(
                Provider::GDrive,
                "https://drive.google.com/file/d/ABC123/view?usp=sharing"
            )
            .unwrap(),
            "https://drive.google.com/uc?export=download&id=ABC123"
        );
    }

    #[test]
    fn resolves_onedrive_share_links() {
        let resolved = resolve(Provider::OneDrive, "https://1drv.ms/u/s!abc").unwrap();
        assert!(resolved.starts_with("https://api.onedrive.com/v1.0/shares/u!"));
        assert!(resolved.ends_with("/root/content"));
    }

    #[test]
    fn resolves_dropbox_links() {
        let resolved = resolve(
            Provider::Dropbox,
            "https://www.dropbox.com/s/abc/file.bin?dl=0",
        )
        .unwrap();
        assert!(resolved.starts_with("https://dl.dropboxusercontent.com/s/abc/file.bin?"));
        assert!(resolved.contains("dl=1"));
    }
}

#[cfg(test)]
mod manifest_tests {
    use aether_plugin_sdk::PluginManifest;

    #[test]
    fn provider_manifests_are_valid() {
        for provider in ["gdrive", "onedrive", "dropbox"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins")
                .join(provider)
                .join("plugin.json");
            let manifest = PluginManifest::from_path(&path).unwrap();
            assert_eq!(manifest.id, provider);
            manifest.validate().unwrap();
        }
    }
}
