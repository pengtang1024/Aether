//! Header counters, speed chart and dashboard cards.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ModelRc, VecModel};

use crate::backend::Snapshot;
use crate::chart::{chart_paths, chart_peak};
use crate::i18n;
use crate::model::{format_size, format_speed, Status};
use crate::state::State;
use crate::update_status::{
    plugin_progress_fraction, plugin_progress_text, update_progress_fraction, update_status_text,
};
use crate::AppWindow;

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
    let latest = (history.back().copied().unwrap_or(0.0) / peak).clamp(0.0, 1.0) as f32;
    ui.set_chart_line(line.into());
    ui.set_chart_area(area.into());
    ui.set_chart_latest(latest);
    ui.set_chart_idle(chart_peak(history) <= 0.0);

    // ---- dashboard counters (daemon `stats`) --------------------------------
    if let Some(stats) = &snapshot.stats {
        ui.set_today_bytes(format_size(stats.today_bytes as f64).into());
        ui.set_week_bytes(format_size(stats.week_bytes as f64).into());
        ui.set_version_name(
            if stats.version == env!("CARGO_PKG_VERSION") {
                format!("Aether v{}", stats.version)
            } else {
                i18n::tr(
                    format!(
                        "Aether v{} · 守护进程 v{}",
                        env!("CARGO_PKG_VERSION"),
                        stats.version
                    ),
                    format!(
                        "Aether v{} · daemon v{}",
                        env!("CARGO_PKG_VERSION"),
                        stats.version
                    ),
                )
            }
            .into(),
        );
        if !stats.plugins_dir.is_empty() {
            ui.set_plugins_dir(stats.plugins_dir.clone().into());
        }
        if !stats.data_dir.is_empty() {
            ui.set_data_dir(stats.data_dir.clone().into());
        }
        // Weekly bars: fractions against the busiest day, oldest first.
        let max_daily = stats.daily.iter().copied().fold(0_u64, u64::max);
        let bars: Vec<f32> = stats
            .daily
            .iter()
            .map(|bytes| {
                if max_daily > 0 {
                    (*bytes as f32 / max_daily as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect();
        ui.set_day_bars(ModelRc::new(VecModel::from(bars)));
        ui.set_day_labels(ModelRc::new(VecModel::from(
            stats
                .daily_labels
                .iter()
                .map(|label| slint::SharedString::from(label.as_str()))
                .collect::<Vec<slint::SharedString>>(),
        )));
        // Storage card: share of the four visible categories.
        let four: u64 = stats.categories[..4].iter().sum();
        let fracs: Vec<f32> = (0..4)
            .map(|index| {
                if four > 0 {
                    (stats.categories[index] as f32 / four as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect();
        ui.set_cat_fractions(ModelRc::new(VecModel::from(fracs)));
        ui.set_cat_values(ModelRc::new(VecModel::from(
            stats.categories[..4]
                .iter()
                .map(|bytes| slint::SharedString::from(format_size(*bytes as f64)))
                .collect::<Vec<slint::SharedString>>(),
        )));
        ui.set_cat_percents(ModelRc::new(VecModel::from(
            stats.categories[..4]
                .iter()
                .map(|bytes| slint::SharedString::from(percent_text(*bytes, four)))
                .collect::<Vec<slint::SharedString>>(),
        )));
        let types = stats.categories.iter().filter(|bytes| **bytes > 0).count();
        let week = format_size(stats.week_bytes as f64);
        ui.set_weekly_label(slint::SharedString::from(i18n::tr(
            format!("{week} · {types} 类"),
            format!("{week} · {types} types"),
        )));
        ui.set_avg_file(
            if stats.today_files > 0 {
                format_size(stats.today_bytes as f64 / stats.today_files as f64)
            } else {
                "—".to_string()
            }
            .into(),
        );
        ui.set_success_rate(
            if stats.completed + stats.failed > 0 {
                percent_text(stats.completed, stats.completed + stats.failed)
            } else {
                "—".to_string()
            }
            .into(),
        );
        ui.set_engine_name(i18n::tr("内置插件宿主", "Built-in plugin host").into());
        ui.set_plugins_text(
            i18n::tr(
                format!("{}/{} 个", stats.plugins_enabled, stats.plugins_total),
                format!("{}/{} plugins", stats.plugins_enabled, stats.plugins_total),
            )
            .into(),
        );
        let protocol_count = snapshot
            .plugins
            .iter()
            .flat_map(|plugin| plugin.protocols.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        ui.set_protocols_text(
            i18n::tr(
                format!("{} 种", protocol_count),
                format!("{} protocols", protocol_count),
            )
            .into(),
        );
        if stats.disk_total > 0 {
            let used = stats.disk_total.saturating_sub(stats.disk_free);
            let percent = (used as f64 / stats.disk_total as f64).clamp(0.0, 1.0);
            ui.set_disk_percent(percent as f32);
            ui.set_disk_percent_text(percent_text(used, stats.disk_total).into());
            ui.set_disk_label(
                format!(
                    "{} / {}",
                    format_size(used as f64),
                    format_size(stats.disk_total as f64)
                )
                .into(),
            );
            ui.set_disk_free(
                i18n::tr(
                    format!("剩余 {}", format_size(stats.disk_free as f64)),
                    format!("{} free", format_size(stats.disk_free as f64)),
                )
                .into(),
            );
        }
    }
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

/// Integer percent for compact cards. A nonzero slice that rounds to zero
/// reads "<1%" instead of a misleading "0%".
fn percent_text(part: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    let rounded = (part as f64 / total as f64 * 100.0).round() as u64;
    if part > 0 && rounded == 0 {
        "<1%".to_string()
    } else {
        format!("{rounded}%")
    }
}

#[cfg(test)]
mod tests {
    use super::percent_text;

    #[test]
    fn empty_total_reads_zero() {
        assert_eq!(percent_text(0, 0), "0%");
    }

    #[test]
    fn nonzero_slice_never_reads_zero() {
        assert_eq!(percent_text(18, 7_700), "<1%");
    }

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(percent_text(1, 200), "1%");
        assert_eq!(percent_text(1, 2), "50%");
    }
}
