//! Cloud-drive plugins with public share-link and private-file support.
//!
//! GDrive / OneDrive / Dropbox resolve public share links to direct HTTP
//! downloads and reuse the HTTP engine. Private files (`gdrive://`,
//! `onedrive://`, `dropbox://` or an authenticated Drive file link) use the
//! official APIs when OAuth credentials are supplied through the task
//! settings; without credentials they keep the existing share-link behaviour.
//! Baidu Pan / Aliyun Drive require separate official API applications and are
//! deliberately not implemented as shared-link scrapers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::{shared_progress, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Budget, Capability, ErrorKind, HostCommand, Metadata, Plugin, PluginContext, PluginError,
    PluginEvent, SdkError, TaskSpec,
};
use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use url::Url;

mod oauth;
#[cfg(test)]
mod testserver;

const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

/// Shown when a private resource is used without any OAuth credentials.
const PRIVATE_NEEDS_TOKEN: &str = "private file needs an access token";

/// Provider API roots. Tests pass their own values through [`Endpoints`].
pub const GDRIVE_API_BASE: &str = "https://www.googleapis.com";
pub const ONEDRIVE_GRAPH_BASE: &str = "https://graph.microsoft.com";
pub const DROPBOX_API_BASE: &str = "https://api.dropboxapi.com";

const GDRIVE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ONEDRIVE_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
const DROPBOX_TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";

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

    fn token_url(self) -> &'static str {
        match self {
            Self::GDrive => GDRIVE_TOKEN_URL,
            Self::OneDrive => ONEDRIVE_TOKEN_URL,
            Self::Dropbox => DROPBOX_TOKEN_URL,
        }
    }
}

/// Provider API roots, injectable so tests can point at a local mock server.
#[derive(Debug, Clone, Copy)]
struct Endpoints<'a> {
    gdrive_api: &'a str,
    onedrive_graph: &'a str,
    dropbox_api: &'a str,
}

const DEFAULT_ENDPOINTS: Endpoints<'static> = Endpoints {
    gdrive_api: GDRIVE_API_BASE,
    onedrive_graph: ONEDRIVE_GRAPH_BASE,
    dropbox_api: DROPBOX_API_BASE,
};

pub struct CloudPlugin {
    provider: Provider,
    tasks: HashMap<String, TaskRecord>,
    /// Last token obtained by a task. `Probe` carries no settings, so this is
    /// the only way a later probe can authenticate a private URL.
    token: Arc<Mutex<Option<CachedToken>>>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: Option<Instant>,
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| expires_at > Instant::now())
    }
}

