//! Progress fractions and captions for app/plugin update passes.

use crate::i18n;

/// One line describing what the plugin pass is doing right now.
pub(crate) fn plugin_progress_text(progress: Option<&serde_json::Value>) -> String {
    let Some(progress) = progress else {
        return String::new();
    };
    if progress["running"].as_bool() != Some(true) {
        return progress["error"]
            .as_str()
            .map_or_else(String::new, str::to_string);
    }
    let done = progress["done"].as_u64().unwrap_or(0);
    let total = progress["total"].as_u64().unwrap_or(0);
    match progress["phase"].as_str().unwrap_or("") {
        "downloading" => {
            let plugin = progress["plugin"].as_str().unwrap_or("");
            i18n::tr(
                format!("正在更新 {plugin} · {done}/{total}"),
                format!("Updating {plugin} · {done}/{total}"),
            )
        }
        _ => i18n::tr("正在检查插件更新…", "Checking for plugin updates…"),
    }
}

/// Fraction (0..1) of a running plugin update/install pass, 0 when idle.
/// `checking` and unknown sizes report 0 — the bar then shows as indeterminate.
pub(crate) fn plugin_progress_fraction(progress: Option<&serde_json::Value>) -> f32 {
    let Some(progress) = progress else { return 0.0 };
    if progress["running"].as_bool() != Some(true) {
        return 0.0;
    }
    let total = progress["total"].as_u64().unwrap_or(0) as f32;
    let done = progress["done"].as_u64().unwrap_or(0) as f32;
    let mut fraction = if total > 0.0 { done / total } else { 0.0 };
    // A plugin currently downloading adds its own slice of one plugin's share.
    if progress["phase"].as_str() == Some("downloading") && total > 0.0 {
        let size = progress["size"].as_u64().unwrap_or(0) as f32;
        let downloaded = progress["downloaded"].as_u64().unwrap_or(0) as f32;
        if size > 0.0 {
            fraction += (downloaded / size).clamp(0.0, 1.0) / total;
        }
    }
    fraction.clamp(0.0, 1.0)
}

/// Download fraction (0..1) of an in-flight update, 0 for every other state.
pub(crate) fn update_progress_fraction(update: Option<&serde_json::Value>) -> f32 {
    let Some(update) = update else { return 0.0 };
    if update["status"].as_str() != Some("downloading") {
        return 0.0;
    }
    let downloaded = update["downloaded"].as_u64().unwrap_or(0) as f32;
    match update["total"].as_u64() {
        Some(total) if total > 0 => (downloaded / total as f32).clamp(0.0, 1.0),
        // Unknown size: keep the bar visible but indeterminate-ish.
        _ => 0.0,
    }
}

