//! Disk-backed blob store for `addTorrent` / `addMetalink`.
//!
//! aria2 accepts torrent/metalink bytes inline; Aether's download plugins take
//! a URL. The bridge writes the decoded bytes under the daemon's data dir and
//! returns a `GET /aria2-blobs/<name>?token=<capability>` URL served by the
//! daemon's existing RPC listener. The name is unguessable, but it is not the
//! credential: the URL carries an HMAC of that name under the RPC token, so a
//! leaked URL reads one stored payload and never the RPC API itself.
//! Persisting to disk (instead of an in-memory loopback server) means a queued
//! task survives daemon and plugin restarts.

use aether_plugin_sdk::blob_capability;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_BLOBS: usize = 128;

#[derive(Clone)]
pub struct BlobStore {
    data_dir: PathBuf,
    base: Option<String>,
    token: Option<String>,
}

impl BlobStore {
    pub fn new(data_dir: PathBuf) -> Self {
        // Clean up files left by a previous daemon/plugin run even if no new
        // addTorrent call happens in this session.
        let directory = data_dir.join("aria2-blobs");
        let _ = fs::create_dir_all(&directory);
        restrict(&directory, true);
        cleanup(&directory);
        Self {
            data_dir,
            base: None,
            token: None,
        }
    }

    /// `listen` is the daemon's bound RPC address (`ip:port`); `token` is the
    /// RPC token the per-blob capability is derived from.
    pub fn set_base(&mut self, listen: &str, token: Option<&str>) {
        let listen = listen.trim().trim_start_matches("http://");
        let listen = if let Some(rest) = listen.strip_prefix("0.0.0.0:") {
            format!("127.0.0.1:{rest}")
        } else if let Some(rest) = listen.strip_prefix("[::]:") {
            format!("127.0.0.1:{rest}")
        } else {
            listen.to_string()
        };
        self.base = Some(format!("http://{listen}"));
        self.token = token
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
    }

    /// Publish `bytes` and return the URL the daemon should add.
    pub fn publish(&self, bytes: Vec<u8>, extension: &str) -> Result<String, String> {
        let base = self
            .base
            .as_deref()
            .ok_or_else(|| "the daemon RPC listener is disabled".to_string())?;
        // Fail closed: the capability can only be derived from the RPC token,
        // so without one the daemon would reject every read.
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| "the daemon RPC token is unavailable".to_string())?;
        let directory = self.data_dir.join("aria2-blobs");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create the blob directory: {error}"))?;
        restrict(&directory, true);
        cleanup(&directory);
        let mut token_bytes = [0u8; 16];
        getrandom::fill(&mut token_bytes).map_err(|error| format!("no random source: {error}"))?;
        let name = format!(
            "{}.{extension}",
            token_bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let path = directory.join(&name);
        fs::write(&path, &bytes).map_err(|error| format!("cannot write the blob: {error}"))?;
        restrict(&path, false);
        // The URL is stored in the task record, so it carries a capability
        // scoped to this blob rather than the RPC token itself.
        let capability = blob_capability(token, &name);
        Ok(format!("{base}/aria2-blobs/{name}?token={capability}"))
    }
}

/// Keep blobs out of reach of other local users, matching the rest of the
/// data directory.
fn restrict(path: &Path, directory: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, directory);
}

/// Drop expired blobs and keep the directory bounded.
fn cleanup(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    let mut kept: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > TTL {
            let _ = fs::remove_file(&path);
        } else {
            kept.push((modified, path));
        }
    }
    let remove = kept.len().saturating_sub(MAX_BLOBS);
    if remove > 0 {
        kept.sort_by_key(|(modified, _)| *modified);
        for (_, path) in kept.into_iter().take(remove) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aether-aria2-blobs-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn publishes_a_persistent_blob_url() {
        let root = temp_dir("publish");
        let mut store = BlobStore::new(root.clone());
        store.set_base("0.0.0.0:6800", Some("abc123"));
        let url = store
            .publish(b"d4:infod4:name4:teste".to_vec(), "torrent")
            .unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:6800/aria2-blobs/"),
            "{url}"
        );
        let (head, capability) = url.split_once("?token=").expect("token query");
        let name = head.rsplit('/').next().unwrap();
        assert_eq!(capability, blob_capability("abc123", name));
        assert_ne!(capability, "abc123", "the RPC token must not be the URL");
        let stored = fs::read(root.join("aria2-blobs").join(name)).unwrap();
        assert_eq!(stored, b"d4:infod4:name4:teste");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_to_publish_without_a_token() {
        let root = temp_dir("no-token");
        let mut store = BlobStore::new(root.clone());
        store.set_base("127.0.0.1:6800", None);
        assert!(store.publish(b"x".to_vec(), "torrent").is_err());
        fs::remove_dir_all(&root).ok();
    }
}
