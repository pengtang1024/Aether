//! Disk-backed blob store for `addTorrent` / `addMetalink`.
//!
//! aria2 accepts torrent/metalink bytes inline; Aether's download plugins take
//! a URL. The bridge writes the decoded bytes under the daemon's data dir and
//! returns a `GET /aria2-blobs/<token>.<ext>` URL served by the daemon's
//! existing RPC listener. Persisting to disk (instead of an in-memory loopback
//! server) means a queued task survives daemon and plugin restarts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_BLOBS: usize = 128;

#[derive(Clone)]
pub struct BlobStore {
    data_dir: PathBuf,
    base: Option<String>,
}

impl BlobStore {
    pub fn new(data_dir: PathBuf) -> Self {
        // Clean up files left by a previous daemon/plugin run even if no new
        // addTorrent call happens in this session.
        let directory = data_dir.join("aria2-blobs");
        let _ = fs::create_dir_all(&directory);
        cleanup(&directory);
        Self {
            data_dir,
            base: None,
        }
    }

    /// `listen` is the daemon's bound RPC address (`ip:port`).
    pub fn set_base(&mut self, listen: &str) {
        let listen = listen.trim().trim_start_matches("http://");
        let listen = if let Some(rest) = listen.strip_prefix("0.0.0.0:") {
            format!("127.0.0.1:{rest}")
        } else if let Some(rest) = listen.strip_prefix("[::]:") {
            format!("127.0.0.1:{rest}")
        } else {
            listen.to_string()
        };
        self.base = Some(format!("http://{listen}"));
    }

    /// Publish `bytes` and return the URL the daemon should add.
    pub fn publish(&self, bytes: Vec<u8>, extension: &str) -> Result<String, String> {
        let base = self
            .base
            .as_deref()
            .ok_or_else(|| "the daemon RPC listener is disabled".to_string())?;
        let directory = self.data_dir.join("aria2-blobs");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create the blob directory: {error}"))?;
        cleanup(&directory);
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).map_err(|error| format!("no random source: {error}"))?;
        let name = format!(
            "{}.{extension}",
            token
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        fs::write(directory.join(&name), &bytes)
            .map_err(|error| format!("cannot write the blob: {error}"))?;
        Ok(format!("{base}/aria2-blobs/{name}"))
    }
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
        store.set_base("0.0.0.0:6800");
        let url = store
            .publish(b"d4:infod4:name4:teste".to_vec(), "torrent")
            .unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:6800/aria2-blobs/"),
            "{url}"
        );
        let name = url.rsplit('/').next().unwrap();
        let stored = fs::read(root.join("aria2-blobs").join(name)).unwrap();
        assert_eq!(stored, b"d4:infod4:name4:teste");
        fs::remove_dir_all(&root).ok();
    }
}
