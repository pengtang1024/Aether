//! RPC config panel: apply, clear token and generate a random token.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;

use aether_daemon::settings::generate_rpc_secret;

use crate::i18n;
use crate::state::State;
use crate::toast::show_toast;
use crate::{AppWindow, SettingsValidation};

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // ---- RPC config -----------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_rpc_apply(move |listen, secret| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let listen = listen.trim().to_string();
            let secret = secret.trim().to_string();
            // The daemon rejects these too, but an inline hint explains the
            // problem next to the field it belongs to. Invalid input never
            // reaches the daemon.
            let mut invalid = None;
            if !listen.is_empty() {
                match listen.parse::<std::net::SocketAddr>() {
                    Ok(addr) if !addr.ip().is_loopback() && secret.is_empty() => {
                        invalid = Some((
                            "rpc-secret",
                            i18n::tr(
                                "非回环地址必须设置访问令牌",
                                "A non-loopback address requires an access token",
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(_) => {
                        invalid = Some((
                            "rpc-listen",
                            i18n::tr(
                                "监听地址需要 ip:port，例如 127.0.0.1:6800",
                                "The listen address must be ip:port, e.g. 127.0.0.1:6800",
                            ),
                        ));
                    }
                }
            }
            if let Some((key, message)) = invalid {
                let validation = ui.global::<SettingsValidation>();
                validation.set_key(key.into());
                validation.set_message(message.into());
                return;
            }
            let validation = ui.global::<SettingsValidation>();
            if validation.get_key() == "rpc-listen" || validation.get_key() == "rpc-secret" {
                validation.set_message(Default::default());
            }
            // An empty string is an explicit choice: blank listen turns
            // the listener off, blank token removes auth (loopback only).
            state.borrow().backend.set_setting(serde_json::json!({
                "rpc_listen": listen,
                "rpc_secret": secret,
            }));
            let message = if listen.is_empty() {
                i18n::tr(
                    "已关闭远程控制：重启 daemon 后生效",
                    "Remote control disabled; applies after a daemon restart",
                )
            } else {
                i18n::tr(
                    "已保存 RPC 配置：监听地址重启后生效，令牌立即生效",
                    "RPC settings saved; the address applies after a restart, the token immediately",
                )
            };
            show_toast(&ui, &state, &message);
        });
    }
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_rpc_clear_secret(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            state
                .borrow()
                .backend
                .set_setting(serde_json::json!({"rpc_secret": ""}));
            ui.set_settings_rpc_secret_value("".into());
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    "已清除访问令牌：立即生效（仅回环地址可用）",
                    "Access token cleared; it applies immediately (loopback only)",
                ),
            );
        });
    }
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_rpc_random_secret(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            match generate_rpc_secret() {
                Some(secret) => {
                    ui.set_settings_rpc_secret_value(secret.into());
                    show_toast(
                        &ui,
                        &state,
                        &i18n::tr(
                            "已生成随机令牌：点击「应用」保存",
                            "Random token generated; press Apply to save it",
                        ),
                    );
                }
                None => show_toast(
                    &ui,
                    &state,
                    &i18n::tr(
                        "无法从系统获取随机数，请手动填写令牌",
                        "Could not read the OS random source; enter a token manually",
                    ),
                ),
            }
        });
    }
}
