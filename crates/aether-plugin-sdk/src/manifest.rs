//! `plugin.json` manifest parsing and validation.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::protocol::{Capability, API_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// Separate process over stdio + MessagePack (default, recommended).
    #[default]
    Process,
    /// In-process `cdylib` loaded through the Aether ABI.
    Cdylib,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Lowercase, globally unique plugin id (for example `http` or `fake`).
    pub id: String,
    pub name: String,
    pub version: String,
    /// IPC/API major version. Must match the Host's [`API_VERSION`].
    pub api: u16,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub min_host: Option<String>,
    #[serde(default)]
    pub max_host: Option<String>,
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Platform key -> executable path relative to the plugin package root.
    #[serde(default)]
    pub executable: BTreeMap<String, String>,
    #[serde(default)]
    pub settings_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub detail_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub signature: Option<String>,
}

impl PluginManifest {
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        let json = std::fs::read_to_string(path)?;
        Self::from_json(&json)
    }

    /// Validate the fields needed to identify a plugin during handshake.
    pub fn validate_identity(&self) -> Result<(), ManifestError> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(ManifestError::Invalid(
                "id must contain only lowercase ASCII letters, digits, '-' or '_'".into(),
            ));
        }

        if self.name.trim().is_empty() {
            return Err(ManifestError::Invalid("name must not be empty".into()));
        }

        Version::parse(&self.version)
            .map_err(|error| ManifestError::Invalid(format!("invalid version: {error}")))?;

        if self.api == 0 {
            return Err(ManifestError::Invalid("api must be >= 1".into()));
        }

        if self.api != API_VERSION {
            return Err(ManifestError::Invalid(format!(
                "manifest api {} does not match SDK api {API_VERSION}",
                self.api
            )));
        }

        if self.protocols.is_empty() {
            return Err(ManifestError::Invalid("protocols must not be empty".into()));
        }

        Ok(())
    }

    /// Validate the full manifest, including executable paths and host range.
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.validate_identity()?;

        if self.executable.is_empty() {
            return Err(ManifestError::Invalid(
                "executable map must not be empty".into(),
            ));
        }
        for (platform, path) in &self.executable {
            if platform.trim().is_empty() || path.trim().is_empty() {
                return Err(ManifestError::Invalid(
                    "executable platform/path must not be empty".into(),
                ));
            }
        }

        if let Some(min_host) = &self.min_host {
            Version::parse(min_host)
                .map_err(|error| ManifestError::Invalid(format!("invalid min_host: {error}")))?;
        }
        if let Some(max_host) = &self.max_host {
            Version::parse(max_host)
                .map_err(|error| ManifestError::Invalid(format!("invalid max_host: {error}")))?;
        }

        Ok(())
    }

    pub fn executable_for(&self, platform: &str) -> Option<&str> {
        self.executable.get(platform).map(String::as_str)
    }

    /// Resolve the executable for the current platform inside `package_root`.
    ///
    /// Absolute paths and `..` components are rejected, and the canonical
    /// result must stay inside the package root.
    pub fn resolve_executable(&self, package_root: &Path) -> Result<PathBuf, ManifestError> {
        let platform = current_platform_key();
        let relative = self
            .executable_for(platform)
            .ok_or_else(|| ManifestError::MissingExecutable(platform.to_string()))?;
        let relative = Path::new(relative);

        if relative.is_absolute() {
            return Err(ManifestError::UnsafePath(relative.display().to_string()));
        }
        if relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(ManifestError::UnsafePath(relative.display().to_string()));
        }

        let canonical_root = package_root.canonicalize()?;
        let canonical = canonical_root.join(relative).canonicalize()?;
        if !canonical.starts_with(&canonical_root) {
            return Err(ManifestError::UnsafePath(canonical.display().to_string()));
        }
        if !canonical.is_file() {
            return Err(ManifestError::MissingExecutable(
                canonical.display().to_string(),
            ));
        }

        Ok(canonical)
    }

    /// Check `min_host` / `max_host` against the running Host version.
    pub fn is_compatible_with_host(&self, host_version: &str) -> Result<bool, ManifestError> {
        let host = Version::parse(host_version)
            .map_err(|error| ManifestError::Invalid(format!("invalid host version: {error}")))?;

        if let Some(min_host) = &self.min_host {
            let min = Version::parse(min_host)
                .map_err(|error| ManifestError::Invalid(format!("invalid min_host: {error}")))?;
            if host < min {
                return Ok(false);
            }
        }
        if let Some(max_host) = &self.max_host {
            let max = Version::parse(max_host)
                .map_err(|error| ManifestError::Invalid(format!("invalid max_host: {error}")))?;
            if host > max {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

/// Manifest key for the current host platform, matching the plugin matrix in
/// `docs/PLUGIN_ARCHITECTURE.md`.
pub fn current_platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-arm64",
        ("macos", _) => "macos-universal",
        _ => "unknown",
    }
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("manifest json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid manifest: {0}")]
    Invalid(String),

    #[error("no executable for platform {0}")]
    MissingExecutable(String),

    #[error("unsafe executable path: {0}")]
    UnsafePath(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest() -> PluginManifest {
        let mut manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "fake",
            "name": "Fake plugin",
            "version": "0.1.0",
            "api": API_VERSION,
            "protocols": ["http"],
            "executable": {}
        }))
        .unwrap();
        manifest
            .executable
            .insert(current_platform_key().into(), "bin/fake".into());
        manifest
    }

    #[test]
    fn parses_minimal_manifest() {
        let manifest = minimal_manifest();
        manifest.validate().unwrap();
        assert_eq!(
            manifest.executable_for(current_platform_key()),
            Some("bin/fake")
        );
    }

    #[test]
    fn rejects_uppercase_id() {
        let mut manifest = minimal_manifest();
        manifest.id = "Fake".into();
        assert!(manifest.validate_identity().is_err());
    }

    #[test]
    fn rejects_wrong_api() {
        let mut manifest = minimal_manifest();
        manifest.api = API_VERSION + 1;
        assert!(manifest.validate_identity().is_err());
    }

    #[test]
    fn host_range_is_inclusive() {
        let mut manifest = minimal_manifest();
        manifest.min_host = Some("0.2.0".into());
        manifest.max_host = Some("0.3.0".into());
        assert!(manifest.is_compatible_with_host("0.2.0").unwrap());
        assert!(manifest.is_compatible_with_host("0.3.0").unwrap());
        assert!(!manifest.is_compatible_with_host("0.1.9").unwrap());
        assert!(!manifest.is_compatible_with_host("0.3.1").unwrap());
    }

    #[test]
    fn rejects_parent_dir_executable() {
        let mut manifest = minimal_manifest();
        manifest
            .executable
            .insert(current_platform_key().into(), "../fake".into());
        let root = std::env::temp_dir();
        assert!(matches!(
            manifest.resolve_executable(&root),
            Err(ManifestError::UnsafePath(_))
        ));
    }

    #[test]
    fn platform_key_is_known() {
        assert_ne!(current_platform_key(), "unknown");
    }
}
