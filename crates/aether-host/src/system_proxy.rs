//! Desktop proxy discovery for `mode: "system"`.
//!
//! Plugins build their own HTTP clients from environment variables, but desktop
//! users set the proxy in GNOME Settings or KDE, not in `~/.bashrc`. Without
//! this the daemon would silently connect directly while the browser uses a
//! proxy.
//!
//! Rules:
//! - Proxy environment variables win. If any of `HTTP_PROXY`, `HTTPS_PROXY` or
//!   `ALL_PROXY` (any case) is set we return `None` and let reqwest honor them
//!   (including `NO_PROXY`), instead of overriding with desktop settings.
//! - GNOME (`gsettings`) then KDE (`kioslaverc`) are consulted; first hit wins.
//! - "Automatic" mode with a configured script URL yields [`SystemProxy::Pac`];
//!   plain WPAD autodiscovery (empty URL) is deliberately not attempted.
//!
//! Detection shells out; the daemon caches the result for a minute.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use aether_plugin_sdk::ProxyConfig;

/// How long a detection result is trusted before probing the desktop again.
pub const SYSTEM_PROXY_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemProxy {
    /// One proxy for every request.
    Manual(ProxyConfig),
    /// The desktop points at a PAC script.
    Pac(String),
}

/// Detect the desktop proxy configuration, if any.
pub fn detect() -> Option<SystemProxy> {
    if env_proxy_present() {
        return None;
    }
    let gsettings = std::env::var_os("AETHER_GSETTINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gsettings"));
    if let Some(proxy) = detect_gnome(&gsettings) {
        return Some(proxy);
    }
    if let Some(path) = kde_config_path() {
        if let Some(proxy) = detect_kde(&path) {
            return Some(proxy);
        }
    }
    None
}

fn env_proxy_present() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn kde_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AETHER_KDE_PROXY_CONFIG") {
        return Some(PathBuf::from(path));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(config).join("kioslaverc"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/kioslaverc"))
}

/// GNOME stores strings as GVariant literals: `'manual'`, `'proxy.local'`.
fn gvariant_string(value: &str) -> String {
    value.trim().trim_matches('\'').to_string()
}

fn gsettings_get(bin: &Path, key: &str) -> Option<String> {
    let output = Command::new(bin).args(["get", key]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn detect_gnome(bin: &Path) -> Option<SystemProxy> {
    let mode = gvariant_string(&gsettings_get(bin, "org.gnome.system.proxy mode")?);
    match mode.as_str() {
        "manual" => {
            for (schema, scheme, default_port) in [
                ("org.gnome.system.proxy.http", "http", 80_u16),
                ("org.gnome.system.proxy.https", "https", 443),
                ("org.gnome.system.proxy.socks", "socks5", 1080),
            ] {
                let host = gsettings_get(bin, &format!("{schema} host"))
                    .map(|value| gvariant_string(&value))
                    .unwrap_or_default();
                if host.is_empty() {
                    continue;
                }
                let port = gsettings_get(bin, &format!("{schema} port"))
                    .and_then(|value| gvariant_string(&value).parse::<u16>().ok())
                    .filter(|port| *port > 0)
                    .unwrap_or(default_port);
                return Some(SystemProxy::Manual(ProxyConfig {
                    mode: aether_plugin_sdk::ProxyMode::Custom,
                    url: Some(format!("{scheme}://{host}:{port}")),
                    username: None,
                    password: None,
                }));
            }
            None
        }
        "auto" => {
            let url = gsettings_get(bin, "org.gnome.system.proxy autoconfig-url")
                .map(|value| gvariant_string(&value))
                .unwrap_or_default();
            // An empty autoconfig-url means "use WPAD", which we do not attempt.
            (!url.is_empty()).then_some(SystemProxy::Pac(url))
        }
        _ => None,
    }
}

/// KDE's `kioslaverc`: `ProxyType` 0 = none, 1 = manual, 2 = PAC script.
fn detect_kde(path: &Path) -> Option<SystemProxy> {
    let text = fs::read_to_string(path).ok()?;
    let mut proxy_type = None;
    let mut http = None;
    let mut https = None;
    let mut socks = None;
    let mut script = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "ProxyType" => proxy_type = Some(value.to_string()),
            "httpProxy" => http = Some(value.to_string()),
            "httpsProxy" => https = Some(value.to_string()),
            "socksProxy" => socks = Some(value.to_string()),
            "Proxy Config Script" => script = Some(value.to_string()),
            _ => {}
        }
    }
    match proxy_type.as_deref() {
        Some("1") => [
            (http, "http", 80_u16),
            (https, "https", 443),
            (socks, "socks5", 1080),
        ]
        .into_iter()
        .find_map(|(value, scheme, default_port)| parse_kde_proxy(value?, scheme, default_port))
        .map(|url| {
            SystemProxy::Manual(ProxyConfig {
                mode: aether_plugin_sdk::ProxyMode::Custom,
                url: Some(url),
                username: None,
                password: None,
            })
        }),
        Some("2") => script
            .filter(|value| !value.trim().is_empty())
            .map(SystemProxy::Pac),
        _ => None,
    }
}

