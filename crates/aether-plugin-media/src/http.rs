//! Small HTTP client helpers for playlists and segments.

use aether_plugin_sdk::Init;
use reqwest::blocking::{Client, Response};

use crate::error::MediaError;

/// The media plugin shares the HTTP plugin's client (user agent, IP mode and
/// DNS settings included); the wrapper only re-frames its error type.
pub fn build_client(init: &Init) -> Result<Client, MediaError> {
    aether_plugin_http::client::build_client(init)
        .map_err(|error| MediaError::Protocol(error.to_string()))
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