impl CloudPlugin {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            tasks: HashMap::new(),
            token: Arc::new(Mutex::new(None)),
        }
    }

    fn cached_token(&self) -> Option<String> {
        let guard = self.token.lock().unwrap();
        guard
            .as_ref()
            .filter(|cached| cached.is_valid())
            .map(|cached| cached.token.clone())
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
                let (supported, url, filename, reason) = self.probe(ctx, &input)?;
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
                let target = match classify(self.provider, &task.url) {
                    Ok(target) => target,
                    Err(_) => {
                        ctx.send(&PluginEvent::Failed {
                            task_id: task.task_id.clone(),
                            error: PluginError::new(
                                ErrorKind::Protocol,
                                "unsupported cloud share link",
                            ),
                        })?;
                        return Ok(());
                    }
                };
                let client = aether_plugin_http::client::build_client(init(ctx)?)
                    .map_err(|error| SdkError::Protocol(error.to_string()))?;
                let record_id = task.task_id.clone();
                let thread_task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                let provider = self.provider;
                let cache = self.token.clone();
                std::thread::spawn(move || {
                    let prepared =
                        match prepare_task(&client, provider, target, task, DEFAULT_ENDPOINTS) {
                            Ok(prepared) => prepared,
                            Err(error) => {
                                let _ = sender.send(&PluginEvent::Failed {
                                    task_id: thread_task_id,
                                    error: PluginError::new(error.kind, error.message),
                                });
                                return;
                            }
                        };
                    if let Some(token) = prepared.token.as_ref() {
                        cache_token(&cache, token);
                    }
                    aether_plugin_http::download::run(
                        sender,
                        client,
                        prepared.task,
                        paths,
                        Budget::default(),
                        thread_control,
                        thread_progress,
                    );
                });
                self.tasks
                    .insert(record_id, TaskRecord { control, progress });
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

/// `(supported, final_url, filename, reason)`.
type ProbeOutcome = (bool, Option<String>, Option<String>, Option<String>);

impl CloudPlugin {
    fn probe(&self, ctx: &mut PluginContext, input: &str) -> Result<ProbeOutcome, SdkError> {
        let target = match classify(self.provider, input) {
            Ok(target) => target,
            Err(error) => return Ok((false, None, None, Some(error))),
        };

        if let Target::Public(url) = target {
            let client = aether_plugin_http::client::build_client(init(ctx)?)
                .map_err(|error| SdkError::Protocol(error.to_string()))?;
            return Ok(direct_probe(&client, &url, &HeaderMap::new()));
        }

        let Some(token) = self.cached_token() else {
            if let Target::GDriveFile(id) = &target {
                let client = aether_plugin_http::client::build_client(init(ctx)?)
                    .map_err(|error| SdkError::Protocol(error.to_string()))?;
                return Ok(direct_probe(&client, &gdrive_uc_url(id), &HeaderMap::new()));
            }
            return Ok((
                true,
                Some(input.to_string()),
                None,
                Some(PRIVATE_NEEDS_TOKEN.to_string()),
            ));
        };

        let client = aether_plugin_http::client::build_client(init(ctx)?)
            .map_err(|error| SdkError::Protocol(error.to_string()))?;
        match resolve_private(&client, &target, &token, DEFAULT_ENDPOINTS) {
            Ok(resolved) => {
                let headers = if resolved.bearer {
                    match bearer_headers(&token) {
                        Ok(headers) => headers,
                        Err(error) => {
                            return Ok((true, Some(input.to_string()), None, Some(error)))
                        }
                    }
                } else {
                    HeaderMap::new()
                };
                Ok(direct_probe(&client, &resolved.url, &headers))
            }
            // A stale or revoked token must not make the URL look unsupported.
            Err(error) => Ok((true, Some(input.to_string()), None, Some(error))),
        }
    }
}

fn init(ctx: &PluginContext) -> Result<&aether_plugin_sdk::Init, SdkError> {
    ctx.init()
        .ok_or_else(|| SdkError::Protocol("cloud command before Init".into()))
}

fn direct_probe(client: &Client, url: &str, headers: &HeaderMap) -> ProbeOutcome {
    let probe = aether_plugin_http::client::probe_with(client, url, headers);
    (
        probe.supported,
        Some(url.to_string()),
        probe.filename,
        probe.reason,
    )
}

/// What the user gave us before any network access.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// A public share link already rewritten to a direct HTTP URL.
    Public(String),
    /// Google Drive file id: API with a token, `uc?export=download` without.
    GDriveFile(String),
    /// OneDrive drive item id (Graph API only).
    OneDriveItem(String),
    /// Dropbox path (Dropbox API only).
    DropboxPath(String),
}

