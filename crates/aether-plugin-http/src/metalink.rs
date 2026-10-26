//! Metalink v3/v4 parsing and mirror selection.

use aether_plugin_sdk::{Checksum, ChecksumAlgorithm};
use reqwest::blocking::Client;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone)]
pub struct MetalinkInfo {
    pub url: String,
    pub filename: String,
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
}

pub fn is_metalink(input: &str) -> bool {
    Url::parse(input)
        .ok()
        .map(|url| {
            let path = url.path().to_ascii_lowercase();
            path.ends_with(".meta4") || path.ends_with(".metalink")
        })
        .unwrap_or(false)
}

pub fn resolve(client: &Client, input: &str) -> Result<Option<MetalinkInfo>, MetalinkError> {
    if !is_metalink(input) {
        return Ok(None);
    }
    let base = Url::parse(input)?;
    let text = client
        .get(base.clone())
        .send()?
        .error_for_status()?
        .text()?;
    if text.len() > 4 * 1024 * 1024 {
        return Err(MetalinkError::Protocol("metalink is too large".into()));
    }
    let document: Metalink = quick_xml::de::from_str(&text)
        .map_err(|error| MetalinkError::Protocol(format!("invalid metalink: {error}")))?;

    let file = document
        .files()
        .into_iter()
        .find(|file| !file.urls().is_empty())
        .ok_or_else(|| MetalinkError::Protocol("metalink has no file URLs".into()))?;

    let entry = file
        .urls()
        .into_iter()
        .min_by_key(|entry| entry.priority.unwrap_or(u32::MAX))
        .ok_or_else(|| MetalinkError::Protocol("metalink has no usable URL".into()))?;
    let url = base.join(entry.value.trim())?;

    let filename = aether_plugin_sdk::fsutil::sanitize_filename(&file.name)
        .or_else(|| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .and_then(aether_plugin_sdk::fsutil::sanitize_filename)
        })
        .unwrap_or_else(|| "download.bin".into());

    let checksum = file.hashes().into_iter().find_map(|hash| {
        let kind = hash.kind.to_ascii_lowercase().replace('_', "-");
        if kind == "sha-256" || kind == "sha256" {
            Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: hash.value.trim().to_ascii_lowercase(),
            })
        } else {
            None
        }
    });

    Ok(Some(MetalinkInfo {
        url: url.to_string(),
        filename,
        size: file.size.as_ref().and_then(|size| size.trim().parse().ok()),
        checksum,
    }))
}

#[derive(Debug, Error)]
pub enum MetalinkError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    #[error("metalink error: {0}")]
    Protocol(String),
}

#[derive(Debug, Deserialize)]
struct Metalink {
    #[serde(rename = "file", default)]
    files: Vec<FileEntry>,
    #[serde(rename = "files")]
    legacy: Option<LegacyFiles>,
}

impl Metalink {
    fn files(&self) -> Vec<&FileEntry> {
        let mut files = self.files.iter().collect::<Vec<_>>();
        if let Some(legacy) = &self.legacy {
            files.extend(legacy.files.iter());
        }
        files
    }
}

#[derive(Debug, Deserialize)]
struct LegacyFiles {
    #[serde(rename = "file", default)]
    files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "size")]
    size: Option<String>,
    #[serde(rename = "hash", default)]
    hashes: Vec<HashEntry>,
    #[serde(rename = "url", default)]
    urls: Vec<UrlEntry>,
    #[serde(rename = "verification")]
    verification: Option<Verification>,
    #[serde(rename = "resources")]
    resources: Option<Resources>,
}

impl FileEntry {
    fn hashes(&self) -> Vec<&HashEntry> {
        let mut hashes = self.hashes.iter().collect::<Vec<_>>();
        if let Some(verification) = &self.verification {
            hashes.extend(verification.hashes.iter());
        }
        hashes
    }

    fn urls(&self) -> Vec<&UrlEntry> {
        let mut urls = self.urls.iter().collect::<Vec<_>>();
        if let Some(resources) = &self.resources {
            urls.extend(resources.urls.iter());
        }
        urls
    }
}

#[derive(Debug, Deserialize)]
struct HashEntry {
    #[serde(rename = "@type")]
    kind: String,
    #[serde(rename = "$text")]
    value: String,
}

#[derive(Debug, Deserialize)]
struct UrlEntry {
    #[serde(rename = "@priority")]
    priority: Option<u32>,
    #[serde(rename = "$text")]
    value: String,
}

#[derive(Debug, Deserialize)]
struct Verification {
    #[serde(rename = "hash", default)]
    hashes: Vec<HashEntry>,
}

#[derive(Debug, Deserialize)]
struct Resources {
    #[serde(rename = "url", default)]
    urls: Vec<UrlEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4: &str = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
        <file name="a.iso">
            <size>1024</size>
            <hash type="sha-256">abc</hash>
            <url priority="2">https://slow/a.iso</url>
            <url priority="1">https://fast/a.iso</url>
        </file>
    </metalink>"#;

    #[test]
    fn picks_best_priority() {
        let doc: Metalink = quick_xml::de::from_str(V4).unwrap();
        let file = doc.files()[0];
        let url = file
            .urls()
            .into_iter()
            .min_by_key(|entry| entry.priority.unwrap_or(u32::MAX))
            .unwrap();
        assert_eq!(url.value, "https://fast/a.iso");
        assert_eq!(file.hashes()[0].kind, "sha-256");
    }
}
