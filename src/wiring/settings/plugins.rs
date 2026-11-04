//! Plugin-related settings and app update policy.

use serde_json::json;

use crate::i18n;
use crate::plugin_settings::parse_plugin_pins;
use crate::state::State;
use crate::AppWindow;

pub(super) fn apply(
    st: &mut State,
    _ui: Option<&AppWindow>,
    key: &str,
    value: &str,
) -> Option<String> {
    let mut hint = None;
    match key {
        "plugin-auto-update" => {
            let enabled = value == "true";
            st.backend
                .set_setting(json!({"plugin_auto_update": enabled}));
            hint = Some(if enabled {
                i18n::tr(
                    "已开启：daemon 每 6 小时检查并安装插件更新",
                    "On; the daemon checks and installs plugin updates every 6 hours",
                )
            } else {
                i18n::tr("已关闭自动更新插件", "Automatic plugin updates are off")
            });
        }
        "plugin-external-process" => {
            let allowed = value == "true";
            st.backend
                .set_setting(json!({"plugin_external_process": allowed}));
            hint = Some(if allowed {
                i18n::tr(
                    "已允许外部进程插件启动",
                    "Plugins that need an external process may start",
                )
            } else {
                i18n::tr(
                    "已阻止外部进程插件：空闲实例已停止，运行中的任务当前进程跑完",
                    "External-process plugins are blocked; idle instances stopped, running tasks finish their process",
                )
            });
        }
        "plugin-pins" => match parse_plugin_pins(value) {
            Ok(pins) => {
                st.backend.set_setting(json!({"plugin_pins": pins}));
                hint = Some(i18n::tr(
                    "版本固定已保存：更新检查会跳过这些插件",
                    "Pins saved; update checks skip these plugins",
                ));
            }
            Err(error) => hint = Some(error),
        },
        "app-auto-update" => {
            let enabled = value == "true";
            st.backend.set_setting(json!({"app_auto_update": enabled}));
            hint = Some(if enabled {
                i18n::tr(
                    "已开启：发现新版本后后台下载",
                    "On; new releases download in the background",
                )
            } else {
                i18n::tr(
                    "已关闭自动下载：发现新版本只提示",
                    "Off; new releases are reported but not downloaded",
                )
            });
        }
        "plugin-install" => {
            let path = value.trim();
            if path.is_empty() {
                hint = Some(i18n::tr(
                    "请输入插件包目录路径（含 plugin.json）",
                    "Enter the plugin package directory (containing plugin.json)",
                ));
            } else {
                // Result (installed / refused: signature, hashes, busy)
                // arrives through the notice channel.
                st.backend.install_plugin(path);
                hint = Some(i18n::tr("正在安装插件…", "Installing the plugin…"));
            }
        }
        _ => {}
    }
    hint
}
