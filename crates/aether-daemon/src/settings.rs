//! Daemon-owned settings persisted next to the task database.
//!
//! The file may hold proxy credentials, so it is written with `0600` on Unix.
//! Unknown fields are rejected instead of silently dropped: a typo in a
//! settings key should surface immediately, not disappear.

use std::fs;
use std::path::{Path, PathBuf};

use aether_host::ProxySetting;
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// `None` keeps the plugin default (honor the environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySetting>,
}

impl Settings {
    pub fn path(download_dir: &Path) -> PathBuf {
        download_dir.join(".aether").join("settings.json")
    }

    /// Missing files are the normal first-run case. A corrupt file is reported
    /// and replaced by defaults so a bad edit cannot keep the daemon down.
    pub fn load(path: &Path) -> Self {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                eprintln!(
                    "aether-daemon: cannot read settings {}: {error}",
                    path.display()
                );
                return Self::default();
            }
        };
        match serde_json::from_str(&raw) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!(
                    "aether-daemon: ignoring malformed settings {}: {error}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Write through a temporary file + rename so a crash never leaves a
    /// half-written settings file behind.
    pub fn save(&self, path: &Path) -> Result<(), DaemonError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        let mut raw = serde_json::to_vec_pretty(self)?;
        raw.push(b'\n');
        fs::write(&temporary, &raw)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&temporary, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aether-settings-{label}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn roundtrips_and_restricts_permissions() {
        let path = temp_path("roundtrip");
        let settings = Settings {
            proxy: Some(ProxySetting::parse_spec("http://user:secret@10.0.0.1:3128").unwrap()),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "settings may hold credentials");
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_and_malformed_files_fall_back_to_defaults() {
        let path = temp_path("missing");
        fs::remove_file(&path).ok();
        assert_eq!(Settings::load(&path), Settings::default());

        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());

        // Unknown keys are rejected by the parser, so they also fall back.
        fs::write(&path, br#"{"proxy_mod": null}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn serializes_proxy_mode_lowercase() {
        let settings = Settings {
            proxy: Some(ProxySetting::System),
        };
        let raw = serde_json::to_string(&settings).unwrap();
        assert_eq!(raw, r#"{"proxy":{"mode":"system"}}"#);
        assert_eq!(
            serde_json::from_str::<Settings>(&raw).unwrap().proxy,
            Some(ProxySetting::System)
        );
    }

    #[test]
    fn persists_pac_settings() {
        let path = temp_path("pac");
        let settings = Settings {
            proxy: Some(ProxySetting::Pac {
                url: Some("http://wpad.example/wpad.dat".into()),
                script: None,
            }),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        fs::remove_file(&path).ok();
    }
}