/// Human-readable app-update state for the About panel. `None` means the
/// daemon has no update source configured at all.
pub(crate) fn update_status_text(update: Option<&serde_json::Value>) -> String {
    let Some(update) = update else {
        return i18n::tr(
            "未配置更新源（daemon 需要 --update-url 与更新公钥）",
            "No update source (the daemon needs --update-url and the public key)",
        );
    };
    let latest = update["latest"].as_str().unwrap_or_default();
    let error = update["error"].as_str().unwrap_or_default();
    match update["status"].as_str().unwrap_or_default() {
        "checking" => i18n::tr("正在检查更新…", "Checking for updates…"),
        "downloading" => {
            let progress = update_progress_fraction(Some(update));
            i18n::tr(
                format!("正在下载新版本 {latest}… {:.0}%", progress * 100.0),
                format!("Downloading {latest}… {:.0}%", progress * 100.0),
            )
        }
        "idle" => i18n::tr(
            "等待首次后台检查（每 6 小时一次）",
            "Waiting for the first background check (every 6 hours)",
        ),
        "up_to_date" => i18n::tr(
            format!("已是最新版本 {}", env!("CARGO_PKG_VERSION")),
            format!("Up to date ({})", env!("CARGO_PKG_VERSION")),
        ),
        "available" => i18n::tr(
            format!("发现新版本 {latest}，自动下载已关闭"),
            format!("Version {latest} is available; automatic downloads are off"),
        ),
        "ready" => i18n::tr(
            format!("新版本 {latest} 已下载，重启后生效"),
            format!("Version {latest} downloaded; restart to apply"),
        ),
        // The updater cannot install this one itself; hand the user the file.
        "package" => {
            let artifact = update["artifact"].as_str().unwrap_or_default();
            match update["kind"].as_str().unwrap_or_default() {
                "deb" => i18n::tr(
                    format!("新版本 {latest} 安装包已下载：{artifact}，运行 sudo dpkg -i 安装"),
                    format!("Version {latest} package downloaded: {artifact}; install it with sudo dpkg -i"),
                ),
                _ => i18n::tr(
                    format!("新版本 {latest} 安装包已下载：{artifact}"),
                    format!("Version {latest} package downloaded: {artifact}"),
                ),
            }
        }
        "error" => i18n::tr(
            format!("检查失败：{error}"),
            format!("Check failed: {error}"),
        ),
        _ => i18n::tr("更新状态未知", "Unknown update state"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::status_text::rpc_status_text;

    use super::{
        plugin_progress_fraction, plugin_progress_text, update_progress_fraction,
        update_status_text,
    };

    /// The process language is global and other test threads flip it, so a
    /// line is accepted in either language instead of setting the flag here
    /// (ja/zh-TW fall back to the Chinese string for Rust-side strings).
    fn assert_line(update: Option<Value>, zh: &str, en: &str) {
        let text = update_status_text(update.as_ref());
        assert!(
            text == zh || text == en,
            "expected {zh:?} or {en:?}, got {text:?}"
        );
    }

    /// The plugin bar's caption names the plugin being downloaded and the
    /// progress through the pass; failures survive into the idle state.
    #[test]
    fn plugin_progress_text_describes_the_phase() {
        let downloading = json!({
            "running": true, "phase": "downloading", "plugin": "http",
            "done": 1, "total": 3
        });
        let text = plugin_progress_text(Some(&downloading));
        assert!(text.contains("http") && text.contains("1/3"), "got {text}");
        let checking = json!({"running": true, "phase": "checking"});
        let text = plugin_progress_text(Some(&checking));
        assert!(
            text.contains("检查") || text.contains("Checking"),
            "got {text}"
        );
        assert_eq!(plugin_progress_text(Some(&json!({"running": false}))), "");
        assert_eq!(
            plugin_progress_text(Some(&json!({"running": false, "error": "boom"}))),
            "boom"
        );
    }

    /// Plugin passes report per-plugin granularity; the bar must move while a
    /// single plugin is still downloading.
    #[test]
    fn plugin_progress_counts_finished_and_active_plugins() {
        assert_eq!(plugin_progress_fraction(None), 0.0);
        assert_eq!(
            plugin_progress_fraction(Some(&json!({"running": false}))),
            0.0
        );
        assert_eq!(
            plugin_progress_fraction(Some(&json!({"running": true, "phase": "checking"}))),
            0.0,
            "checking has no total yet"
        );
        let half = plugin_progress_fraction(Some(&json!({
            "running": true, "phase": "downloading", "done": 1, "total": 2,
            "downloaded": 50, "size": 100
        })));
        assert!(
            (half - 0.75).abs() < 0.001,
            "1 done + half of the second = 0.75, got {half}"
        );
        assert_eq!(
            plugin_progress_fraction(Some(&json!({
                "running": true, "phase": "downloading", "done": 1, "total": 2,
                "downloaded": 10, "size": null
            }))),
            0.5,
            "unknown size falls back to finished plugins only"
        );
    }

    /// A download reports its fraction so the About panel can draw the bar.
    #[test]
    fn download_progress_is_the_fraction_of_the_expected_size() {
        assert_eq!(
            update_progress_fraction(Some(&json!({
                "status": "downloading", "downloaded": 25, "total": 100
            }))),
            0.25
        );
        assert_eq!(
            update_progress_fraction(Some(&json!({"status": "downloading", "downloaded": 5}))),
            0.0,
            "unknown size keeps the bar empty instead of inventing one"
        );
        assert_eq!(
            update_progress_fraction(Some(
                &json!({"status": "ready", "downloaded": 1, "total": 2})
            )),
            0.0,
            "only the downloading state shows a bar"
        );
        assert_eq!(update_progress_fraction(None), 0.0);
    }

    /// The About panel renders exactly one line per daemon state; a missing
    /// source must read as "not configured", never as "up to date".
    #[test]
    fn every_update_state_has_a_line() {
        assert_line(
            None,
            "未配置更新源（daemon 需要 --update-url 与更新公钥）",
            "No update source (the daemon needs --update-url and the public key)",
        );
        assert_line(
            Some(json!({"status": "checking"})),
            "正在检查更新…",
            "Checking for updates…",
        );
        assert_line(
            Some(json!({"status": "idle"})),
            "等待首次后台检查（每 6 小时一次）",
            "Waiting for the first background check (every 6 hours)",
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_line(
            Some(json!({"status": "up_to_date"})),
            &format!("已是最新版本 {version}"),
            &format!("Up to date ({version})"),
        );
        assert_line(
            Some(json!({"status": "available", "latest": "0.3.0"})),
            "发现新版本 0.3.0，自动下载已关闭",
            "Version 0.3.0 is available; automatic downloads are off",
        );
        assert_line(
            Some(json!({"status": "ready", "latest": "0.3.0"})),
            "新版本 0.3.0 已下载，重启后生效",
            "Version 0.3.0 downloaded; restart to apply",
        );
        assert_line(
            Some(json!({"status": "error", "error": "timeout"})),
            "检查失败：timeout",
            "Check failed: timeout",
        );
        assert_line(
            Some(json!({"status": "what"})),
            "更新状态未知",
            "Unknown update state",
        );
    }

    #[test]
    fn rpc_status_names_the_configured_address() {
        let off = rpc_status_text("", false);
        assert!(
            off == "远程控制未开启" || off == "Remote control is off",
            "unexpected: {off:?}"
        );
        let with_token = rpc_status_text("127.0.0.1:7777", true);
        assert!(with_token.contains("127.0.0.1:7777"), "{with_token}");
        assert!(
            with_token.contains("令牌") || with_token.contains("token"),
            "{with_token}"
        );
        let no_token = rpc_status_text("0.0.0.0:7777", false);
        assert!(no_token.contains("0.0.0.0:7777"), "{no_token}");
        // No token means no listener: loopback is never presented as safe.
        assert!(
            no_token.contains("不会监听") || no_token.contains("stays off"),
            "{no_token}"
        );
    }
}
