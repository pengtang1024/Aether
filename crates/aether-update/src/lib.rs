//! Signed auto-update client.
//!
//! Flow:
//! 1. fetch `update.json` and detached `update.json.sig`;
//! 2. verify the Ed25519 signature with a compiled-in public key;
//! 3. compare semver and select the current platform asset;
//! 4. download to staging, verify SHA-256, then atomically apply or report
//!    that the package needs a manual install (`.deb`).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub notes_url: Option<String>,
    #[serde(default)]
    pub assets: BTreeMap<String, UpdateAsset>,
    /// Optional plugin releases keyed by plugin id; absent in older
    /// manifests (serde default keeps them parsing).
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginUpdateEntry>,
    /// Staged rollout percentage (0-100, default 100). Each machine maps to
    /// a stable bucket from its hostname and the target version, so a
    /// manifest at 25% reaches roughly a quarter of installs and the same
    /// machine always gets the same answer. Raising the percentage requires
    /// re-signing (the signature covers these bytes).
    #[serde(default = "full_rollout")]
    pub rollout: u8,
}

fn full_rollout() -> u8 {
    100
}

/// One plugin entry in the update manifest: the release a plugin should be
/// updated to. Covered by the manifest's own detached signature, so the
/// URL and SHA-256 are authenticated; the package's inner manifest
/// signature (4.6a) is verified again at install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginUpdateEntry {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAsset {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    pub last_check: i64,
    pub latest_version: Option<String>,
    pub status: String,
    pub artifact: Option<PathBuf>,
    pub error: Option<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            last_check: 0,
            latest_version: None,
            status: "idle".into(),
            artifact: None,
            error: None,
        }
    }
}

pub struct UpdateClient {
    manifest_url: Url,
    public_key: VerifyingKey,
    current_version: Version,
    platform: String,
    state_path: PathBuf,
    http: Client,
}

impl UpdateClient {
    pub fn new(
        manifest_url: &str,
        public_key_base64: &str,
        current_version: &str,
        platform: &str,
        state_dir: &Path,
    ) -> Result<Self, UpdateError> {
        let public_key = decode_public_key(public_key_base64)?;
        let current_version = Version::parse(current_version)
            .map_err(|error| UpdateError::Protocol(format!("current version: {error}")))?;
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .user_agent("Aether-Updater/0.1")
            .build()?;
        fs::create_dir_all(state_dir)?;
        Ok(Self {
            manifest_url: Url::parse(manifest_url)?,
            public_key,
            current_version,
            platform: platform.to_string(),
            state_path: state_dir.join("update-state.json"),
            http,
        })
    }

