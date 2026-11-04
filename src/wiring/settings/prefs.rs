//! UI-local preferences: language, appearance, notifications and autostart.

use crate::i18n;
use crate::prefs::{save_theme_mode, UiPrefs};
use crate::state::State;
use crate::tray;
use crate::AppWindow;

pub(super) fn apply(
    st: &mut State,
    ui: Option<&AppWindow>,
    key: &str,
    value: &str,
) -> Option<String> {
    let mut hint = None;
    match key {
        // Persisted locally, no daemon round-trip.
        "language" => {
            // The ComboBox shows the display names; resolve the language
            // code through the shared table so the UI list and the runtime
            // cannot drift.
            let code = i18n::LANGUAGES
                .iter()
                .find(|(_, name)| *name == value)
                .map(|(code, _)| *code)
                .unwrap_or("zh");
            st.prefs.lang = code.to_string();
            st.prefs.save();
            i18n::set_language(code);
            tray::refresh_language();
            if let Err(error) =
                slint::select_bundled_translation(if code == "zh" { "" } else { code })
            {
                eprintln!("aether: language selection failed: {error:?}");
            }
            if let Some(ui) = ui {
                ui.set_settings_language(i18n::index_of(code) as i32);
            }
            hint = Some(if code == "zh" {
                "界面语言已切回简体中文".into()
            } else {
                i18n::tr(
                    "界面语言已切换（Rust 侧字符串仅完整支持中文与英文）",
                    format!(
                        "Language switched to {code} (Rust-side strings cover Chinese and English)"
                    ),
                )
            });
        }
        // UI-local preferences: persist and apply immediately.
        "accent" => {
            let index = match value {
                "靛蓝" => 1,
                "青碧" => 2,
                "苔绿" => 3,
                "琥珀" => 4,
                "蔷薇" => 5,
                _ => 0,
            };
            st.prefs.accent = index;
            st.prefs.save();
            if let Some(ui) = ui {
                ui.set_settings_accent(index);
            }
        }
        "motion" | "flow-background" => {
            let enabled = value == "true";
            if key == "motion" {
                st.prefs.motion = enabled;
                if let Some(ui) = ui {
                    ui.set_settings_motion(enabled);
                }
            } else {
                st.prefs.flow = enabled;
                if let Some(ui) = ui {
                    ui.set_settings_flow(enabled);
                }
            }
            st.prefs.save();
        }
        "start-page" => {
            st.prefs.start_page = match value {
                "下载列表" => 1,
                "上次页面" => 2,
                _ => 0,
            };
            st.prefs.save();
            if let Some(ui) = ui {
                ui.set_settings_start_page(st.prefs.start_page);
            }
        }
        "after-done" => {
            st.prefs.after_done = match value {
                "打开所在文件夹" => 1,
                "打开文件" => 2,
                _ => 0,
            };
            st.prefs.save();
            if let Some(ui) = ui {
                ui.set_settings_after_done(st.prefs.after_done);
            }
        }
        "notify-done" | "notify-fail" => {
            let enabled = value == "true";
            if key == "notify-done" {
                st.prefs.notify_done = enabled;
                if let Some(ui) = ui {
                    ui.set_settings_notify_done(enabled);
                }
            } else {
                st.prefs.notify_fail = enabled;
                if let Some(ui) = ui {
                    ui.set_settings_notify_fail(enabled);
                }
            }
            st.prefs.save();
        }
        "clipboard-watch" => {
            let enabled = value == "true";
            // No backend in this session: refuse the switch instead of saving
            // a preference that does nothing.
            if st.clipboard_control.is_none() {
                if let Some(ui) = ui {
                    ui.set_settings_clipboard(false);
                }
                hint = Some(i18n::tr(
                    "当前会话没有可用的剪贴板后端",
                    "No clipboard backend in this session",
                ));
            } else {
                st.prefs.clipboard_watch = enabled;
                if let Some(control) = st.clipboard_control.as_ref() {
                    control.set_enabled(enabled);
                }
                if let Some(ui) = ui {
                    ui.set_settings_clipboard(enabled);
                }
                st.prefs.save();
                hint = Some(if enabled {
                    i18n::tr(
                        "剪贴板检测已开启：复制下载链接后会弹出提示",
                        "Clipboard detection on; copying a download link offers it",
                    )
                } else {
                    i18n::tr("剪贴板检测已关闭", "Clipboard detection is off")
                });
            }
        }
        "autostart" => {
            let enabled = value == "true";
            match crate::autostart::set_enabled(enabled) {
                Ok(()) => {
                    if let Some(ui) = ui {
                        ui.set_settings_autostart(enabled);
                    }
                    hint = Some(if enabled {
                        i18n::tr("已设置为登录时启动", "Aether will start at login")
                    } else {
                        i18n::tr("已取消登录时启动", "Aether no longer starts at login")
                    });
                }
                Err(error) => {
                    // The OS refused: bounce the switch back and say why
                    // instead of showing a fake "on".
                    if let Some(ui) = ui {
                        ui.set_settings_autostart(!enabled);
                    }
                    hint = Some(i18n::tr(
                        format!("无法修改开机自启：{error}"),
                        format!("Could not change start-at-login: {error}"),
                    ));
                }
            }
        }
        "reset-settings" => {
            st.prefs = UiPrefs::default();
            st.prefs.save();
            if let Some(control) = st.clipboard_control.as_ref() {
                control.set_enabled(false);
            }
            if let Some(ui) = ui {
                ui.set_settings_clipboard(false);
            }
            // Language and theme count as appearance: reset them now instead
            // of leaving the runtime on the old value until the next launch.
            i18n::set_language("zh");
            tray::refresh_language();
            if let Err(error) = slint::select_bundled_translation("") {
                eprintln!("aether: language selection failed: {error:?}");
            }
            save_theme_mode(0);
            if let Some(ui) = ui {
                ui.set_settings_accent(0);
                ui.set_settings_motion(true);
                ui.set_settings_flow(true);
                ui.set_settings_start_page(0);
                ui.set_settings_after_done(0);
                ui.set_settings_notify_done(true);
                ui.set_settings_notify_fail(true);
                ui.set_settings_language(0);
                ui.set_theme_mode(0);
            }
            hint = Some(i18n::tr(
                "界面偏好已恢复默认（含语言与主题）",
                "UI preferences restored to defaults (including language and theme)",
            ));
        }
        _ => {}
    }
    hint
}
