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
    /// Maximum number of simultaneously running tasks; the rest stay queued.
    /// `0` is rejected: a queue with no capacity would never start anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u64>,
    /// Global download limit in bytes per second, shared by the tasks started
    /// while it is in force. `0` removes the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_speed_bps: Option<u64>,
    /// Parallel connections one HTTP(S) task may open, capped at
    /// [`CONNECTIONS_MAX`] by the http plugin. Resolution turns `None` into
    /// [`CONNECTIONS_DEFAULT`], so the setting always reports a real count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<u32>,
    /// Automatic retry cap per task; `0` disables automatic retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_max: Option<u32>,
    /// Default request headers for HTTP-family plugins: `"Key: Value"` pairs
    /// separated by newlines or semicolons, validated by
    /// [`aether_plugin_sdk::NetworkConfig::parse_header_list`]. A per-task
    /// header of the same name wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_headers: Option<String>,
    /// FTP data connection mode. `None`/`Some(true)` = passive, `Some(false)`
    /// = active for firewalls that require it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ftp_passive: Option<bool>,
    /// BitTorrent listen port (`1..=65535`); the plugin falls back to
    /// `AETHER_BT_LISTEN_PORT` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bt_listen_port: Option<u16>,
    /// BitTorrent DHT; unset keeps the plugin default (enabled unless
    /// `AETHER_BT_DISABLE_DHT` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bt_dht: Option<bool>,
    /// BitTorrent upload cap in bytes per second; `0` or unset = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bt_upload_bps: Option<u64>,
    /// BitTorrent blocklist ("dat" format): a path, `file://` URL or
    /// `http(s)://` URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bt_blocklist: Option<String>,
    /// Extra trackers for every torrent, separated by newlines or semicolons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bt_trackers: Option<String>,
    /// Scheduled download window: when enabled and both times are set, the
    /// daemon only launches and keeps running tasks inside the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_on: Option<bool>,
    /// Window start, `"HH:MM"` in the daemon host's local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_start: Option<String>,
    /// Window end, `"HH:MM"`. `end < start` means the window wraps midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_end: Option<String>,
    /// RPC over TCP: `"ip:port"` to listen on, off when unset. Requires
    /// [`Settings::rpc_secret`] on any non-loopback address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_listen: Option<String>,
    /// Token every RPC connection must present as its first message
    /// (`{"method":"auth","settings":{"token":"…"}}`). Blank input is
    /// normalised to `None` by `set_settings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_secret: Option<String>,
    /// ffmpeg binary for plugins that shell out (rtmp capture, media
    /// remux). Injected into task settings at `add` time when the task
    /// does not set its own `ffmpeg_path`; blank input clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg_path: Option<String>,
}

/// The upper bound the http plugin honours for [`Settings::connections`]:
/// it clamps `Budget::connections` to this when it opens range connections.
pub const CONNECTIONS_MAX: u32 = 16;

/// The connection count the daemon assumes when no setting asks for one; it
/// must stay equal to `Budget::default().connections` (asserted in the tests).
pub const CONNECTIONS_DEFAULT: u32 = 4;

/// Every key `set_settings` accepts. The file is parsed with
/// `deny_unknown_fields`, so the RPC rejects the same typos instead of
/// silently storing nothing.
pub const KEYS: [&str; 21] = [
    "proxy",
    "user_agent",
    "ip_mode",
    "dns",
    "max_concurrent",
    "max_speed_bps",
    "connections",
    "retry_max",
    "default_headers",
    "ftp_passive",
    "bt_listen_port",
    "bt_dht",
    "bt_upload_bps",
    "bt_blocklist",
    "bt_trackers",
    "schedule_on",
    "schedule_start",
    "schedule_end",
    "rpc_listen",
    "rpc_secret",
    "ffmpeg_path",
];

/// The limits in force: `settings.json` wins, the boot configuration
/// (`AETHER_MAX_*`) fills what the file leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_concurrent: usize,
    /// `None` means no limit.
    pub max_speed_bps: Option<u64>,
    pub retry_max: u32,
}

