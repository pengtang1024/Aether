//! Small HTTP client helpers for playlists and segments.

use std::time::Duration;

use aether_plugin_sdk::{Init, ProxyMode};
use reqwest::blocking::{Client, Response};

use crate::error::MediaError;

pub fn build_client(init: &Init) -> Result<Client, MediaError> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(format!(
            "Aether/{} (+https://github.com/pengtang1024/Aether)",
            init.host_version
        ));

    if let Some(proxy) = &init.proxy {
        match proxy.mode {
            ProxyMode::Direct => builder = builder.no_proxy(),
            ProxyMode::System => {}
            ProxyMode::Custom => {
                let url = proxy
                    .url
                    .as_deref()
                    .ok_or_else(|| MediaError::Protocol("custom proxy requires a URL".into()))?;
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

pub fn get_text(client: &Client, url: &url::Url) -> Result<String, MediaError> {
    let response = client.get(url.clone()).send()?.error_for_status()?;
    let text = response.text()?;
    if text.len() > 4 * 1024 * 1024 {
        return Err(MediaError::Protocol("playlist is too large".into()));
    }
    Ok(text)
}

pub fn get_bytes(client: &Client, url: &url::Url) -> Result<Vec<u8>, MediaError> {
    let response = client.get(url.clone()).send()?.error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

pub fn get_bytes_range(
    client: &Client,
    url: &url::Url,
    start: u64,
    end: u64,
) -> Result<Response, MediaError> {
    Ok(client
        .get(url.clone())
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
        .send()?
        .error_for_status()?)
}