    /// Fetch + verify + compare. Returns `Ok(None)` when up to date.
    /// Fetch and verify the manifest, without comparing it to the running
    /// version. The plugin update channel uses this: plugin entries matter
    /// even when the app itself is up to date.
    pub fn check_manifest(&self) -> Result<UpdateManifest, UpdateError> {
        let bytes = self.fetch(self.manifest_url.as_str())?;
        let signature_bytes = self.fetch(&format!("{}.sig", self.manifest_url))?;
        verify_signature(&bytes, &signature_bytes, &self.public_key)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn check(&self) -> Result<Option<UpdateManifest>, UpdateError> {
        let manifest = self.check_manifest()?;
        let version = Version::parse(&manifest.version)
            .map_err(|error| UpdateError::Protocol(format!("manifest version: {error}")))?;
        if version <= self.current_version {
            return Ok(None);
        }
        if manifest.assets.is_empty() {
            return Err(UpdateError::Protocol("manifest has no assets".into()));
        }
        if manifest.rollout > 100 {
            return Err(UpdateError::Protocol(format!(
                "manifest rollout out of range: {}",
                manifest.rollout
            )));
        }
        if !in_rollout(&manifest.version, manifest.rollout) {
            // Staged rollout: this machine's turn has not come yet. It is
            // "up to date" for now — the next check re-evaluates the same
            // stable bucket.
            return Ok(None);
        }
        Ok(Some(manifest))
    }

    pub fn asset_for_platform<'a>(
        &self,
        manifest: &'a UpdateManifest,
    ) -> Result<&'a UpdateAsset, UpdateError> {
        manifest
            .assets
            .get(&self.platform)
            .ok_or_else(|| UpdateError::Protocol(format!("no update asset for {}", self.platform)))
    }

    /// Download an asset into `<state>/<version>/`, verify size + SHA-256.
    pub fn download(
        &self,
        manifest: &UpdateManifest,
        asset: &UpdateAsset,
    ) -> Result<PathBuf, UpdateError> {
        let directory = self
            .state_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("artifacts")
            .join(&manifest.version);
        let target = self.download_artifact(
            &asset.url,
            &asset.sha256,
            asset.size,
            &directory,
            "aether-update",
        )?;
        self.save_state(&UpdateState {
            last_check: now_ms(),
            latest_version: Some(manifest.version.clone()),
            status: "downloaded".into(),
            artifact: Some(target.clone()),
            error: None,
        })?;
        Ok(target)
    }

    /// Download one artifact into `directory`, verifying size (when known)
    /// and SHA-256 before it is moved into place. Shared by app and plugin
    /// updates: nothing is trusted until the bytes match the signed
    /// manifest entry.
    pub fn download_artifact(
        &self,
        url: &str,
        sha256: &str,
        size: Option<u64>,
        directory: &Path,
        filename_hint: &str,
    ) -> Result<PathBuf, UpdateError> {
        fs::create_dir_all(directory)?;
        let filename = url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(filename_hint);
        let target = directory.join(filename);
        let temp = directory.join(format!("{filename}.part"));

        let mut response = self.http.get(url).send()?.error_for_status()?;
        let mut file = fs::File::create(&temp)?;
        let mut digest = Sha256::new();
        let mut written = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            digest.update(&buffer[..read]);
            written += read as u64;
        }
        file.sync_all()?;

        if let Some(size) = size {
            if written != size {
                let _ = fs::remove_file(&temp);
                return Err(UpdateError::SizeMismatch {
                    expected: size,
                    actual: written,
                });
            }
        }
        let actual = hex(&digest.finalize());
        if !actual.eq_ignore_ascii_case(sha256) {
            let _ = fs::remove_file(&temp);
            return Err(UpdateError::ChecksumMismatch);
        }
        fs::rename(&temp, &target)?;
        Ok(target)
    }

    /// Replace an AppImage atomically, keeping `<target>.old` for rollback.
    pub fn apply_appimage(&self, artifact: &Path, target: &Path) -> Result<(), UpdateError> {
        if !artifact.is_file() {
            return Err(UpdateError::Protocol("artifact is missing".into()));
        }
        let backup = sibling(target, "old");
        if target.is_file() {
            fs::copy(target, &backup)?;
        }
        let temp = sibling(target, "new");
        fs::copy(artifact, &temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
        }
        fs::rename(&temp, target)?;
        self.save_state(&UpdateState {
            last_check: now_ms(),
            latest_version: self
                .read_state()
                .ok()
                .and_then(|state| state.latest_version),
            status: "applied".into(),
            artifact: Some(artifact.to_path_buf()),
            error: None,
        })?;
        Ok(())
    }

    pub fn rollback_appimage(target: &Path) -> Result<(), UpdateError> {
        let backup = sibling(target, "old");
        if !backup.is_file() {
            return Err(UpdateError::Protocol("no rollback backup".into()));
        }
        fs::copy(backup, target)?;
        Ok(())
    }

    pub fn state(&self) -> Result<UpdateState, UpdateError> {
        self.read_state()
    }

    pub fn save_error(&self, error: &str) -> Result<(), UpdateError> {
        self.save_state(&UpdateState {
            last_check: now_ms(),
            latest_version: None,
            status: "error".into(),
            artifact: None,
            error: Some(error.to_string()),
        })
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, UpdateError> {
        let mut response = self.http.get(url).send()?.error_for_status()?;
        let mut bytes = Vec::new();
        response.read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(UpdateError::Protocol("manifest response too large".into()));
        }
        Ok(bytes)
    }

    fn save_state(&self, state: &UpdateState) -> Result<(), UpdateError> {
        fs::write(&self.state_path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    fn read_state(&self) -> Result<UpdateState, UpdateError> {
        match fs::read(&self.state_path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(UpdateState::default())
            }
            Err(error) => Err(UpdateError::Io(error)),
        }
    }
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{}", target.to_string_lossy(), suffix))
}

fn decode_public_key(value: &str) -> Result<VerifyingKey, UpdateError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| UpdateError::Protocol(format!("public key base64: {error}")))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| UpdateError::Protocol("public key must be 32 bytes".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|error| UpdateError::Protocol(format!("public key: {error}")))
}

fn verify_signature(
    manifest: &[u8],
    signature: &[u8],
    key: &VerifyingKey,
) -> Result<(), UpdateError> {
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .map_err(|error| UpdateError::Protocol(format!("signature base64: {error}")))?;
    let signature = Signature::from_slice(&signature)
        .map_err(|error| UpdateError::Protocol(format!("signature: {error}")))?;
    key.verify(manifest, &signature)
        .map_err(|_| UpdateError::Signature)
}

/// Canonical bytes for a plugin manifest signature: the manifest JSON with
/// the `signature` key removed. `serde_json::Value` maps are sorted
/// (this workspace does not enable `preserve_order`), so the bytes are
/// identical no matter how the file was formatted or ordered.
pub fn canonical_manifest_bytes(manifest: &Value) -> Vec<u8> {
    let mut clone = manifest.clone();
    if let Some(object) = clone.as_object_mut() {
        object.remove("signature");
    }
    // Serializing a Value cannot fail.
    serde_json::to_vec(&clone).expect("serde_json::Value serialization")
}