fn classify(provider: Provider, input: &str) -> Result<Target, String> {
    match provider {
        Provider::GDrive => {
            if let Some(raw) = protocol_arg(input, "gdrive://") {
                let id = raw.trim_matches('/');
                if id.is_empty() || id.contains('/') {
                    return Err("gdrive:// needs a single file id".into());
                }
                return Ok(Target::GDriveFile(id.to_string()));
            }
            let url = Url::parse(input).map_err(|error| error.to_string())?;
            if !url
                .host_str()
                .is_some_and(|host| host.contains("drive.google.com"))
            {
                return Err("not a Google Drive link".into());
            }
            gdrive_file_id(&url)
                .map(Target::GDriveFile)
                .ok_or_else(|| "Google Drive link has no file id".to_string())
        }
        Provider::OneDrive => {
            if let Some(raw) = protocol_arg(input, "onedrive://") {
                let id = raw.trim_matches('/');
                if id.is_empty() || id.contains('/') {
                    return Err("onedrive:// needs a single item id".into());
                }
                return Ok(Target::OneDriveItem(id.to_string()));
            }
            resolve_onedrive(input).map(Target::Public)
        }
        Provider::Dropbox => {
            if let Some(raw) = protocol_arg(input, "dropbox://") {
                let trimmed = raw.trim_matches('/');
                if trimmed.is_empty() {
                    return Err("dropbox:// needs a file path".into());
                }
                return Ok(Target::DropboxPath(format!("/{trimmed}")));
            }
            resolve_dropbox(input).map(Target::Public)
        }
    }
}

fn protocol_arg<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    if trimmed.len() < prefix.len() {
        return None;
    }
    if !trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    Some(&trimmed[prefix.len()..])
}

/// Public share-link resolution, unchanged from the shared-link-only plugin.
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
    let id = gdrive_file_id(&url).ok_or_else(|| "Google Drive link has no file id".to_string())?;
    Ok(gdrive_uc_url(&id))
}

fn gdrive_file_id(url: &Url) -> Option<String> {
    url.path_segments()
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
}

fn gdrive_uc_url(id: &str) -> String {
    format!(
        "https://drive.google.com/uc?export=download&id={}",
        encode_segment(id)
    )
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

/// Google Drive media endpoint (`alt=media`).
fn gdrive_media_url(base: &str, id: &str) -> String {
    format!(
        "{}/drive/v3/files/{}?alt=media",
        base.trim_end_matches('/'),
        encode_segment(id)
    )
}

/// OneDrive/Graph item content endpoint.
fn onedrive_content_url(base: &str, id: &str) -> String {
    format!(
        "{}/v1.0/me/drive/items/{}:/content",
        base.trim_end_matches('/'),
        encode_segment(id)
    )
}

/// Ask Dropbox for a short-lived direct link to `path`.
fn dropbox_temporary_link(
    client: &Client,
    base: &str,
    token: &str,
    path: &str,
) -> Result<String, String> {
    let url = format!("{}/2/files/get_temporary_link", base.trim_end_matches('/'));
    let body = serde_json::json!({ "path": path }).to_string();
    let response = client
        .post(&url)
        .timeout(oauth::DEFAULT_TIMEOUT)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(body)
        .send()
        .map_err(|error| format!("dropbox temporary link request failed: {error}"))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("dropbox temporary link response failed: {error}"))?;
    if !status.is_success() {
        let detail = oauth::sanitize_detail(&body, &[token]);
        return Err(format!(
            "dropbox temporary link returned HTTP {status}: {detail}"
        ));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|_| "dropbox temporary link returned invalid JSON".to_string())?;
    value
        .get("link")
        .and_then(Value::as_str)
        .filter(|link| link.starts_with("https://") || link.starts_with("http://"))
        .map(ToOwned::to_owned)
        .ok_or_else(|| "dropbox temporary link response has no download link".to_string())
}

fn encode_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char)
            }
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn bearer_headers(token: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "access token cannot be used as a header".to_string())?;
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

/// Resolve a private target into a direct URL plus whether it needs a bearer.
struct ResolvedUrl {
    url: String,
    bearer: bool,
}

