//! Plugin catalogue actions: remove, rollback, enable and update passes.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n;
use crate::state::State;
use crate::toast::show_toast;
use crate::AppWindow;

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // ---- plugin removal / rollback --------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_plugin_remove(move |row| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let name = state
                .borrow()
                .plugins
                .get(row as usize)
                .map(|plugin| plugin.name.clone())
                .unwrap_or_default();
            state.borrow().backend.uninstall_plugin(row);
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    format!("已请求卸载 {name}，有运行任务时会拒绝"),
                    format!("Requested uninstalling {name}; refused while tasks are running"),
                ),
            );
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_plugin_rollback(move |row| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let name = state
                .borrow()
                .plugins
                .get(row as usize)
                .map(|plugin| plugin.name.clone())
                .unwrap_or_default();
            state.borrow().backend.rollback_plugin(row);
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    format!("正在把 {name} 回滚到上一版…"),
                    format!("Rolling {name} back to the previous version…"),
                ),
            );
        });
    }

    // ---- plugin toggles go to the daemon --------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_plugin_toggled(move |row, enabled| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let name = state
                .borrow()
                .plugins
                .get(row as usize)
                .map(|plugin| plugin.name.clone())
                .unwrap_or_default();
            state.borrow().backend.set_plugin_enabled(row, enabled);
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    if enabled {
                        format!("已请求启用 {name}，有运行任务时会拒绝")
                    } else {
                        format!("已请求停用 {name}")
                    },
                    if enabled {
                        format!("Requested enabling {name}; refused while tasks are running")
                    } else {
                        format!("Requested disabling {name}")
                    },
                ),
            );
        });
    }

    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_plugin_action(move |action| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            match action.as_str() {
                // Install goes through the package-path field (the button
                // emits `plugin-install` with the typed path).
                "install" => show_toast(
                    &ui,
                    &state,
                    &i18n::tr(
                        "请在「安装插件」输入包目录路径",
                        "Enter the package directory under Install plugin",
                    ),
                ),
                // Check the update manifest and install every outdated
                // plugin; the summary arrives through the notice channel.
                "update-all" => {
                    show_toast(
                        &ui,
                        &state,
                        &i18n::tr("正在检查插件更新…", "Checking for plugin updates…"),
                    );
                    state.borrow().backend.plugin_update_all();
                }
                "reload" => {
                    show_toast(
                        &ui,
                        &state,
                        &i18n::tr("正在重新扫描插件目录…", "Re-scanning the plugin directory…"),
                    );
                    state.borrow().backend.rescan_plugins();
                }
                _ => show_toast(
                    &ui,
                    &state,
                    &i18n::tr("该操作尚未接线", "This action is not wired up yet"),
                ),
            }
        });
    }
}