/// Verify a plugin manifest's embedded Ed25519 `signature` (base64) against
/// the release public key (same trust root as update manifests).
pub fn verify_manifest_signature(
    manifest: &Value,
    public_key_base64: &str,
) -> Result<(), UpdateError> {
    let signature = manifest
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(|| UpdateError::Protocol("manifest has no signature".into()))?;
    let key = decode_public_key(public_key_base64)?;
    verify_signature(
        &canonical_manifest_bytes(manifest),
        signature.as_bytes(),
        &key,
    )
}

/// Verify every entry of the manifest's `files` map (relative path → sha256)
/// against the package on disk. The map is inside the signed bytes, so this
/// is what makes the *binaries* — not just the manifest — authenticated.
pub fn verify_package_files(package_root: &Path, manifest: &Value) -> Result<(), UpdateError> {
    let files = manifest
        .get("files")
        .and_then(Value::as_object)
        .filter(|files| !files.is_empty())
        .ok_or_else(|| UpdateError::Protocol("manifest has no files map".into()))?;
    for (relative, expected) in files {
        let expected = expected
            .as_str()
            .ok_or_else(|| UpdateError::Protocol(format!("files.{relative} is not a string")))?;
        let path = Path::new(relative);
        let unsafe_path = path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            });
        if unsafe_path {
            return Err(UpdateError::Protocol(format!(
                "files entry is not a safe relative path: {relative}"
            )));
        }
        let bytes = fs::read(package_root.join(path)).map_err(|error| {
            UpdateError::Protocol(format!("missing package file {relative}: {error}"))
        })?;
        let mut digest = Sha256::new();
        digest.update(&bytes);
        if !hex(&digest.finalize()).eq_ignore_ascii_case(expected) {
            return Err(UpdateError::Protocol(format!(
                "hash mismatch for package file {relative}"
            )));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Unpack a plugin `.tar.gz` into `destination` (created fresh).
///
/// Guards: only plain files and directories, relative paths only (no `..`,
/// no absolute paths, no symlinks/hardlinks), and a total unpacked size cap
/// so a hostile archive cannot fill the disk.
pub fn unpack_plugin_archive(
    archive: &Path,
    destination: &Path,
    max_bytes: u64,
) -> Result<(), UpdateError> {
    let file = fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    fs::create_dir_all(destination)?;
    let mut total = 0_u64;
    for entry in tar
        .entries()
        .map_err(|error| UpdateError::Protocol(format!("plugin archive: {error}")))?
    {
        let mut entry = entry
            .map_err(|error| UpdateError::Protocol(format!("plugin archive entry: {error}")))?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(UpdateError::Protocol(format!(
                "plugin archive contains a non-regular entry ({entry_type:?})"
            )));
        }
        let path = entry
            .path()
            .map_err(|error| UpdateError::Protocol(format!("plugin archive entry path: {error}")))?
            .into_owned();
        let unsafe_path = path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if unsafe_path {
            return Err(UpdateError::Protocol(format!(
                "plugin archive entry escapes the package: {}",
                path.display()
            )));
        }
        if entry_type.is_file() {
            total = total.saturating_add(entry.header().size().unwrap_or(0));
            if total > max_bytes {
                return Err(UpdateError::Protocol(format!(
                    "plugin archive unpacks larger than {max_bytes} bytes"
                )));
            }
        }
        entry.unpack_in(destination).map_err(|error| {
            UpdateError::Protocol(format!("unpacking {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("url error: {0}")]
    Url(#[from] url::ParseError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("invalid update signature")]
    Signature,

    #[error("update size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("update checksum mismatch")]
    ChecksumMismatch,
}

/// Staged-rollout bucket for this machine and target version: stable across
/// runs, no randomness and no stored identity. The hostname never leaves the
/// machine — it only feeds a local hash.
fn in_rollout(version: &str, percentage: u8) -> bool {
    percentage >= 100 || rollout_bucket(version) < percentage
}

fn rollout_bucket(version: &str) -> u8 {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "aether".to_string());
    let digest = Sha256::digest(format!("{host}\n{version}").as_bytes());
    digest[0] % 100
}

#[cfg(test)]
mod rollout_tests {
    use super::*;

    #[test]
    fn rollout_boundaries_are_exact() {
        assert!(!in_rollout("9.9.9", 0), "0% reaches nobody");
        assert!(in_rollout("9.9.9", 100), "100% reaches everybody");
    }

    #[test]
    fn bucket_is_stable_and_per_version() {
        let first = rollout_bucket("1.2.3");
        assert_eq!(first, rollout_bucket("1.2.3"), "same version, same bucket");
        assert!(first < 100);
        // The bucket comparison must agree with the boundary semantics.
        assert_eq!(in_rollout("1.2.3", first + 1), true);
        assert_eq!(in_rollout("1.2.3", first), false);
    }

    #[test]
    fn manifest_rollout_defaults_to_full() {
        let manifest: UpdateManifest = serde_json::from_value(serde_json::json!({
            "version": "0.3.0",
            "assets": {"linux-x86_64": {"url": "https://example.test/a", "sha256": "00"}}
        }))
        .expect("minimal manifest parses");
        assert_eq!(manifest.rollout, 100);
    }
}
