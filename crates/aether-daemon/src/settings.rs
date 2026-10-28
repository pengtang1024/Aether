//! Daemon-owned settings persisted next to the task database.
//!
//! The file may hold proxy credentials, so it is written with `0600` on Unix.
//! Unknown fields are rejected instead of silently dropped: a typo in a
//! settings key should surface immediately, not disappear.

use std::fs;
use std::path::{Path, PathBuf};

use aether_host::ProxySetting;
use aether_plugin_sdk::{IpMode, NetworkConfig};
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// `None` keeps the plugin default (honor the environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySetting>,
    /// Overrides the `Aether/<version>` user agent plugins send. Blank input
    /// is normalised to `None` by `set_settings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// `None` lets the system try every address family in its usual order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_mode: Option<IpMode>,
    /// Comma/space separated DNS server addresses, validated by
    /// [`aether_plugin_sdk::NetworkConfig::parse_dns_servers`]. Blank input is
    /// normalised to `None` by `set_settings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<String>,
}

impl Settings {
    pub fn path(download_dir: &Path) -> PathBuf {
        download_dir.join(".aether").join("settings.json")
    }

    /// The network configuration handed to plugin processes, or `None` when
    /// every knob is at its default. A stored DNS list that no longer parses
    /// (hand-edited file) degrades to the system resolver with a warning.
    pub fn network(&self) -> Option<NetworkConfig> {
        let user_agent = self.user_agent.clone().filter(|ua| !ua.trim().is_empty());
        let dns = match self.dns.as_deref() {
            Some(text) if !text.trim().is_empty() => match NetworkConfig::parse_dns_servers(text) {
                Ok(servers) => Some(servers),
                Err(error) => {
                    eprintln!("aether-daemon: ignoring DNS setting: {error}");
                    None
                }
            },
            _ => None,
        };
        if user_agent.is_none() && self.ip_mode.is_none() && dns.is_none() {
            return None;
        }
        Some(NetworkConfig {
            user_agent,
            ip_mode: self.ip_mode,
            dns,
        })
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
            ..Settings::default()
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
            ..Settings::default()
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
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn network_config_follows_the_stored_settings() {
        use aether_plugin_sdk::IpMode;

        // Everything at the default means plugins keep their own behaviour.
        assert_eq!(Settings::default().network(), None);

        let settings = Settings {
            user_agent: Some("TestUA/9".into()),
            ip_mode: Some(IpMode::Ipv4Only),
            dns: Some("223.5.5.5, [::1]:5353".into()),
            ..Settings::default()
        };
        let network = settings.network().unwrap();
        assert_eq!(network.user_agent.as_deref(), Some("TestUA/9"));
        assert_eq!(network.ip_mode, Some(IpMode::Ipv4Only));
        assert_eq!(
            network.dns.as_deref(),
            Some(&["223.5.5.5".to_owned(), "[::1]:5353".to_owned()][..])
        );

        // A hand-edited file can hold a list that no longer parses: degrade to
        // the system resolver instead of refusing to run.
        let settings = Settings {
            dns: Some("not-an-ip".into()),
            ..Settings::default()
        };
        assert!(settings.network().is_none());
    }

    #[test]
    fn roundtrips_network_fields() {
        use aether_plugin_sdk::IpMode;

        let path = temp_path("network");
        let settings = Settings {
            user_agent: Some("RoundTrip/1".into()),
            ip_mode: Some(IpMode::Ipv6Only),
            dns: Some("1.1.1.1".into()),
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);

        // Unknown keys keep falling back to defaults with the new fields too.
        fs::write(&path, br#"{"ip_modes": "ipv4_only"}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::remove_file(&path).unwrap();
    }
}
