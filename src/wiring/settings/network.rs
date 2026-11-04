//! Network, proxy, FTP and BitTorrent setting edits.

use serde_json::json;

use crate::i18n;
use crate::state::State;
use crate::AppWindow;

pub(super) fn apply(
    st: &mut State,
    ui: Option<&AppWindow>,
    key: &str,
    value: &str,
) -> Option<String> {
    let mut hint = None;
    match key {
        "proxy-url" => {
            st.proxy_url = value.to_string();
            // Editing the address after choosing 手动 applies it.
            if st.proxy_mode == "手动" && !st.proxy_url.trim().is_empty() {
                st.backend.set_proxy("手动", &st.proxy_url);
            }
        }
        "proxy-mode" => {
            let previous = st.proxy_mode.clone();
            st.proxy_mode = value.to_string();
            match st.proxy_mode.as_str() {
                // PAC needs a URL from the daemon's start configuration.
                // Bounce the combo box back so it never shows a mode the
                // daemon is not using.
                "PAC" if previous != "PAC" => {
                    st.proxy_mode = previous;
                    if let Some(ui) = ui {
                        let index = match st.proxy_mode.as_str() {
                            "直连" => 1,
                            "手动" => 2,
                            "PAC" => 3,
                            _ => 0,
                        };
                        ui.set_settings_proxy_mode(index);
                    }
                    hint = Some(
                        i18n::tr("PAC 由启动配置（AETHER_PAC_URL）决定，请用启动参数切换", "PAC comes from the launch configuration (AETHER_PAC_URL); switch it with launch options"),
                    );
                }
                "手动" if st.proxy_url.trim().is_empty() => {
                    hint = Some(i18n::tr(
                        "请先填写代理地址，再切换到手动模式",
                        "Enter the proxy address before switching to manual mode",
                    ))
                }
                _ => {
                    let url = st.proxy_url.clone();
                    st.backend.set_proxy(&st.proxy_mode, &url);
                }
            }
        }
        // Network settings go through the same set_settings RPC;
        // blank input is normalised to "off" by the daemon.
        "dns" => {
            st.backend.set_setting(json!({"dns": value}));
            hint = Some(i18n::tr(
                "DNS 已保存：空闲插件重启后生效",
                "DNS saved; idle plugins pick it up after a restart",
            ));
        }
        "user-agent" => {
            st.backend.set_setting(json!({"user_agent": value}));
            hint = Some(i18n::tr(
                "User-Agent 已保存：空闲插件重启后生效",
                "User-Agent saved; idle plugins pick it up after a restart",
            ));
        }
        "ip-mode" => {
            let patch = match value {
                "仅 IPv4" => json!({"ip_mode": "ipv4_only"}),
                "仅 IPv6" => json!({"ip_mode": "ipv6_only"}),
                _ => json!({"ip_mode": null}),
            };
            st.backend.set_setting(patch);
            hint = Some(i18n::tr(
                "IP 协议已保存：空闲插件重启后生效",
                "IP protocol saved; idle plugins pick it up after a restart",
            ));
        }
        // Network extras and BT knobs: same set_settings channel.
        "headers" => {
            st.backend.set_setting(json!({"default_headers": value}));
            hint = Some(i18n::tr(
                "请求头已保存：空闲插件重启后生效",
                "Custom headers saved; idle plugins pick it up after a restart",
            ));
        }
        "ffmpeg-path" => {
            st.backend.set_setting(json!({"ffmpeg_path": value}));
            hint = Some(i18n::tr(
                "ffmpeg 路径已保存：对之后开始的任务生效",
                "ffmpeg path saved; applies to tasks started afterwards",
            ));
        }
        "ftp-passive" => {
            st.backend.set_setting(json!({
                "ftp_passive": value == "true"
            }));
            hint = Some(i18n::tr(
                "FTP 模式已保存：空闲插件重启后生效",
                "FTP mode saved; idle plugins pick it up after a restart",
            ));
        }
        "bt-port" => match value.trim().parse::<u16>() {
            Ok(0) | Err(_) => {
                hint = Some(i18n::tr(
                    "监听端口需要 1-65535 的数字",
                    "The listen port must be between 1 and 65535",
                ))
            }
            Ok(port) => {
                st.backend.set_setting(json!({"bt_listen_port": port}));
                hint = Some(i18n::tr(
                    "BT 端口已保存：空闲时重启生效",
                    "BT port saved; applies when idle / after a restart",
                ));
            }
        },
        "bt-dht" => {
            st.backend.set_setting(json!({
                "bt_dht": value == "true"
            }));
            hint = Some(i18n::tr(
                "DHT 已保存：空闲时重启生效",
                "DHT saved; applies when idle / after a restart",
            ));
        }
        "bt-upload-limit" => {
            // Slider reports KB/s; 0 is the stored form of "no cap".
            let kbps = value.trim().parse::<f64>().unwrap_or(0.0).max(0.0);
            let bps = (kbps * 1024.0) as u64;
            st.backend.set_setting(json!({"bt_upload_bps": bps}));
            hint = Some(i18n::tr(
                "上传限速已保存：对新任务与重启后的做种生效",
                "Upload limit saved; applies to new tasks and to seeding after a restart",
            ));
        }
        "bt-blocklist" => {
            st.backend.set_setting(json!({"bt_blocklist": value}));
            hint = Some(i18n::tr(
                "屏蔽列表已保存：空闲时重启生效",
                "Blocklist saved; applies when idle / after a restart",
            ));
        }
        "bt-trackers" => {
            st.backend.set_setting(json!({"bt_trackers": value}));
            hint = Some(i18n::tr(
                "Tracker 已保存：附加到之后的任务",
                "Tracker saved; attached to tasks added afterwards",
            ));
        }
        _ => {}
    }
    hint
}