/// KDE writes either `http://host:3128` or `http://host 3128`.
fn parse_kde_proxy(value: String, scheme: &str, default_port: u16) -> Option<String> {
    let mut parts = value.split_whitespace();
    let address = parts.next()?.trim();
    if address.is_empty() {
        return None;
    }
    let explicit_port = parts.next().and_then(|port| port.parse::<u16>().ok());
    let without_scheme = address
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(address);
    let (host, embedded_port) = match without_scheme.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host, Some(port)),
            Err(_) => (without_scheme, None),
        },
        None => (without_scheme, None),
    };
    if host.is_empty() {
        return None;
    }
    Some(format!(
        "{scheme}://{host}:{}",
        explicit_port.or(embedded_port).unwrap_or(default_port)
    ))
}

// The tests fabricate `gsettings` shell scripts and need Unix permissions.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn stub_gsettings(dir: &Path, mode: &str, values: &[(&str, &str)]) -> PathBuf {
        // A tiny shell script that answers `get <key>` from a case table.
        let mut script = String::from("#!/bin/sh\ncase \"$2\" in\n");
        script.push_str(&format!(
            "  \"org.gnome.system.proxy mode\") echo \"'{mode}'\" ;;\n"
        ));
        for (key, value) in values {
            script.push_str(&format!("  \"{key}\") echo \"{value}\" ;;\n"));
        }
        script.push_str("  *) exit 1 ;;\nesac\n");
        let path = dir.join("gsettings");
        fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aether-system-proxy-{label}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn gnome_manual_http_proxy() {
        let dir = temp_dir("gnome-http");
        let stub = stub_gsettings(
            &dir,
            "manual",
            &[
                ("org.gnome.system.proxy.http host", "'proxy.corp'"),
                ("org.gnome.system.proxy.http port", "3128"),
            ],
        );
        let proxy = detect_gnome(&stub).unwrap();
        match proxy {
            SystemProxy::Manual(config) => {
                assert_eq!(config.url.as_deref(), Some("http://proxy.corp:3128"))
            }
            other => panic!("expected manual, got {other:?}"),
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn gnome_falls_through_to_socks_and_auto_pac() {
        let dir = temp_dir("gnome-socks");
        let socks = stub_gsettings(
            &dir,
            "manual",
            &[
                ("org.gnome.system.proxy.http host", "''"),
                ("org.gnome.system.proxy.socks host", "'10.0.0.5'"),
                ("org.gnome.system.proxy.socks port", "1080"),
            ],
        );
        match detect_gnome(&socks).unwrap() {
            SystemProxy::Manual(config) => {
                assert_eq!(config.url.as_deref(), Some("socks5://10.0.0.5:1080"))
            }
            other => panic!("expected manual, got {other:?}"),
        }

        let pac = stub_gsettings(
            &dir,
            "auto",
            &[(
                "org.gnome.system.proxy autoconfig-url",
                "'http://wpad.corp/proxy.pac'",
            )],
        );
        assert_eq!(
            detect_gnome(&pac),
            Some(SystemProxy::Pac("http://wpad.corp/proxy.pac".into()))
        );

        // GNOME's "auto" with an empty URL means WPAD: not attempted.
        let wpad = stub_gsettings(
            &dir,
            "auto",
            &[("org.gnome.system.proxy autoconfig-url", "''")],
        );
        assert_eq!(detect_gnome(&wpad), None);

        let none = stub_gsettings(&dir, "none", &[]);
        assert_eq!(detect_gnome(&none), None);

        // A missing binary is not an error.
        assert_eq!(detect_gnome(&dir.join("missing")), None);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn kde_manual_and_script_modes() {
        let dir = temp_dir("kde");
        let manual = dir.join("kioslaverc");
        fs::write(
            &manual,
            "[Proxy Settings]\nProxyType=1\nhttpProxy=http://kde-proxy:8080\nhttpsProxy=\nsocksProxy=\n",
        )
        .unwrap();
        match detect_kde(&manual).unwrap() {
            SystemProxy::Manual(config) => {
                assert_eq!(config.url.as_deref(), Some("http://kde-proxy:8080"))
            }
            other => panic!("expected manual, got {other:?}"),
        }

        // KDE sometimes writes "http://host 3128" (space before the port).
        fs::write(
            &manual,
            "[Proxy Settings]\nProxyType=1\nhttpProxy=http://kde-proxy 3128\n",
        )
        .unwrap();
        match detect_kde(&manual).unwrap() {
            SystemProxy::Manual(config) => {
                assert_eq!(config.url.as_deref(), Some("http://kde-proxy:3128"))
            }
            other => panic!("expected manual, got {other:?}"),
        }

        fs::write(
            &manual,
            "[Proxy Settings]\nProxyType=2\nProxy Config Script=http://wpad.corp/proxy.pac\n",
        )
        .unwrap();
        assert_eq!(
            detect_kde(&manual),
            Some(SystemProxy::Pac("http://wpad.corp/proxy.pac".into()))
        );

        // ProxyType=0 (none) and missing files are ignored.
        fs::write(&manual, "[Proxy Settings]\nProxyType=0\n").unwrap();
        assert_eq!(detect_kde(&manual), None);
        assert_eq!(detect_kde(&dir.join("missing")), None);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn kde_proxy_forms() {
        assert_eq!(
            parse_kde_proxy("http://p:3128".into(), "http", 80),
            Some("http://p:3128".into())
        );
        assert_eq!(
            parse_kde_proxy("p 3128".into(), "http", 80),
            Some("http://p:3128".into())
        );
        assert_eq!(
            parse_kde_proxy("p".into(), "socks5", 1080),
            Some("socks5://p:1080".into())
        );
        assert_eq!(parse_kde_proxy("   ".into(), "http", 80), None);
    }
}