/// Split a newline/semicolon separated list (trackers, …) into trimmed,
/// non-empty entries. `None` when nothing survives, so "blank" and "unset"
/// stay indistinguishable in `NetworkConfig`.
fn split_list(text: Option<&str>) -> Option<Vec<String>> {
    let entries = text?
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(entries)
}

/// Parse `"HH:MM"` into minutes since midnight; `None` for anything the user
/// could not have meant as a clock time.
pub fn parse_hhmm(text: &str) -> Option<u32> {
    let text = text.trim();
    let (hours, minutes) = text.split_once(':')?;
    let hours: u32 = hours.trim().parse().ok()?;
    let minutes: u32 = minutes.trim().parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

impl Settings {
    pub fn path(download_dir: &Path) -> PathBuf {
        download_dir.join(".aether").join("settings.json")
    }

    /// The network configuration handed to plugin processes, or `None` when
    /// every knob is at its default. A stored list that no longer parses
    /// (hand-edited file) degrades to the default with a warning instead of
    /// refusing to run.
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
        let default_headers = match self.default_headers.as_deref() {
            Some(text) if !text.trim().is_empty() => match NetworkConfig::parse_header_list(text) {
                Ok(headers) => Some(headers),
                Err(error) => {
                    eprintln!("aether-daemon: ignoring header setting: {error}");
                    None
                }
            },
            _ => None,
        };
        let bt_trackers = split_list(self.bt_trackers.as_deref());
        if user_agent.is_none()
            && self.ip_mode.is_none()
            && dns.is_none()
            && default_headers.is_none()
            && self.ftp_passive.is_none()
            && self.bt_listen_port.is_none()
            && self.bt_dht.is_none()
            && self.bt_upload_bps.is_none()
            && self.bt_blocklist.is_none()
            && bt_trackers.is_none()
        {
            return None;
        }
        Some(NetworkConfig {
            user_agent,
            ip_mode: self.ip_mode,
            dns,
            default_headers,
            ftp_passive: self.ftp_passive,
            bt_listen_port: self.bt_listen_port,
            bt_dht: self.bt_dht,
            // `0` is the stored form of "no cap"; plugins treat `None` the
            // same way, so normalise before handing it over.
            bt_upload_bps: self.bt_upload_bps.filter(|bps| *bps > 0),
            bt_blocklist: self
                .bt_blocklist
                .clone()
                .filter(|path| !path.trim().is_empty()),
            bt_trackers,
        })
    }

    /// Resolve the limits the daemon runs with and write them back, so
    /// `get_settings` reports what is in force instead of `null`. A value the
    /// engine cannot honour is logged and replaced by the boot value: a
    /// daemon that refuses to start over one bad line in a hand-edited file
    /// is worse than one that says so and keeps running.
    pub fn resolve_limits(&mut self, boot: Limits) -> Limits {
        let resolved = Limits {
            max_concurrent: match self.max_concurrent {
                Some(0) => {
                    eprintln!(
                        "aether-daemon: ignoring max_concurrent 0, using {}",
                        boot.max_concurrent
                    );
                    boot.max_concurrent
                }
                Some(value) => value as usize,
                None => boot.max_concurrent,
            },
            // `0` is the stored form of "no limit"; `None` means the file
            // never chose, so the boot value applies.
            max_speed_bps: match self.max_speed_bps {
                Some(0) => None,
                Some(value) => Some(value),
                None => boot.max_speed_bps,
            },
            retry_max: self.retry_max.unwrap_or(boot.retry_max),
        };
        let connections = match self.connections {
            Some(value) if (1..=CONNECTIONS_MAX).contains(&value) => value,
            Some(value) => {
                eprintln!(
                    "aether-daemon: ignoring connections {value}, using {CONNECTIONS_DEFAULT}"
                );
                CONNECTIONS_DEFAULT
            }
            None => CONNECTIONS_DEFAULT,
        };
        self.max_concurrent = Some(resolved.max_concurrent as u64);
        self.max_speed_bps = Some(resolved.max_speed_bps.unwrap_or(0));
        self.retry_max = Some(resolved.retry_max);
        self.connections = Some(connections);
        resolved
    }

    /// Reject values the engine cannot honour before anything is applied or
    /// stored, so `set_settings` can report the error and leave the previous
    /// settings running.
    pub fn validate(&self) -> Result<(), DaemonError> {
        if self.max_concurrent == Some(0) {
            return Err(DaemonError::Protocol(
                "max_concurrent must be at least 1".into(),
            ));
        }
        if let Some(connections) = self.connections {
            if !(1..=CONNECTIONS_MAX).contains(&connections) {
                return Err(DaemonError::Protocol(format!(
                    "connections must be within 1..={CONNECTIONS_MAX}, got {connections}"
                )));
            }
        }
        if let Some(headers) = self.default_headers.as_deref() {
            if !headers.trim().is_empty() {
                aether_plugin_sdk::NetworkConfig::parse_header_list(headers)
                    .map_err(DaemonError::Protocol)?;
            }
        }
        if self.bt_listen_port == Some(0) {
            return Err(DaemonError::Protocol(
                "bt_listen_port must be 1..=65535".into(),
            ));
        }
        let schedule_start = self
            .schedule_start
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let schedule_end = self
            .schedule_end
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if let Some(text) = schedule_start {
            if parse_hhmm(text).is_none() {
                return Err(DaemonError::Protocol(format!(
                    "schedule_start must be HH:MM, got {text:?}"
                )));
            }
        }
        if let Some(text) = schedule_end {
            if parse_hhmm(text).is_none() {
                return Err(DaemonError::Protocol(format!(
                    "schedule_end must be HH:MM, got {text:?}"
                )));
            }
        }
        if self.schedule_on == Some(true) && (schedule_start.is_none() || schedule_end.is_none()) {
            return Err(DaemonError::Protocol(
                "schedule_on needs schedule_start and schedule_end".into(),
            ));
        }
        // RPC over TCP: a listen address must parse, and anything beyond
        // loopback must carry a token — a wide-open unauthenticated RPC is
        // never what the user meant.
        if let Some(text) = self
            .rpc_listen
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            match text.parse::<std::net::SocketAddr>() {
                Ok(addr) => {
                    let has_secret = self
                        .rpc_secret
                        .as_deref()
                        .map(|secret| !secret.trim().is_empty())
                        .unwrap_or(false);
                    if !addr.ip().is_loopback() && !has_secret {
                        return Err(DaemonError::Protocol(
                            "rpc_listen on a non-loopback address requires rpc_secret".into(),
                        ));
                    }
                }
                Err(_) => {
                    return Err(DaemonError::Protocol(format!(
                        "rpc_listen must be ip:port, got {text:?}"
                    )));
                }
            }
        }
        Ok(())
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

    #[test]
    fn resolves_limits_from_the_file_and_the_boot_config() {
        let boot = Limits {
            max_concurrent: 3,
            max_speed_bps: None,
            retry_max: 5,
        };

        // Nothing stored: the boot values apply and are written back, so
        // `get_settings` reports what runs instead of `null`.
        let mut settings = Settings::default();
        assert_eq!(settings.resolve_limits(boot), boot);
        assert_eq!(settings.max_concurrent, Some(3));
        assert_eq!(settings.max_speed_bps, Some(0));
        assert_eq!(settings.retry_max, Some(5));
        assert_eq!(settings.connections, Some(CONNECTIONS_DEFAULT));
        // The daemon's fallback is the plugin's own default, so filling the
        // setting in changes nothing about how a task starts.
        assert_eq!(
            CONNECTIONS_DEFAULT,
            aether_plugin_sdk::Budget::default().connections
        );

        // A stored limit wins; a stored speed of 0 means "no limit".
        let mut settings = Settings {
            max_concurrent: Some(7),
            max_speed_bps: Some(0),
            connections: Some(8),
            retry_max: Some(0),
            ..Settings::default()
        };
        assert_eq!(
            settings.resolve_limits(boot),
            Limits {
                max_concurrent: 7,
                max_speed_bps: None,
                retry_max: 0,
            }
        );
        assert_eq!(settings.connections, Some(8));
    }

    #[test]
    fn repairs_limits_the_engine_cannot_honour_at_boot() {
        let boot = Limits {
            max_concurrent: 3,
            max_speed_bps: Some(1024),
            retry_max: 5,
        };
        let mut settings = Settings {
            max_concurrent: Some(0),
            connections: Some(CONNECTIONS_MAX + 1),
            ..Settings::default()
        };
        assert_eq!(settings.resolve_limits(boot).max_concurrent, 3);
        assert_eq!(
            settings.connections,
            Some(CONNECTIONS_DEFAULT),
            "an out-of-range connection count falls back to the plugin default"
        );
        // The repaired settings are usable…
        settings.validate().unwrap();

        // …while a patch that carries the bad value is rejected up front.
        let no_capacity = Settings {
            max_concurrent: Some(0),
            ..Settings::default()
        };
        assert!(no_capacity.validate().is_err());
        let too_many = Settings {
            connections: Some(CONNECTIONS_MAX + 1),
            ..Settings::default()
        };
        assert!(too_many.validate().is_err());
        assert!(Settings::default().validate().is_ok());
    }

    #[test]
    fn roundtrips_scheduler_limits() {
        let path = temp_path("limits");
        let settings = Settings {
            max_concurrent: Some(5),
            max_speed_bps: Some(2048 * 1024),
            connections: Some(CONNECTIONS_MAX),
            retry_max: Some(0),
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn every_serialized_field_is_a_known_key() {
        use aether_plugin_sdk::IpMode;

        // The RPC accepts exactly what the file holds, so a key added to the
        // struct without a `set_settings` branch cannot slip through.
        let settings = Settings {
            proxy: Some(ProxySetting::System),
            user_agent: Some("TestUA/9".into()),
            ip_mode: Some(IpMode::Ipv4Only),
            dns: Some("1.1.1.1".into()),
            max_concurrent: Some(3),
            max_speed_bps: Some(1),
            connections: Some(4),
            retry_max: Some(5),
            default_headers: Some("Referer: https://x.test".into()),
            ftp_passive: Some(false),
            bt_listen_port: Some(6881),
            bt_dht: Some(false),
            bt_upload_bps: Some(1024),
            bt_blocklist: Some("/tmp/blocklist.dat".into()),
            bt_trackers: Some("udp://tracker.test:80/announce".into()),
            schedule_on: Some(true),
            schedule_start: Some("01:00".into()),
            schedule_end: Some("07:00".into()),
            rpc_listen: Some("127.0.0.1:9090".into()),
            rpc_secret: Some("token".into()),
            ffmpeg_path: Some("/usr/bin/ffmpeg".into()),
        };
        let object = serde_json::to_value(settings).unwrap();
        let object = object.as_object().unwrap();
        assert_eq!(object.len(), KEYS.len());
        for key in object.keys() {
            assert!(KEYS.contains(&key.as_str()), "unknown key {key}");
        }
    }

    #[test]
    fn network_config_carries_headers_ftp_and_bt_knobs() {
        let settings = Settings {
            default_headers: Some("Referer: https://x.test/a; Origin: https://x.test".into()),
            ftp_passive: Some(false),
            bt_listen_port: Some(6881),
            bt_dht: Some(false),
            bt_upload_bps: Some(0),
            bt_blocklist: Some("  ".into()),
            bt_trackers: Some("udp://t1.test:80/announce\nudp://t2.test:80/announce\n".into()),
            ..Settings::default()
        };
        let network = settings.network().unwrap();
        assert_eq!(
            network.default_headers.as_ref().unwrap(),
            &[
                ("Referer".to_string(), "https://x.test/a".to_string()),
                ("Origin".to_string(), "https://x.test".to_string()),
            ][..]
        );
        assert_eq!(network.ftp_passive, Some(false));
        assert_eq!(network.bt_listen_port, Some(6881));
        assert_eq!(network.bt_dht, Some(false));
        // A stored 0 is the form of "no cap"; plugins get `None`.
        assert_eq!(network.bt_upload_bps, None);
        // Blank entries collapse to `None`, keeping "unset" semantics.
        assert_eq!(network.bt_blocklist, None);
        assert_eq!(
            network.bt_trackers.as_ref().unwrap(),
            &[
                "udp://t1.test:80/announce".to_string(),
                "udp://t2.test:80/announce".to_string()
            ][..]
        );

        // Hand-edited garbage degrades per field; the rest still applies.
        let settings = Settings {
            user_agent: Some("Keep/1".into()),
            default_headers: Some("Referer https://x.test".into()),
            ..Settings::default()
        };
        let network = settings.network().unwrap();
        assert_eq!(network.default_headers, None);
        assert_eq!(network.user_agent.as_deref(), Some("Keep/1"));

        // A port of 0 cannot be bound; validation rejects it up front.
        let bad_port = Settings {
            bt_listen_port: Some(0),
            ..Settings::default()
        };
        assert!(bad_port.validate().is_err());
        let bad_headers = Settings {
            default_headers: Some("Referer https://x.test".into()),
            ..Settings::default()
        };
        assert!(bad_headers.validate().is_err());
        assert!(Settings::default().validate().is_ok());
    }

    #[test]
    fn validates_the_schedule_window() {
        // HH:MM parsing accepts real clock times only.
        assert_eq!(super::parse_hhmm("01:00"), Some(60));
        assert_eq!(super::parse_hhmm(" 23:59 "), Some(23 * 60 + 59));
        assert_eq!(super::parse_hhmm("24:00"), None);
        assert_eq!(super::parse_hhmm("12:60"), None);
        assert_eq!(super::parse_hhmm("7:5"), Some(7 * 60 + 5));
        assert_eq!(super::parse_hhmm("midnight"), None);

        // Enabling the schedule without both times cannot work.
        let incomplete = Settings {
            schedule_on: Some(true),
            schedule_start: Some("01:00".into()),
            ..Settings::default()
        };
        assert!(incomplete.validate().is_err());

        // Storing times without enabling the window is fine (draft state).
        let draft = Settings {
            schedule_start: Some("01:00".into()),
            schedule_end: Some("07:00".into()),
            ..Settings::default()
        };
        draft.validate().unwrap();

        let bad_time = Settings {
            schedule_on: Some(true),
            schedule_start: Some("25:00".into()),
            schedule_end: Some("07:00".into()),
            ..Settings::default()
        };
        assert!(bad_time.validate().is_err());

        let full = Settings {
            schedule_on: Some(true),
            schedule_start: Some("01:00".into()),
            schedule_end: Some("07:00".into()),
            ..Settings::default()
        };
        full.validate().unwrap();
    }

    #[test]
    fn validates_the_rpc_listen_address() {
        // Off by default.
        Settings::default().validate().unwrap();

        // Loopback needs no token; a typo is still an error.
        let loopback = Settings {
            rpc_listen: Some("127.0.0.1:9090".into()),
            ..Settings::default()
        };
        loopback.validate().unwrap();
        let typo = Settings {
            rpc_listen: Some("localhost:9090".into()),
            ..Settings::default()
        };
        assert!(typo.validate().is_err());

        // Anything wider than loopback must carry a token.
        let wide = Settings {
            rpc_listen: Some("0.0.0.0:9090".into()),
            ..Settings::default()
        };
        assert!(wide.validate().is_err());
        let wide_with_token = Settings {
            rpc_listen: Some("0.0.0.0:9090".into()),
            rpc_secret: Some("s3cret".into()),
            ..Settings::default()
        };
        wide_with_token.validate().unwrap();
    }
}
