//! Capability tokens for the `/aria2-blobs/` URLs.
//!
//! `addTorrent`/`addMetalink` hand a URL to another plugin, and that URL ends
//! up in the task record (UI, exports, screenshots, aria2 `tellStatus`). It
//! therefore must not carry the RPC token: whoever reads a task would get the
//! whole RPC API. Each URL carries an HMAC of its own blob name instead, so a
//! leaked URL reads exactly one stored payload, and rotating the RPC token
//! retires every old URL.
//!
//! The derivation is a wire contract between the daemon and the plugin that
//! mints the URL: change [`DOMAIN`] and both sides must ship together.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Prefixes every MAC so the same secret used for another purpose later
/// cannot produce a colliding capability.
const DOMAIN: &[u8] = b"aether-blob-v1:";

/// Capability authorizing a read of `name` while `secret` is the RPC token.
/// Hex, 64 characters, no characters that need URL encoding.
pub fn blob_capability(secret: &str, name: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(DOMAIN);
    mac.update(name.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_documented_derivation() {
        // Pinned so a daemon and a plugin built from different revisions
        // cannot silently disagree on the URL format.
        assert_eq!(
            blob_capability("abc123", "deadbeef.torrent"),
            "23ad7fcb0086d1fde4d418b89ca363ce92910eee5dc74dd3ca418eecddab0fdc"
        );
        assert_eq!(blob_capability("abc123", "deadbeef.torrent").len(), 64);
    }

    #[test]
    fn is_scoped_to_one_name_and_one_secret() {
        let base = blob_capability("abc123", "one.torrent");
        assert_ne!(base, blob_capability("abc123", "two.torrent"));
        assert_ne!(base, blob_capability("other", "one.torrent"));
        // Hex only: the URL needs no percent-encoding.
        assert!(base.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