fn resolve_private(
    client: &Client,
    target: &Target,
    token: &str,
    endpoints: Endpoints<'_>,
) -> Result<ResolvedUrl, String> {
    match target {
        Target::GDriveFile(id) => Ok(ResolvedUrl {
            url: gdrive_media_url(endpoints.gdrive_api, id),
            bearer: true,
        }),
        Target::OneDriveItem(id) => Ok(ResolvedUrl {
            url: onedrive_content_url(endpoints.onedrive_graph, id),
            bearer: true,
        }),
        Target::DropboxPath(path) => Ok(ResolvedUrl {
            url: dropbox_temporary_link(client, endpoints.dropbox_api, token, path)?,
            bearer: false,
        }),
        Target::Public(url) => Ok(ResolvedUrl {
            url: url.clone(),
            bearer: false,
        }),
    }
}

/// Attach `Authorization: Bearer` to the task settings consumed by the HTTP
/// engine. Any user-supplied Authorization header is replaced.
fn with_bearer(task: &mut TaskSpec, token: &str) {
    if !task.settings.is_object() {
        task.settings = Value::Object(Default::default());
    }
    let settings = task.settings.as_object_mut().expect("settings object");
    let headers = settings
        .entry("headers")
        .or_insert_with(|| Value::Object(Default::default()));
    if !headers.is_object() {
        *headers = Value::Object(Default::default());
    }
    let headers = headers.as_object_mut().expect("headers object");
    headers.retain(|name, _| !name.eq_ignore_ascii_case("authorization"));
    headers.insert(
        "Authorization".to_string(),
        Value::String(format!("Bearer {token}")),
    );
}

/// Remove OAuth material once it has been consumed. The download worker never
/// needs the refresh token or client secret, so keeping them would only widen
/// the exposure window.
fn strip_credentials(settings: &mut Value) {
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    for key in [
        "access_token",
        "oauth_client_id",
        "oauth_client_secret",
        "oauth_refresh_token",
        "oauth_token_url",
    ] {
        settings.remove(key);
    }
}

fn cache_token(cache: &Mutex<Option<CachedToken>>, token: &oauth::AccessToken) {
    let expires_at = token
        .expires_in
        .map(|seconds| Instant::now() + Duration::from_secs(seconds.saturating_sub(60).max(1)));
    *cache.lock().unwrap() = Some(CachedToken {
        token: token.token.clone(),
        expires_at,
    });
}

#[derive(Debug)]
struct PreparedTask {
    task: TaskSpec,
    token: Option<oauth::AccessToken>,
}

#[derive(Debug)]
struct PrepareError {
    kind: ErrorKind,
    message: String,
}

impl PrepareError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

fn prepare_task(
    client: &Client,
    provider: Provider,
    target: Target,
    mut task: TaskSpec,
    endpoints: Endpoints<'_>,
) -> Result<PreparedTask, PrepareError> {
    if let Target::Public(url) = &target {
        task.url = url.clone();
        strip_credentials(&mut task.settings);
        return Ok(PreparedTask { task, token: None });
    }

    let token = oauth::access_token(
        client,
        &task.settings,
        provider.token_url(),
        oauth::DEFAULT_TIMEOUT,
    )
    .map_err(|error| {
        let kind = oauth_kind(&error);
        PrepareError::new(kind, error.to_string())
    })?;

    let Some(token) = token else {
        if let Target::GDriveFile(id) = &target {
            task.url = gdrive_uc_url(id);
            return Ok(PreparedTask { task, token: None });
        }
        return Err(PrepareError::auth(PRIVATE_NEEDS_TOKEN));
    };

    let resolved = resolve_private(client, &target, &token.token, endpoints)
        .map_err(|error| PrepareError::new(ErrorKind::Protocol, error))?;
    task.url = resolved.url;
    if resolved.bearer {
        with_bearer(&mut task, &token.token);
    }
    strip_credentials(&mut task.settings);
    Ok(PreparedTask {
        task,
        token: Some(token),
    })
}

