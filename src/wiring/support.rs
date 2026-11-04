//! Documentation, community and update-check links on the About panel.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::i18n;
use crate::platform::open_url;
use crate::state::{State, UpdateWatch};
use crate::toast::show_toast;
use crate::AppWindow;

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let ui_handle = ui.as_weak();
    let state = state.clone();
    ui.on_support_action(move |key| {
        let Some(ui) = ui_handle.upgrade() else {
            return;
        };
        let opened = |url: &str, zh: &str, en: &str| {
            if open_url(url) {
                i18n::tr(zh.to_string(), en.to_string())
            } else {
                i18n::tr(
                    format!("无法打开浏览器，请访问 {url}"),
                    format!("Could not open a browser; visit {url}"),
                )
            }
        };
        let message = match key.as_str() {
            "docs" => opened(
                "https://github.com/pengtang1024/Aether/tree/main/docs",
                "已在浏览器打开文档",
                "Documentation opened in the browser",
            ),
            "community" => opened(
                "https://github.com/pengtang1024/Aether/discussions",
                "已在浏览器打开社区",
                "Community opened in the browser",
            ),
            "feedback" => opened(
                "https://github.com/pengtang1024/Aether/issues",
                "已在浏览器打开问题反馈",
                "Issue tracker opened in the browser",
            ),
            "changelog" => opened(
                "https://github.com/pengtang1024/Aether/blob/main/CHANGELOG.md",
                "已在浏览器打开版本说明",
                "Release notes opened in the browser",
            ),
            "license" => opened(
                "https://github.com/pengtang1024/Aether/blob/main/LICENSE",
                "已在浏览器打开许可证",
                "License opened in the browser",
            ),
            "update" => {
                let (connected, configured) = {
                    let st = state.borrow();
                    let snapshot = st.backend.snapshot();
                    (
                        snapshot.connected,
                        snapshot
                            .stats
                            .as_ref()
                            .is_some_and(|stats| stats.update.is_some()),
                    )
                };
                if !connected {
                    i18n::tr("daemon 未连接，无法检查更新", "The daemon is not connected")
                } else if !configured {
                    i18n::tr(
                        "未配置更新源：daemon 需要 --update-url 与更新公钥",
                        "No update source: the daemon needs --update-url and the public key",
                    )
                } else {
                    {
                        let mut st = state.borrow_mut();
                        st.backend.check_update();
                        st.update_watch = Some(UpdateWatch {
                            since: Instant::now(),
                            saw_checking: false,
                        });
                    }
                    i18n::tr("正在检查更新…", "Checking for updates…")
                }
            }
            _ => i18n::tr("操作已记录", "Done"),
        };
        show_toast(&ui, &state, &message);
    });
}
