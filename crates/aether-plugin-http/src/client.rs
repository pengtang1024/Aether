//! HTTP client construction, probing and filename handling.

use std::sync::Arc;
use std::time::Duration;

use crate::dns;
use aether_plugin_sdk::fsutil::sanitize_filename;
use aether_plugin_sdk::{Init, ProxyMode};
use percent_encoding::percent_decode_str;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT_RANGES, CONNECTION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
    COOKIE, ETAG, HOST, RANGE, REFERER, TE, TRANSFER_ENCODING, UPGRADE, USER_AGENT,
};
use reqwest::StatusCode;
use thiserror::Error;

/// Shorthand for the reqwest header map type.
pub type HeaderMap = reqwest::header::HeaderMap;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("http client: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("invalid proxy configuration: {0}")]
    Proxy(String),

    #[error("invalid DNS configuration: {0}")]
    Resolver(String),
}

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub supported: bool,
    pub protocol: Option<String>,
    pub final_url: String,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub filename: Option<String>,
    pub accept_ranges: bool,
    pub reason: Option<String>,
}

impl ProbeInfo {
    fn unsupported(url: &str, reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            protocol: None,
            final_url: url.to_string(),
            size: None,
            etag: None,
            content_type: None,
            filename: None,
            accept_ranges: false,
            reason: Some(reason.into()),
        }
    }
}

pub fn build_client(init: &Init) -> Result<Client, ClientError> {
    build_client_with_auth(init, None)
}

/// Build a client with an optional HTTP Basic Authorization header.
pub fn build_client_with_auth(
    init: &Init,
    auth: Option<(&str, &str)>,
) -> Result<Client, ClientError> {
    build_client_custom(init, None, auth)
}

