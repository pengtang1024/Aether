//! Dashboard cards: today/week counters, weekly bars, storage and disk.

use slint::{ModelRc, VecModel};

use crate::backend::Snapshot;
use crate::i18n;
use crate::model::format_size;
use crate::AppWindow;

use super::format::{percent_text, share};

/// Fill the dashboard counters that come from the daemon's `stats` payload.
pub(super) fn apply(ui: &AppWindow, snapshot: &Snapshot) {
    let Some(stats) = &snapshot.stats else {
        return;
    };
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
    ui.set_cat_fractions(ModelRc::new(VecModel::from(
        (0..4)
            .map(|index| share(stats.categories[index], four))
            .collect::<Vec<f32>>(),
    )));
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
        ui.set_disk_percent(share(used, stats.disk_total));
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
