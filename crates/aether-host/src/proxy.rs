//! Host-level proxy setting: static modes plus PAC.
//!
//! This is the shape stored in `.aether/settings.json` and returned by the
//! daemon's settings API. It stays wire-compatible with the older
//! `{"mode":"system"|"none"|"custom","url":…}` objects that plugins know.

use aether_plugin_sdk::{ProxyConfig, ProxyMode};
use serde::{Deserialize, Serialize};

use crate::pac::PacResolver;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ProxySetting {
    /// Honor the platform environment (`HTTP_PROXY`, `ALL_PROXY`, …).
    System,
    /// Never use a proxy, even when the environment sets one.
    #[serde(rename = "none")]
    Direct,
    /// One explicit proxy for every request.
    Custom {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    /// Evaluate `FindProxyForURL` from a script URL or inline source.
    Pac {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script: Option<String>,
    },
}

impl ProxySetting {
    /// Parse the `AETHER_PROXY` one-liner: `system`, `none`, or a proxy URL.
    pub fn parse_spec(spec: &str) -> Result<Self, String> {
        let config = ProxyConfig::parse(spec)?;
        Ok(match config.mode {
            ProxyMode::System => Self::System,
            ProxyMode::Direct => Self::Direct,
            ProxyMode::Custom => Self::Custom {
                url: config.url.unwrap_or_default(),
                username: config.username,
                password: config.password,
            },
        })
    }

    /// PAC without a source, or a custom proxy without a URL, is a user error
    /// that should surface while they are still on the settings screen.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::System | Self::Direct => Ok(()),
            Self::Custom {
                url,
                username,
                password,
            } => ProxyConfig {
                mode: ProxyMode::Custom,
                url: Some(url.clone()),
                username: username.clone(),
                password: password.clone(),
            }
            .validate(),
            Self::Pac { url, script } => {
                let has_url = url.as_deref().is_some_and(|value| !value.trim().is_empty());
                let has_script = script
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                match (has_url, has_script) {
                    (true, true) => {
                        Err("a PAC setting takes either url or script, not both".into())
                    }
                    (false, false) => Err("PAC needs a script url or an inline script".into()),
                    _ => Ok(()),
                }
            }
        }
    }

    /// The process-level setting handed to plugins on `Init`. PAC is per task,
    /// so the process default becomes "honor the environment": a task whose PAC
    /// lookup failed should behave like the system default, not go direct.
    pub fn init_config(&self) -> Option<ProxyConfig> {
        match self {
            Self::System => Some(ProxyConfig::system()),
            Self::Direct => Some(ProxyConfig::direct()),
            Self::Custom {
                url,
                username,
                password,
            } => Some(ProxyConfig {
                mode: ProxyMode::Custom,
                url: Some(url.clone()),
                username: username.clone(),
                password: password.clone(),
            }),
            Self::Pac { .. } => Some(ProxyConfig::system()),
        }
    }

    pub fn is_pac(&self) -> bool {
        matches!(self, Self::Pac { .. })
    }

    pub fn resolver(&self) -> Option<PacResolver> {
        match self {
            Self::Pac { url, script } => Some(PacResolver::new(url.clone(), script.clone())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_legacy_settings_shape() {
        let system: ProxySetting = serde_json::from_str(r#"{"mode":"system"}"#).unwrap();
        assert_eq!(system, ProxySetting::System);

        let direct: ProxySetting = serde_json::from_str(r#"{"mode":"none"}"#).unwrap();
        assert_eq!(direct, ProxySetting::Direct);

        let custom: ProxySetting = serde_json::from_str(
            r#"{"mode":"custom","url":"http://10.0.0.1:3128","username":"u","password":"p"}"#,
        )
        .unwrap();
        assert_eq!(
            custom,
            ProxySetting::Custom {
                url: "http://10.0.0.1:3128".into(),
                username: Some("u".into()),
                password: Some("p".into()),
            }
        );
        assert_eq!(
            serde_json::to_string(&custom).unwrap(),
            r#"{"mode":"custom","url":"http://10.0.0.1:3128","username":"u","password":"p"}"#
        );
    }

    #[test]
    fn parses_command_line_specs() {
        assert_eq!(
            ProxySetting::parse_spec("system").unwrap(),
            ProxySetting::System
        );
        assert_eq!(
            ProxySetting::parse_spec("none").unwrap(),
            ProxySetting::Direct
        );
        assert_eq!(
            ProxySetting::parse_spec("socks5://127.0.0.1:1080").unwrap(),
            ProxySetting::Custom {
                url: "socks5://127.0.0.1:1080".into(),
                username: None,
                password: None,
            }
        );
        assert!(ProxySetting::parse_spec("nope").is_err());
    }

    #[test]
    fn validates_pac_sources() {
        let ok = ProxySetting::Pac {
            url: Some("http://wpad/wpad.dat".into()),
            script: None,
        };
        assert!(ok.validate().is_ok());
        assert!(ok.resolver().is_some());

        let both = ProxySetting::Pac {
            url: Some("http://wpad/wpad.dat".into()),
            script: Some("function FindProxyForURL(){}".into()),
        };
        assert!(both.validate().is_err());

        let empty = ProxySetting::Pac {
            url: None,
            script: Some("   ".into()),
        };
        assert!(empty.validate().is_err());

        let bad_custom = ProxySetting::Custom {
            url: "10.0.0.1:3128".into(),
            username: None,
            password: None,
        };
        assert!(bad_custom.validate().is_err());
    }

    #[test]
    fn pac_falls_back_to_the_environment_for_plugins() {
        let pac = ProxySetting::Pac {
            script: Some("function FindProxyForURL(){return 'DIRECT';}".into()),
            url: None,
        };
        assert_eq!(pac.init_config(), Some(ProxyConfig::system()));
        assert_eq!(
            ProxySetting::Direct.init_config(),
            Some(ProxyConfig::direct())
        );
    }
}
