//! UI-local preferences and the theme file.

use std::fs;
use std::path::PathBuf;

pub(crate) fn theme_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var("APPDATA").ok()?;
    #[cfg(target_os = "macos")]
    let base = format!(
        "{}/Library/Application Support",
        std::env::var("HOME").ok()?
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var("XDG_CONFIG_HOME").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|home| format!("{home}/.config"))
    })?;
    Some(PathBuf::from(base).join("aether").join("theme"))
}

pub(crate) fn read_theme_mode() -> Option<i32> {
    let value = fs::read_to_string(theme_config_path()?).ok()?;
    match value.trim() {
        "0" => Some(0),
        "1" => Some(1),
        "2" => Some(2),
        _ => None,
    }
}

pub(crate) fn save_theme_mode(mode: i32) {
    let Some(path) = theme_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, mode.to_string());
}

/// UI-local preferences that have no daemon counterpart. Stored as JSON in
/// the same config directory as the theme file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct UiPrefs {
    /// Accent preset index, see `apply-appearance()` in ui/app.slint.
    pub(crate) accent: i32,
    /// Transitions enabled; false zeroes the motion tokens (accessibility).
    pub(crate) motion: bool,
    /// Decorative flow lines at the window edges.
    pub(crate) flow: bool,
    /// 0 = 主页, 1 = 下载列表, 2 = 上次页面.
    pub(crate) start_page: i32,
    /// Last visited page index, used when `start_page == 2`.
    pub(crate) last_page: i32,
    /// 0 = 不操作, 1 = 打开所在文件夹, 2 = 打开文件.
    pub(crate) after_done: i32,
    /// Desktop notification when a task completes.
    pub(crate) notify_done: bool,
    /// Desktop notification when a task fails (cancellations never notify).
    pub(crate) notify_fail: bool,
    /// Watch the clipboard for a fresh download URL and offer a one-click
    /// toast. Off by default: clipboard access is opt-in.
    pub(crate) clipboard_watch: bool,
    /// Interface language: "zh" (Chinese source, default) or "en"
    /// (translations/en/LC_MESSAGES/aether.po).
    pub(crate) lang: String,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            accent: 0,
            motion: true,
            flow: true,
            start_page: 0,
            last_page: 0,
            after_done: 0,
            notify_done: true,
            notify_fail: true,
            clipboard_watch: false,
            lang: "zh".into(),
        }
    }
}

impl UiPrefs {
    pub(crate) fn config_path() -> Option<PathBuf> {
        theme_config_path().map(|theme| theme.with_file_name("ui.json"))
    }

    pub(crate) fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(raw) = serde_json::to_string_pretty(self) else {
            return;
        };
        let _ = fs::write(path, raw);
    }
}