/// Build a client with a custom User-Agent and optional Basic auth.
///
/// Precedence for the user agent: the explicit per-call one (task/plugin
/// specific) wins, then the daemon-wide user agent from `Init::network`, then
/// the built-in `Aether/<version>` default.
pub fn build_client_custom(
    init: &Init,
    user_agent: Option<&str>,
    auth: Option<(&str, &str)>,
) -> Result<Client, ClientError> {
    let network = init.network.as_ref();
    let user_agent = user_agent
        .map(ToOwned::to_owned)
        .or_else(|| network.and_then(|config| config.user_agent.clone()))
        .unwrap_or_else(|| {
            format!(
                "Aether/{} (+https://github.com/pengtang1024/Aether)",
                init.host_version
            )
        });

    let mut headers = reqwest::header::HeaderMap::new();
    if let Some((username, password)) = auth {
        let encoded = base64_encode(format!("{username}:{password}").as_bytes());
        let value = reqwest::header::HeaderValue::from_str(&format!("Basic {encoded}"))
            .map_err(|error| ClientError::Proxy(format!("invalid Basic auth header: {error}")))?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    // Daemon-wide default headers; a per-task header of the same name
    // overrides these because `apply_headers` inserts per request.
    if let Some(defaults) = network.and_then(|config| config.default_headers.as_ref()) {
        for (name, value) in defaults {
            let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                eprintln!("aether-plugin-http: skipping invalid header name {name:?}");
                continue;
            };
            let Ok(value) = reqwest::header::HeaderValue::from_str(value) else {
                eprintln!("aether-plugin-http: skipping invalid value for header {name:?}");
                continue;
            };
            headers.insert(name, value);
        }
    }

    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .user_agent(user_agent);

    if let Some(config) = network {
        // Binding a wildcard address restricts connections to one family; the
        // resolver applies the same filter for resolved hostnames.
        if let Some(local_address) = config.local_address() {
            builder = builder.local_address(local_address);
        }
        if config
            .dns
            .as_ref()
            .is_some_and(|servers| !servers.is_empty())
        {
            let resolver = dns::custom_resolver(config).map_err(ClientError::Resolver)?;
            builder = builder.dns_resolver(Arc::new(resolver));
        }
    }

    if let Some(proxy) = &init.proxy {
        match proxy.mode {
            ProxyMode::Direct => {
                builder = builder.no_proxy();
            }
            ProxyMode::System => {}
            ProxyMode::Custom => {
                let url = proxy
                    .url
                    .as_deref()
                    .ok_or_else(|| ClientError::Proxy("custom proxy requires a URL".into()))?;
                let mut configured = reqwest::Proxy::all(url)?;
                if let (Some(username), Some(password)) = (&proxy.username, &proxy.password) {
                    configured = configured.basic_auth(username, password);
                }
                builder = builder.proxy(configured);
            }
        }
    }

    Ok(builder.build()?)
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (b0 << 16) | (b1 << 8) | b2;
        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

/// Probe an HTTP(S) URL. Network errors are returned as `supported: false`
/// with a reason so the Host can try another plugin.
/// Header names that must not be overridden from task settings: the download
/// engine owns them (or they break the HTTP connection semantics).
const FORBIDDEN_SETTING_HEADERS: &[reqwest::header::HeaderName] = &[
    RANGE,
    HOST,
    CONTENT_LENGTH,
    CONNECTION,
    TRANSFER_ENCODING,
    TE,
    UPGRADE,
];

/// Build extra per-request headers from task settings.
///
/// Recognized keys: `referrer`, `user_agent`, `cookies` (string or object) and
/// `headers` (object). Control headers inside `headers` are rejected so they
/// cannot break segmented downloads; `COOKIE`/`REFERER`/`USER_AGENT` given
/// through their dedicated keys take precedence over the `headers` object.
pub fn headers_from_settings(settings: &serde_json::Value) -> Result<HeaderMap, ClientError> {
    let mut headers = HeaderMap::new();

    if let Some(map) = settings
        .get("headers")
        .and_then(serde_json::Value::as_object)
    {
        for (name, value) in map {
            let Some(value) = value.as_str() else {
                return Err(ClientError::Proxy(format!(
                    "header {name} must be a string"
                )));
            };
            let name =
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                    ClientError::Proxy(format!("invalid header name {name}: {error}"))
                })?;
            if FORBIDDEN_SETTING_HEADERS.contains(&name) {
                return Err(ClientError::Proxy(format!(
                    "header {name} is managed by the download engine"
                )));
            }
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
                ClientError::Proxy(format!("invalid value for header {name}: {error}"))
            })?;
            headers.insert(name, value);
        }
    }

    if let Some(referrer) = settings.get("referrer").and_then(serde_json::Value::as_str) {
        let value = reqwest::header::HeaderValue::from_str(referrer)
            .map_err(|error| ClientError::Proxy(format!("invalid referrer: {error}")))?;
        headers.insert(REFERER, value);
    }

    if let Some(agent) = settings
        .get("user_agent")
        .and_then(serde_json::Value::as_str)
    {
        let value = reqwest::header::HeaderValue::from_str(agent)
            .map_err(|error| ClientError::Proxy(format!("invalid user_agent: {error}")))?;
        headers.insert(USER_AGENT, value);
    }

    match settings.get("cookies") {
        Some(serde_json::Value::String(raw)) => {
            let value = reqwest::header::HeaderValue::from_str(raw)
                .map_err(|error| ClientError::Proxy(format!("invalid cookies: {error}")))?;
            headers.insert(COOKIE, value);
        }
        Some(serde_json::Value::Object(map)) => {
            let pairs = map
                .iter()
                .map(|(name, value)| {
                    let value = value.as_str().ok_or_else(|| {
                        ClientError::Proxy(format!("cookie {name} must be a string"))
                    })?;
                    Ok(format!("{name}={value}"))
                })
                .collect::<Result<Vec<_>, ClientError>>()?;
            let raw = pairs.join("; ");
            let value = reqwest::header::HeaderValue::from_str(&raw)
                .map_err(|error| ClientError::Proxy(format!("invalid cookies: {error}")))?;
            headers.insert(COOKIE, value);
        }
        _ => {}
    }

    Ok(headers)
}

