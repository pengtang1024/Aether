//! HTTP client construction, probing and filename handling.

use std::time::Duration;

use aether_plugin_sdk::fsutil::sanitize_filename;
use aether_plugin_sdk::{Init, ProxyMode};
use percent_encoding::percent_decode_str;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
};
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("http client: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("invalid proxy configuration: {0}")]
    Proxy(String),
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
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(format!(
            "Aether/{} (+https://github.com/pengtang1024/Aether)",
            init.host_version
        ));

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

/// Probe an HTTP(S) URL. Network errors are returned as `supported: false`
/// with a reason so the Host can try another plugin.
pub fn probe(client: &Client, input: &str) -> ProbeInfo {
    let url = match reqwest::Url::parse(input) {
        Ok(url) => url,
        Err(error) => return ProbeInfo::unsupported(input, format!("invalid URL: {error}")),
    };
    if !matches!(url.scheme(), "http" | "https") {
        return ProbeInfo::unsupported(input, "not an http(s) URL");
    }

    if let Ok(response) = client.head(url.clone()).send() {
        if response.status().is_success() {
            return inspect(response, url.as_str(), false);
        }
    }

    // Some servers reject HEAD. A one-byte GET still gives us the headers.
    match client
        .get(url.clone())
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
    {
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
