//! Install a signed runtime bundle into the user's data directory.
//!
//! A package install (`.deb`) puts root-owned programs under `/usr/bin` and
//! `/usr/lib/aether/seed`, which only dpkg can replace. The launcher
//! (`aether-launcher`) looks here first, so updates can land without root —
//! and therefore without an authorization prompt — while the signed manifest
//! stored beside each copy lets the launcher re-check it on every start.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use super::{SignedManifest, UpdateAsset, UpdateClient, UpdateError};

/// Directory holding one subdirectory per installed version.
pub const VERSIONS_DIR: &str = "versions";
/// Symlink naming the version the launcher prefers.
pub const CURRENT_LINK: &str = "current";
/// Plain-text version of the copy, used to order candidates.
pub const VERSION_FILE: &str = "version";
/// Signed release manifest kept beside the runtime.
pub const MANIFEST_FILE: &str = "update.json";
/// Detached Ed25519 signature of the manifest, as released.
pub const SIGNATURE_FILE: &str = "update.json.sig";
/// Prefix of the staging directories; anything left over is stale.
const STAGING_PREFIX: &str = ".";

/// `$AETHER_RUNTIME_ROOT`, else `$XDG_DATA_HOME/aether`, else
/// `~/.local/share/aether`; the launcher resolves the same path.
pub fn default_root() -> PathBuf {
    if let Some(root) = std::env::var_os("AETHER_RUNTIME_ROOT") {
        return PathBuf::from(root);
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        if !data.is_empty() {
            return PathBuf::from(data).join("aether");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/aether");
    }
    PathBuf::from("/nonexistent/aether")
}

/// Whether this process runs from a package install, i.e. whether a runtime
/// copy is the right update shape. The launcher exports `AETHER_LAUNCHER`;
/// the `versions/` path check covers a copy started directly.
pub fn is_package_install() -> bool {
    std::env::var_os("AETHER_LAUNCHER").is_some() || running_from_runtime_copy()
}

fn running_from_runtime_copy() -> bool {
    std::env::current_exe()
        .map(|exe| {
            let text = exe.to_string_lossy();
            text.contains("/aether/versions/") || text.contains("/aether/seed/")
        })
        .unwrap_or(false)
}

impl UpdateClient {
    /// Download every file of this platform's runtime bundle, write them into
    /// `root/versions/<version>/` together with the verified manifest, then
    /// point `current` at it. Returns the version directory.
    pub fn install_runtime(
        &self,
        signed: &SignedManifest,
        root: &Path,
    ) -> Result<PathBuf, UpdateError> {
        self.install_runtime_with_progress(signed, root, &mut |_, _| {})
    }

    pub fn install_runtime_with_progress(
        &self,
        signed: &SignedManifest,
        root: &Path,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PathBuf, UpdateError> {
        let files = self
            .runtime_files(&signed.manifest)
            .ok_or_else(|| UpdateError::Protocol("manifest has no runtime bundle".into()))?;
        if files.is_empty() {
            return Err(UpdateError::Protocol("runtime bundle is empty".into()));
        }
        validate_names(files)?;
        let version = signed.manifest.version.clone();
        let versions = root.join(VERSIONS_DIR);
        fs::create_dir_all(&versions)?;
        cleanup_staging(&versions);

        let staging = versions.join(format!(
            "{STAGING_PREFIX}{version}.tmp-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging)?;
        let total = total_size(files);
        let mut written = 0_u64;
        for (name, asset) in files {
            let artifact = self.download_artifact(
                &asset.url,
                &asset.sha256,
                asset.size,
                &self.artifact_dir(&version),
                name,
            )?;
            let target = staging.join(name);
            fs::copy(&artifact, &target)?;
            set_executable(&target)?;
            written += fs::metadata(&artifact)?.len();
            on_progress(written, total);
        }
        // The proof travels with the copy: the launcher re-verifies these two
        // files before it starts anything.
        fs::write(staging.join(MANIFEST_FILE), &signed.bytes)?;
        fs::write(staging.join(SIGNATURE_FILE), &signed.signature)?;
        fs::write(staging.join(VERSION_FILE), format!("{version}\n"))?;

        let installed = versions.join(&version);
        if installed.exists() {
            fs::remove_dir_all(&installed)?;
        }
        fs::rename(&staging, &installed)?;
        switch_current(&versions, &version)?;
        prune_old_versions(&versions, &version);
        Ok(installed)
    }

    fn artifact_dir(&self, version: &str) -> PathBuf {
        self.state_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("artifacts")
            .join(version)
    }
}

fn total_size(files: &BTreeMap<String, UpdateAsset>) -> Option<u64> {
    let mut total = 0_u64;
    for asset in files.values() {
        total += asset.size?;
    }
    Some(total)
}

/// Program names come from the signed manifest, so they are ours — but a
/// bundle entry must still be a bare file name, never a path.
fn validate_names(files: &BTreeMap<String, UpdateAsset>) -> Result<(), UpdateError> {
    for name in files.keys() {
        let bare = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\');
        if !bare {
            return Err(UpdateError::Protocol(format!(
                "runtime bundle has an invalid file name: {name}"
            )));
        }
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Point `current` at `version` atomically: build a sibling symlink, then
/// rename it over the old one.
fn switch_current(versions: &Path, version: &str) -> Result<(), UpdateError> {
    let link = versions.join(CURRENT_LINK);
    let temp = versions.join(format!(
        "{STAGING_PREFIX}current.tmp-{}",
        std::process::id()
    ));
    let _ = fs::remove_file(&temp);
    #[cfg(not(unix))]
    {
        let _ = (versions, version);
        return Err(UpdateError::Protocol(
            "runtime copies are only installed on unix".into(),
        ));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(version, &temp)?;
        fs::rename(&temp, &link)?;
        Ok(())
    }
}

/// Keep the running version and one predecessor: enough to roll back by hand,
/// small enough not to accumulate.
fn prune_old_versions(versions: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(versions) else {
        return;
    };
    let mut installed: Vec<(Version, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_dir() || name.starts_with(STAGING_PREFIX) {
                return None;
            }
            Version::parse(&name).ok().map(|version| (version, path))
        })
        .collect();
    installed.sort_by(|left, right| right.0.cmp(&left.0));
    let keep_version = Version::parse(keep).ok();
    for (version, path) in installed.into_iter().skip(2) {
        if Some(&version) == keep_version.as_ref() {
            continue;
        }
        let _ = fs::remove_dir_all(path);
    }
}

fn cleanup_staging(versions: &Path) {
    let Ok(entries) = fs::read_dir(versions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(STAGING_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