fn oauth_kind(error: &oauth::OAuthError) -> ErrorKind {
    match error {
        oauth::OAuthError::Timeout => ErrorKind::Timeout,
        oauth::OAuthError::InvalidSettings(_) | oauth::OAuthError::Provider { .. } => {
            ErrorKind::Auth
        }
        oauth::OAuthError::Request(_) => ErrorKind::Connect,
        oauth::OAuthError::Malformed(_) => ErrorKind::Protocol,
    }
}

impl PrepareError {
    fn auth(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Auth, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Response, Server};
    use serde_json::json;

    fn task(url: &str, settings: Value) -> TaskSpec {
        TaskSpec {
            task_id: "t1".into(),
            url: url.into(),
            filename: None,
            settings,
            plugin_state: None,
            expected_size: None,
            checksum: None,
            proxy: None,
        }
    }

    fn client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("test client")
    }

    fn authorization(task: &TaskSpec) -> Option<String> {
        aether_plugin_http::client::headers_from_settings(&task.settings)
            .expect("settings headers")
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

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

    #[test]
    fn classifies_private_schemes() {
        assert_eq!(
            classify(Provider::GDrive, "gdrive://ABC123").unwrap(),
            Target::GDriveFile("ABC123".into())
        );
        assert_eq!(
            classify(Provider::OneDrive, "onedrive://ITEM-1").unwrap(),
            Target::OneDriveItem("ITEM-1".into())
        );
        assert_eq!(
            classify(Provider::Dropbox, "dropbox:///folder/file.bin").unwrap(),
            Target::DropboxPath("/folder/file.bin".into())
        );
        assert_eq!(
            classify(
                Provider::GDrive,
                "https://drive.google.com/file/d/XYZ/view?usp=sharing"
            )
            .unwrap(),
            Target::GDriveFile("XYZ".into())
        );
        assert!(classify(Provider::Dropbox, "dropbox://").is_err());
        assert!(classify(Provider::GDrive, "gdrive://a/b").is_err());
    }

    #[test]
    fn builds_provider_api_urls() {
        assert_eq!(
            gdrive_media_url("https://api.example", "A B"),
            "https://api.example/drive/v3/files/A%20B?alt=media"
        );
        assert_eq!(
            onedrive_content_url("https://graph.example/", "ITEM"),
            "https://graph.example/v1.0/me/drive/items/ITEM:/content"
        );
    }

    #[test]
    fn gdrive_without_token_falls_back_to_public_link() {
        let prepared = prepare_task(
            &client(),
            Provider::GDrive,
            Target::GDriveFile("ABC123".into()),
            task("gdrive://ABC123", json!({})),
            DEFAULT_ENDPOINTS,
        )
        .unwrap();

        assert_eq!(
            prepared.task.url,
            "https://drive.google.com/uc?export=download&id=ABC123"
        );
        assert_eq!(authorization(&prepared.task), None);
    }

    #[test]
    fn gdrive_token_uses_api_and_strips_credentials() {
        let prepared = prepare_task(
            &client(),
            Provider::GDrive,
            Target::GDriveFile("ABC123".into()),
            task(
                "gdrive://ABC123",
                json!({
                    "access_token": "tok-123",
                    "oauth_client_secret": "secret-value",
                    "oauth_refresh_token": "refresh-value",
                }),
            ),
            DEFAULT_ENDPOINTS,
        )
        .unwrap();

        assert_eq!(
            prepared.task.url,
            "https://www.googleapis.com/drive/v3/files/ABC123?alt=media"
        );
        assert_eq!(
            authorization(&prepared.task).as_deref(),
            Some("Bearer tok-123")
        );
        for key in [
            "access_token",
            "oauth_client_secret",
            "oauth_refresh_token",
            "oauth_client_id",
            "oauth_token_url",
        ] {
            assert!(prepared.task.settings.get(key).is_none(), "{key} leaked");
        }
    }

    #[test]
    fn onedrive_without_token_keeps_private_url_supported_with_reason() {
        let error = prepare_task(
            &client(),
            Provider::OneDrive,
            Target::OneDriveItem("ITEM".into()),
            task("onedrive://ITEM", json!({})),
            DEFAULT_ENDPOINTS,
        )
        .unwrap_err();

        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.message, PRIVATE_NEEDS_TOKEN);
    }

    #[test]
    fn gdrive_private_request_targets_mock_with_bearer() {
        let server = Server::start(|_| Response::binary(Vec::new()));
        let prepared = prepare_task(
            &client(),
            Provider::GDrive,
            Target::GDriveFile("ABC123".into()),
            task("gdrive://ABC123", json!({ "access_token": "tok-gdrive" })),
            Endpoints {
                gdrive_api: &server.base(),
                onedrive_graph: &server.base(),
                dropbox_api: &server.base(),
            },
        )
        .unwrap();

        let headers = aether_plugin_http::client::headers_from_settings(&prepared.task.settings)
            .expect("settings headers");
        let probe = aether_plugin_http::client::probe_with(&client(), &prepared.task.url, &headers);
        assert!(probe.supported, "{probe:?}");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "HEAD");
        assert_eq!(requests[0].path, "/drive/v3/files/ABC123?alt=media");
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer tok-gdrive")
        );
    }

    #[test]
    fn onedrive_private_request_targets_graph_with_bearer() {
        let server = Server::start(|_| Response::binary(Vec::new()));
        let prepared = prepare_task(
            &client(),
            Provider::OneDrive,
            Target::OneDriveItem("ITEM!42".into()),
            task(
                "onedrive://ITEM!42",
                json!({ "access_token": "tok-onedrive" }),
            ),
            Endpoints {
                gdrive_api: &server.base(),
                onedrive_graph: &server.base(),
                dropbox_api: &server.base(),
            },
        )
        .unwrap();

        assert_eq!(
            prepared.task.url,
            format!("{}/v1.0/me/drive/items/ITEM%2142:/content", server.base())
        );

        let headers = aether_plugin_http::client::headers_from_settings(&prepared.task.settings)
            .expect("settings headers");
        let probe = aether_plugin_http::client::probe_with(&client(), &prepared.task.url, &headers);
        assert!(probe.supported, "{probe:?}");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1.0/me/drive/items/ITEM%2142:/content");
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer tok-onedrive")
        );
    }

    #[test]
    fn dropbox_private_requests_temporary_link() {
        let server =
            Server::start(|_| Response::json(r#"{"link":"https://dl.example/temp/file.bin"}"#));
        let prepared = prepare_task(
            &client(),
            Provider::Dropbox,
            Target::DropboxPath("/folder/file.bin".into()),
            task(
                "dropbox:///folder/file.bin",
                json!({ "access_token": "tok-dropbox" }),
            ),
            Endpoints {
                gdrive_api: &server.base(),
                onedrive_graph: &server.base(),
                dropbox_api: &server.base(),
            },
        )
        .unwrap();

        assert_eq!(prepared.task.url, "https://dl.example/temp/file.bin");
        assert_eq!(authorization(&prepared.task), None);

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/2/files/get_temporary_link");
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer tok-dropbox")
        );
        assert_eq!(
            requests[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body, json!({ "path": "/folder/file.bin" }));
    }

    #[test]
    fn public_targets_ignore_oauth_settings() {
        let target = classify(
            Provider::Dropbox,
            "https://www.dropbox.com/s/abc/file.bin?dl=0",
        )
        .unwrap();
        let prepared = prepare_task(
            &client(),
            Provider::Dropbox,
            target,
            task(
                "https://www.dropbox.com/s/abc/file.bin?dl=0",
                json!({ "access_token": "tok" }),
            ),
            DEFAULT_ENDPOINTS,
        )
        .unwrap();

        assert!(prepared
            .task
            .url
            .starts_with("https://dl.dropboxusercontent.com/"));
        assert!(prepared.token.is_none());
        assert_eq!(authorization(&prepared.task), None);
        assert!(prepared.task.settings.get("access_token").is_none());
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
