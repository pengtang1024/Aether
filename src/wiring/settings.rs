//! Settings search and the `settings-changed` dispatcher.

use std::cell::RefCell;
use std::net::IpAddr;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::{ModelRc, VecModel};

use crate::i18n;
use crate::plugin_settings::parse_plugin_pins;
use crate::prefs::save_theme_mode;
use crate::settings_search;
use crate::state::State;
use crate::status_text::speed_limit_bps;
use crate::toast::show_toast;
use crate::{AppWindow, SettingsFilter, SettingsHit, SettingsValidation};

mod limits;
mod network;
mod plugins;
mod prefs;

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // ---- settings search -------------------------------------------
    // Rust owns the card vocabulary (Slint cannot test substrings); the
    // page only renders the hits and filters its cards by id.
    {
        let ui_handle = ui.as_weak();
        ui.on_settings_search_changed(move |query| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let results = settings_search::search(query.as_str());
            let rows: Vec<SettingsHit> = results
                .hits
                .iter()
                .map(|hit| SettingsHit {
                    tab: hit.tab,
                    id: hit.id.into(),
                    title_before: hit.title_before.clone().into(),
                    title_hit: hit.title_hit.clone().into(),
                    title_after: hit.title_after.clone().into(),
                    subtitle_before: hit.subtitle_before.clone().into(),
                    subtitle_hit: hit.subtitle_hit.clone().into(),
                    subtitle_after: hit.subtitle_after.clone().into(),
                    heading: hit.heading.clone().into(),
                })
                .collect();
            let filter = ui.global::<SettingsFilter>();
            filter.set_active(!query.is_empty());
            filter.set_hits(results.cards.into());
            filter.set_row_hits(results.rows.into());
            filter.set_results(ModelRc::new(VecModel::from(rows)));
        });
    }

    // ---- speed limit pills (global throttle via set_settings) --------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_speed_limit_picked(move |mode| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let bps = speed_limit_bps(mode);
            state
                .borrow()
                .backend
                .set_setting(serde_json::json!({"max_speed_bps": bps}));
            let name = match mode {
                1 => "1 MB/s".to_string(),
                2 => "5 MB/s".to_string(),
                3 => "20 MB/s".to_string(),
                _ => i18n::tr("不限", "Unlimited"),
            };
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    format!("全局限速：{name}"),
                    format!("Global speed limit: {name}"),
                ),
            );
        });
    }

    let ui_handle = ui.as_weak();
    let state = state.clone();
    ui.on_settings_changed(move |key, value| {
        // Reject malformed free-form entries before they reach the daemon and
        // surface the reason next to the field. The daemon keeps its own
        // validation; this only covers the formats the UI can explain.
        if let Some(message) = validate_text(key.as_str(), value.as_str()) {
            if let Some(ui) = ui_handle.upgrade() {
                let validation = ui.global::<SettingsValidation>();
                validation.set_key(key.clone());
                validation.set_message(message.into());
            }
            return;
        }
        if let Some(ui) = ui_handle.upgrade() {
            let validation = ui.global::<SettingsValidation>();
            if validation.get_key() == key {
                validation.set_message(Default::default());
            }
        }
        if key.as_str() == "theme" {
            let mode = match value.as_str() {
                "浅色" => 1,
                "深色" => 2,
                _ => 0,
            };
            save_theme_mode(mode);
            // A real preset was chosen, so any custom row in the theme combo
            // has served its purpose: rebuild the model without it.
            if let Some(ui) = ui_handle.upgrade() {
                crate::settings_presets::mirror_theme(&ui, mode);
            }
        }
        let hint = {
            let mut st = state.borrow_mut();
            let ui = ui_handle.upgrade();
            dispatch(&mut st, ui.as_ref(), key.as_str(), value.as_str())
        };
        if let (Some(hint), Some(ui)) = (hint, ui_handle.upgrade()) {
            show_toast(&ui, &state, &hint);
        }
        println!("[settings] {key} = {value}");
    });
}

fn dispatch(st: &mut State, ui: Option<&AppWindow>, key: &str, value: &str) -> Option<String> {
    match key {
        "proxy-url" | "proxy-mode" | "dns" | "user-agent" | "ip-mode" | "headers"
        | "ffmpeg-path" | "ftp-passive" | "bt-port" | "bt-dht" | "bt-upload-limit"
        | "bt-blocklist" | "bt-trackers" => network::apply(st, ui, key, value),
        "plugin-auto-update"
        | "plugin-external-process"
        | "plugin-pins"
        | "app-auto-update"
        | "plugin-install" => plugins::apply(st, ui, key, value),
        "schedule-on" | "schedule-start" | "schedule-end" | "max-tasks" | "max-connections"
        | "retry-count" | "global-limit" | "download-limit" => limits::apply(st, ui, key, value),
        "language" | "accent" | "motion" | "flow-background" | "start-page" | "after-done"
        | "notify-done" | "notify-fail" | "clipboard-watch" | "autostart" | "reset-settings" => {
            prefs::apply(st, ui, key, value)
        }
        "theme" => None,
        other => Some(i18n::tr(
            format!("设置项「{other}」尚未接线"),
            format!("Setting {other} is not wired up yet"),
        )),
    }
}

