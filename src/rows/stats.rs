//! Header counters, speed chart and dashboard cards.

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::Snapshot;
use crate::chart::{chart_paths, chart_peak};
use crate::i18n;
use crate::model::{format_speed, Status};
use crate::state::State;
use crate::update_status::{
    plugin_progress_fraction, plugin_progress_text, update_progress_fraction, update_status_text,
};
use crate::AppWindow;

mod dashboard;
mod format;

pub(super) fn apply(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    let st = state.borrow();
    let tasks = &snapshot.tasks;
    let total = tasks.len() as i32;
    let active = tasks
        .iter()
        .filter(|task| task.status == Status::Downloading)
        .count() as i32;
    let done = tasks
        .iter()
        .filter(|task| task.status == Status::Completed)
        .count() as i32;
    let paused = tasks
        .iter()
        .filter(|task| task.status == Status::Paused)
        .count() as i32;
    let failed = tasks
        .iter()
        .filter(|task| task.status == Status::Failed)
        .count() as i32;
    let seed = tasks.iter().filter(|task| task.torrent).count() as i32;
    let speed: f64 = tasks
        .iter()
        .filter(|task| task.status == Status::Downloading)
        .map(|task| task.speed)
        .sum();

    if !snapshot.download_dir.is_empty() {
        ui.set_default_download_dir(snapshot.download_dir.clone().into());
    }

    ui.set_count_all(total);
    ui.set_count_active(active);
    ui.set_count_done(done);
    ui.set_count_paused(paused);
    ui.set_count_failed(failed);
    ui.set_count_seed(seed);
    ui.set_connections(
        snapshot
            .settings
            .as_ref()
            .and_then(|settings| settings["connections"].as_u64())
            .unwrap_or(4) as i32,
    );
    ui.set_total_speed(format_speed(speed).into());

    let history = &snapshot.history;
    let (line, area) = chart_paths(history);
    let peak = chart_peak(history).max(1.0);
    let latest = format::ratio(history.back().copied().unwrap_or(0.0), peak);
    ui.set_chart_line(line.into());
    ui.set_chart_area(area.into());
    ui.set_chart_latest(latest);
    ui.set_chart_idle(chart_peak(history) <= 0.0);

    dashboard::apply(ui, snapshot);

    // About panel: the daemon's real update state (null = no update source).
    let (update_text, update_progress) = match snapshot.stats.as_ref() {
        Some(stats) => (
            update_status_text(stats.update.as_ref()),
            update_progress_fraction(stats.update.as_ref()),
        ),
        None => (i18n::tr("daemon 未连接", "Daemon is not connected"), 0.0),
    };
    ui.set_settings_update_status(update_text.into());
    ui.set_settings_update_progress(update_progress);
    // A check has no fraction: show the sweeping sheen instead of a frozen bar.
    let checking = snapshot
        .stats
        .as_ref()
        .and_then(|stats| stats.update.as_ref())
        .and_then(|update| update["status"].as_str())
        == Some("checking");
    ui.set_settings_update_progress_visible(update_progress > 0.0 || checking);

    // Plugin update/install pass: same bar language, per-plugin granularity.
    let plugin_progress = snapshot.stats.as_ref().map(|stats| &stats.plugin_progress);
    let plugin_running = plugin_progress
        .and_then(|value| value["running"].as_bool())
        .unwrap_or(false);
    ui.set_settings_plugin_progress(plugin_progress_fraction(plugin_progress));
    ui.set_settings_plugin_progress_visible(plugin_running);
    ui.set_settings_plugin_progress_text(plugin_progress_text(plugin_progress).into());

    let peak_now = snapshot.history.iter().copied().fold(0.0_f64, f64::max);
    ui.set_peak_speed(if peak_now > 0.0 {
        format_speed(peak_now).into()
    } else {
        "—".into()
    });
    let minutes = st.started.elapsed().as_secs() / 60;
    ui.set_uptime_text(
        i18n::tr(
            if minutes < 60 {
                format!("{minutes} 分钟")
            } else {
                format!("{}h {}m", minutes / 60, minutes % 60)
            },
            if minutes < 60 {
                format!("{minutes} min")
            } else {
                format!("{}h {}m", minutes / 60, minutes % 60)
            },
        )
        .into(),
    );
    ui.set_extension_status(
        if st.extension_installed {
            i18n::tr("已安装", "Installed")
        } else {
            i18n::tr("未安装", "Not installed")
        }
        .into(),
    );
    ui.set_extension_connected(st.extension_installed);
}