/// Apply the extra headers to a request builder.
pub fn apply_headers(
    request: reqwest::blocking::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::blocking::RequestBuilder {
    request.headers(headers.clone())
}

pub fn probe(client: &Client, input: &str) -> ProbeInfo {
    probe_with(client, input, &HeaderMap::new())
}

/// Probe a URL with extra per-request headers (referrer-based hotlink
/// protection needs them to see the real response).
pub fn probe_with(client: &Client, input: &str, extra: &HeaderMap) -> ProbeInfo {
    let url = match reqwest::Url::parse(input) {
        Ok(url) => url,
        Err(error) => return ProbeInfo::unsupported(input, format!("invalid URL: {error}")),
    };
    if !matches!(url.scheme(), "http" | "https") {
        return ProbeInfo::unsupported(input, "not an http(s) URL");
    }

    if let Ok(response) = apply_headers(client.head(url.clone()), extra).send() {
        if response.status().is_success() {
            return inspect(response, url.as_str(), false);
        }
    }

    // Some servers reject HEAD. A one-byte GET still gives us the headers.
    let request = apply_headers(
        client
            .get(url.clone())
            .header(reqwest::header::RANGE, "bytes=0-0"),
        extra,
    );
    match request.send() {
        Ok(response) if response.status().is_success() => inspect(response, url.as_str(), true),
        Ok(response) => ProbeInfo {
            supported: true,
            protocol: Some(url.scheme().to_string()),
            final_url: response.url().to_string(),
            size: content_length(&response),
            etag: header(&response, ETAG),
            content_type: header(&response, CONTENT_TYPE),
            filename: filename_from_response(&response, &url),
            accept_ranges: header(&response, ACCEPT_RANGES)
                .is_some_and(|value| value.eq_ignore_ascii_case("bytes")),
            reason: Some(format!("HTTP {}", response.status())),
        },
        Err(error) => ProbeInfo {
            // The URL looked valid; keep it addable so the download attempt
            // surfaces a retryable error instead of "no plugin supports this".
            supported: true,
            protocol: Some(url.scheme().to_string()),
            final_url: url.to_string(),
            size: None,
            etag: None,
            content_type: None,
            filename: filename_from_url(&url),
            accept_ranges: false,
            reason: Some(format!("probe failed: {error}")),
        },
    }
}

fn inspect(response: Response, requested_url: &str, ranged_request: bool) -> ProbeInfo {
    let url = reqwest::Url::parse(requested_url).ok();
    let final_url = response.url().clone();
    let status = response.status();
    let total = content_range_total(&response).or_else(|| content_length(&response));
    let accept_ranges = (ranged_request && status == StatusCode::PARTIAL_CONTENT)
        || header(&response, ACCEPT_RANGES)
            .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));

    ProbeInfo {
        supported: true,
        protocol: url.as_ref().map(|url| url.scheme().to_string()),
        final_url: final_url.to_string(),
        size: total,
        etag: header(&response, ETAG),
        content_type: header(&response, CONTENT_TYPE),
        filename: filename_from_response(&response, &final_url),
        accept_ranges,
        reason: (!status.is_success()).then(|| format!("HTTP {status}")),
    }
}

fn header(response: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn content_length(response: &Response) -> Option<u64> {
    header(response, CONTENT_LENGTH)?.parse().ok()
}

fn content_range_total(response: &Response) -> Option<u64> {
    let value = header(response, CONTENT_RANGE)?;
    value.rsplit_once('/')?.1.parse().ok()
}

fn filename_from_response(response: &Response, url: &reqwest::Url) -> Option<String> {
    header(response, CONTENT_DISPOSITION)
        .and_then(|value| filename_from_disposition(&value))
        .or_else(|| filename_from_url(url))
}

pub fn filename_from_url(url: &reqwest::Url) -> Option<String> {
    let last = url.path_segments()?.next_back()?;
    let decoded = percent_decode_str(last).decode_utf8_lossy();
    sanitize_filename(&decoded)
}

pub fn filename_from_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        let raw = if let Some(rest) = part.strip_prefix("filename*=") {
            // RFC 5987: UTF-8''name
            rest.rsplit("''").next().unwrap_or(rest)
        } else if let Some(rest) = part.strip_prefix("filename=") {
            rest
        } else {
            continue;
        };
        let raw = raw.trim().trim_matches('"').trim_matches('\'');
        let decoded = percent_decode_str(raw).decode_utf8_lossy();
        if let Some(name) = sanitize_filename(&decoded) {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_disposition() {
        assert_eq!(
            filename_from_disposition("attachment; filename=\"report.pdf\"").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            filename_from_disposition("attachment; filename*=UTF-8''na%C3%AFve.txt").as_deref(),
            Some("naïve.txt")
        );
    }

    #[test]
    fn extracts_filename_from_url() {
        let url = reqwest::Url::parse("https://example.com/files/hello%20world.bin?x=1").unwrap();
        assert_eq!(filename_from_url(&url).as_deref(), Some("hello world.bin"));
    }
}