/// Format check for the free-form fields the UI keeps as text. `None` means
/// "send it to the daemon"; empty input generally means "use the default".
fn validate_text(key: &str, value: &str) -> Option<String> {
    let text = value.trim();
    match key {
        "proxy-url" => (!text.is_empty() && !proxy_url_ok(text)).then(|| {
            i18n::tr(
                "代理地址需以 http://、https://、socks5:// 或 socks5h:// 开头",
                "Proxy URL must start with http://, https://, socks5:// or socks5h://",
            )
        }),
        "dns" => (!text.is_empty() && !dns_ok(text)).then(|| {
            i18n::tr(
                "DNS 需为 IP 或 [IPv6]:端口，多项用逗号分隔",
                "DNS entries must be an IP or [IPv6]:port, separated by commas",
            )
        }),
        "headers" => (!text.is_empty() && !headers_ok(text)).then(|| {
            i18n::tr(
                "请求头需为 Key: Value，多条用分号分隔",
                "Headers must be Key: Value, entries separated by semicolons",
            )
        }),
        "bt-port" => (!port_ok(text)).then(|| {
            i18n::tr(
                "监听端口需为 1–65535 的整数",
                "The listening port must be an integer in 1–65535",
            )
        }),
        "schedule-start" | "schedule-end" => {
            (!text.is_empty() && aether_daemon::settings::parse_hhmm(text).is_none()).then(|| {
                i18n::tr(
                    "时间需为 HH:MM（00:00–23:59）",
                    "Time must be HH:MM (00:00–23:59)",
                )
            })
        }
        "plugin-pins" => parse_plugin_pins(text).err(),
        "plugin-install" => text.is_empty().then(|| {
            i18n::tr(
                "请输入插件包目录路径（含 plugin.json）",
                "Enter the plugin package directory (containing plugin.json)",
            )
        }),
        _ => None,
    }
}

fn proxy_url_ok(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["http://", "https://", "socks5://", "socks5h://"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

fn dns_ok(text: &str) -> bool {
    text.split([',', ' ', '\t', '\n', ';'])
        .filter(|entry| !entry.trim().is_empty())
        .all(|entry| dns_entry_ok(entry.trim()))
}

fn dns_entry_ok(entry: &str) -> bool {
    if entry.parse::<IpAddr>().is_ok() {
        return true;
    }
    let candidate = entry.strip_prefix('[');
    if let Some(rest) = candidate {
        return rest
            .split_once("]:")
            .map(|(addr, port)| addr.parse::<IpAddr>().is_ok() && port.parse::<u16>().is_ok())
            .unwrap_or(false);
    }
    match entry.rsplit_once(':') {
        Some((addr, port)) => addr.parse::<IpAddr>().is_ok() && port.parse::<u16>().is_ok(),
        None => false,
    }
}

fn headers_ok(text: &str) -> bool {
    text.split([';', '\n'])
        .filter(|entry| !entry.trim().is_empty())
        .all(|entry| {
            entry
                .split_once(':')
                .map(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
                .unwrap_or(false)
        })
}

fn port_ok(text: &str) -> bool {
    text.parse::<u32>()
        .map(|port| (1..=65535).contains(&port))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_accepts_ipv4_ipv6_and_ports() {
        assert!(dns_ok("1.1.1.1, 8.8.8.8"));
        assert!(dns_ok("[::1]:5353"));
        assert!(!dns_ok("dns.example"));
        assert!(!dns_ok("1.1.1.1:"));
    }

    #[test]
    fn headers_require_a_pair() {
        assert!(headers_ok("Referer: https://example.com; X-Token: abc"));
        assert!(!headers_ok("NoColon"));
        assert!(!headers_ok("Key: "));
    }

    #[test]
    fn proxy_urls_are_scheme_checked() {
        assert!(proxy_url_ok("http://127.0.0.1:7890"));
        assert!(proxy_url_ok("SOCKS5://127.0.0.1:1080"));
        assert!(!proxy_url_ok("127.0.0.1:7890"));
    }

    #[test]
    fn ports_reject_zero_and_out_of_range() {
        assert!(port_ok("51413"));
        assert!(!port_ok("0"));
        assert!(!port_ok("70000"));
        assert!(!port_ok("51413x"));
    }

    #[test]
    fn empty_values_are_allowed_except_install_path() {
        assert_eq!(validate_text("dns", ""), None);
        assert_eq!(validate_text("schedule-start", ""), None);
        assert!(validate_text("plugin-install", "").is_some());
    }

    #[test]
    fn schedule_rejects_bad_clock() {
        assert_eq!(validate_text("schedule-start", "23:59"), None);
        assert!(validate_text("schedule-end", "24:00").is_some());
    }
}
