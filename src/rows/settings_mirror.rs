//! Mirror the daemon's settings into the network and scheduler panels.

use std::cell::RefCell;
use std::rc::Rc;

use aether_daemon::settings::RPC_LISTEN_DEFAULT;

use crate::backend::Snapshot;
use crate::plugin_settings::format_plugin_pins;
use crate::state::State;
use crate::status_text::{rpc_status_text, speed_limit_mode};
use crate::AppWindow;

pub(super) fn apply(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    let mut st = state.borrow_mut();

    if !snapshot.download_dir.is_empty() {
        ui.set_default_download_dir(snapshot.download_dir.clone().into());
    }

    if snapshot.settings != st.pushed_settings {
        st.pushed_settings = snapshot.settings.clone();
        if let Some(settings) = snapshot.settings.as_ref() {
            ui.set_settings_dns(settings["dns"].as_str().unwrap_or("").to_string().into());
            ui.set_settings_user_agent(
                settings["user_agent"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
                    .into(),
            );
            ui.set_settings_ip_mode(match settings["ip_mode"].as_str() {
                Some("ipv4_only") => 1,
                Some("ipv6_only") => 2,
                _ => 0,
            });
            // Proxy: show the daemon's real mode and address instead of
            // guessing (pac has no user-editable URL, so it is display-only).
            let proxy = &settings["proxy"];
            ui.set_settings_proxy_mode(match proxy["mode"].as_str() {
                Some("none") => 1,
                Some("custom") => 2,
                Some("pac") => 3,
                _ => 0,
            });
            ui.set_settings_proxy_url(proxy["url"].as_str().unwrap_or("").into());
            // Network extras: default headers, FTP mode, BT knobs.
            ui.set_settings_headers(settings["default_headers"].as_str().unwrap_or("").into());
            ui.set_settings_ftp_passive(settings["ftp_passive"].as_bool().unwrap_or(true));
            ui.set_settings_bt_port(
                settings["bt_listen_port"]
                    .as_u64()
                    .map(|port| port.to_string())
                    .unwrap_or_else(|| "51413".into())
                    .into(),
            );
            ui.set_settings_bt_dht(settings["bt_dht"].as_bool().unwrap_or(true));
            ui.set_settings_bt_upload_kbps(
                (settings["bt_upload_bps"].as_u64().unwrap_or(0) as f32 / 1024.0).round(),
            );
            ui.set_settings_bt_blocklist(settings["bt_blocklist"].as_str().unwrap_or("").into());
            ui.set_settings_bt_trackers(settings["bt_trackers"].as_str().unwrap_or("").into());
            ui.set_settings_media_ffmpeg(settings["ffmpeg_path"].as_str().unwrap_or("").into());
            ui.set_settings_plugin_auto_update(
                settings["plugin_auto_update"].as_bool().unwrap_or(false),
            );
            ui.set_settings_plugin_pins(format_plugin_pins(&settings["plugin_pins"]).into());
            ui.set_settings_plugin_external_process(
                settings["plugin_external_process"]
                    .as_bool()
                    .unwrap_or(true),
            );
            // Missing means "on": that is what the update loop did before the
            // switch existed.
            ui.set_settings_app_auto_update(settings["app_auto_update"].as_bool().unwrap_or(true));
            // RPC is configured through the same settings file. A missing
            // address means an older daemon that has not seeded the default
            // yet; an empty string is the explicit "off" state. The token is
            // mirrored into the masked field so the random button can replace
            // it and the user can copy the generated one.
            let rpc_listen = settings["rpc_listen"]
                .as_str()
                .unwrap_or(RPC_LISTEN_DEFAULT)
                .to_string();
            let rpc_secret = settings["rpc_secret"].as_str().unwrap_or("");
            ui.set_settings_rpc_listen(rpc_listen.clone().into());
            ui.set_settings_rpc_listen_value(rpc_listen.clone().into());
            ui.set_settings_rpc_secret_value(rpc_secret.to_string().into());
            ui.set_settings_rpc_status(rpc_status_text(&rpc_listen, !rpc_secret.is_empty()).into());
            // Scheduled window (daemon-side; HH:MM strings, blank = unset).
            ui.set_settings_schedule_on(settings["schedule_on"].as_bool().unwrap_or(false));
            ui.set_settings_schedule_start(
                settings["schedule_start"].as_str().unwrap_or("").into(),
            );
            ui.set_settings_schedule_end(settings["schedule_end"].as_str().unwrap_or("").into());
            // Seed the proxy bookkeeping so a later mode switch pushes a
            // complete setting instead of guessing at the daemon's URL.
            let mode = proxy["mode"].as_str().unwrap_or("system");
            st.proxy_mode = match mode {
                "none" => "直连".into(),
                "custom" => "手动".into(),
                "pac" => "PAC".into(),
                _ => "跟随系统".into(),
            };
            st.proxy_url = proxy["url"].as_str().unwrap_or("").into();

            // Scheduler limits: report what the daemon is running with, so
            // the download page shows the truth instead of widget defaults.
            // A value outside a combo box's model maps to -1 (no selection),
            // which reads as "set from outside this list".
            let index_of = |value: Option<u64>, model: &[u64]| {
                value
                    .and_then(|value| model.iter().position(|entry| *entry == value))
                    .map(|index| index as i32)
                    .unwrap_or(-1)
            };
            ui.set_settings_max_tasks(index_of(
                settings["max_concurrent"].as_u64(),
                &[3, 5, 10, 16],
            ));
            ui.set_settings_max_connections(index_of(
                settings["connections"].as_u64(),
                &[2, 4, 8, 16],
            ));
            ui.set_settings_retry(index_of(settings["retry_max"].as_u64(), &[0, 3, 5, 10]));
            let speed = settings["max_speed_bps"].as_u64().unwrap_or(0);
            st.global_limit = speed > 0;
            ui.set_settings_global_limit(st.global_limit);
            ui.set_speed_limit_mode(speed_limit_mode(speed));
            if speed > 0 {
                // The slider works in whole KB/s, so a smaller cap rounds up
                // rather than showing a limit the daemon does not have.
                st.download_limit_kb = (speed / 1024).max(1) as f64;
                ui.set_settings_download_limit_kb(st.download_limit_kb as f32);
            }
        }
    }
}
