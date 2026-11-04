//! Settings search and the `settings-changed` dispatcher.

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::{ModelRc, VecModel};

use crate::i18n;
use crate::prefs::save_theme_mode;
use crate::settings_search;
use crate::state::State;
use crate::status_text::speed_limit_bps;
use crate::toast::show_toast;
use crate::{AppWindow, SettingsFilter, SettingsHit};

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
        if key.as_str() == "theme" {
            let mode = match value.as_str() {
                "浅色" => 1,
                "深色" => 2,
                _ => 0,
            };
            save_theme_mode(mode);
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
