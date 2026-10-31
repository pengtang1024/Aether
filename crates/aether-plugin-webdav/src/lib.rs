//! WebDAV plugin for Aether. Reuses the HTTP download engine with a Basic
//! Authorization header and the `webdav(s)://` URL convention.

use std::sync::Arc;

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, HostCommand, Metadata, Plugin, PluginContext, PluginEvent, SdkError,
};
use url::Url;

mod propfind;

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
            HostCommand::Probe {
                request_id, input, ..
            } => {
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
                let outcome = probe_target(&client, &target);
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: outcome.supported,
                    protocol: Some("webdav".into()),
                    priority: 90,
                    metadata: outcome.metadata,
                    suggested_filename: outcome.suggested_filename,
                    reason: outcome.reason,
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

fn init_from_ctx(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("WebDAV command before Init".into()))
}

struct ProbeOutcome {
    supported: bool,
    metadata: Option<Metadata>,
    suggested_filename: Option<String>,
    reason: Option<String>,
}

/// Probe with DAV metadata first and fall back to the shared HTTP probe when
/// PROPFIND is rejected or unusable (405/501/403, non-2xx, transport errors,
/// malformed XML). Downloads and resume still use the HTTP engine.
fn probe_target(client: &reqwest::blocking::Client, target: &DavTarget) -> ProbeOutcome {
    match propfind::propfind(client, &target.url) {
        Ok(props) if props.is_collection => ProbeOutcome {
            supported: false,
            metadata: None,
            suggested_filename: None,
            reason: Some("WebDAV collection, not a file".into()),
        },
        Ok(props) => ProbeOutcome {
            supported: true,
            metadata: Some(Metadata {
                final_url: target.url.clone(),
                size: props.size,
                etag: props.etag,
                content_type: None,
                files: vec![],
            }),
            suggested_filename: target.filename(),
            reason: None,
        },
        Err(_) => {
            let info = aether_plugin_http::client::probe(client, &target.url);
            ProbeOutcome {
                supported: info.supported,
                metadata: info.supported.then(|| Metadata {
                    final_url: info.final_url,
                    size: info.size,
                    etag: info.etag,
                    content_type: info.content_type,
                    files: vec![],
                }),
                suggested_filename: info.filename,
                reason: info.reason,
            }
        }
    }
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    #[test]
    fn rewrites_webdav_scheme_and_auth() {
        let target = parse_target("webdavs://user:pass@example.com/dav/file.bin").unwrap();
        assert_eq!(target.url, "https://example.com/dav/file.bin");
        assert_eq!(target.auth(), Some(("user", "pass")));
        assert_eq!(target.username.as_deref(), Some("user"));
    }

    fn spawn_server(
        respond: impl Fn(&str) -> String + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    continue;
                }
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let request_line = request.lines().next().unwrap_or_default().to_string();
                recorded.lock().unwrap().push(request_line.clone());
                let response = respond(&request_line);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("webdav://{address}/dav/file.bin"), requests)
    }

    fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
    }

    #[test]
    fn falls_back_to_head_when_propfind_is_rejected() {
        let (url, requests) = spawn_server(|request_line| {
            if request_line.starts_with("PROPFIND ") {
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 42\r\nETag: \"head-etag\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
                    .to_string()
            }
        });
        let target = parse_target(&url).unwrap();

        let outcome = probe_target(&client(), &target);

        assert!(outcome.supported);
        let metadata = outcome.metadata.unwrap();
        assert_eq!(metadata.size, Some(42));
        assert_eq!(metadata.etag.as_deref(), Some("\"head-etag\""));
        assert_eq!(outcome.suggested_filename.as_deref(), Some("file.bin"));
        let requests = requests.lock().unwrap().clone();
        assert!(requests.iter().any(|line| line.starts_with("PROPFIND ")));
        assert!(requests.iter().any(|line| line.starts_with("HEAD ")));
    }

    #[test]
    fn rejects_collections_without_falling_back() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/file.bin/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let response = format!(
            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, requests) = spawn_server(move |_| response.clone());
        let target = parse_target(&url).unwrap();

        let outcome = probe_target(&client(), &target);

        assert!(!outcome.supported);
        assert!(outcome.metadata.is_none());
        assert_eq!(
            outcome.reason.as_deref(),
            Some("WebDAV collection, not a file")
        );
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("PROPFIND "));
    }
}
