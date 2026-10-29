//! Aether — a native Material 3 download manager.
//!
//! Rust bridges the Slint window to `aether-daemon` (`src/backend.rs`); Slint
//! renders everything with its own Material 3 widget set. There is no WebView
//! anywhere in the stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod backend;
mod chart;
mod model;

slint::include_modules!();

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Model, ModelRc, Timer, TimerMode, VecModel};

use aether_daemon::PluginInfo;
use backend::Backend;
use chart::{chart_paths, chart_peak};
use model::{
    format_eta, format_pair, format_size, format_speed, rel_time, Status, Task, CHUNK_COUNT,
};

struct State {
    backend: Backend,
    filter: i32,
    query: String,
    detail_id: i32,
    rows: Rc<VecModel<TaskRow>>,
    records: Rc<VecModel<RecordRow>>,
    chunks: Rc<VecModel<f32>>,
    /// Ids of the rows currently materialised in `rows`, in order.
    row_ids: Vec<i32>,
    /// Ids of the records currently materialised in `records`, in order.
    record_ids: Vec<i32>,
    /// When the current toast should disappear.
    toast_until: Option<Instant>,
    /// Task id the chunk model was seeded for.
    detail_data_for: i32,
    /// Last proxy URL typed on the settings page, so switching the mode can
    /// push a complete proxy setting.
    proxy_url: String,
    proxy_mode: String,
    /// Global speed cap as the download page shows it: the switch state and
    /// the KB/s the slider holds (kept while the cap is off, so switching it
    /// back on restores the previous rate).
    global_limit: bool,
    download_limit_kb: f64,
    /// Settings JSON last pushed into the window, so the mirror is only
    /// written when the daemon actually reported a change.
    pushed_settings: Option<serde_json::Value>,
    /// UI-local preferences (accent, motion, flow, start page, after-done).
    prefs: UiPrefs,
    /// Last page index seen in the window, for "上次页面" persistence.
    seen_page: i32,
    last_notice: Option<String>,
    /// Installed plugins mirrored from the snapshot (row id → plugin id).
    plugins: Vec<PluginInfo>,
    /// Change signature so the catalog model is only rebuilt on real change.
    plugins_sig: String,
    /// Session start, for the dashboard uptime row.
    started: Instant,
    /// Whether a browser extension native-host manifest is present.
    extension_installed: bool,
}

impl State {
    fn new() -> Self {
        State {
            backend: Backend::spawn(),
            filter: 0,
            query: String::new(),
            detail_id: -1,
            rows: Rc::new(VecModel::default()),
            records: Rc::new(VecModel::default()),
            chunks: Rc::new(VecModel::from(vec![0.0_f32; CHUNK_COUNT])),
            row_ids: Vec::new(),
            record_ids: Vec::new(),
            toast_until: None,
            detail_data_for: -1,
            proxy_url: String::new(),
            proxy_mode: "跟随系统".into(),
            global_limit: true,
            download_limit_kb: 2048.0,
            pushed_settings: None,
            prefs: UiPrefs::load(),
            seen_page: 0,
            last_notice: None,
            plugins: Vec::new(),
            plugins_sig: String::new(),
            started: Instant::now(),
            extension_installed: browser_extension_installed(),
        }
    }

    fn matches(&self, task: &model::Task) -> bool {
        let status_ok = match self.filter {
            0 => true,
            1 => task.status == Status::Downloading,
            2 => task.status == Status::Completed,
            3 => task.status == Status::Paused,
            4 => task.status == Status::Failed,
            5 => task.torrent,
            _ => true,
        };
        if !status_ok {
            return false;
        }
        if self.query.is_empty() {
            return true;
        }
        let needle = self.query.to_lowercase();
        task.name.to_lowercase().contains(&needle)
            || task.source.to_lowercase().contains(&needle)
            || task.note.to_lowercase().contains(&needle)
    }

    fn visible_ids(&self, tasks: &[model::Task]) -> Vec<i32> {
        tasks
            .iter()
            .filter(|task| self.matches(task))
            .map(|task| task.id)
            .collect()
    }
}

/// Category tone for the row glyph. The extension text is never displayed;
/// the filename is revealed in the hover card instead.
fn file_tone(name: &str) -> i32 {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.zst")
    {
        return 1;
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "iso" | "img" | "dmg" => 2,
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" => 1,
        "mkv" | "mp4" | "avi" | "mov" | "webm" => 3,
        "appimage" | "exe" | "deb" | "rpm" => 4,
        _ => 0,
    }
}

/// One-line purpose per first-party plugin, shown in the settings catalogue.
fn plugin_blurb(info: &PluginInfo) -> String {
    match info.id.as_str() {
        "http" => "HTTP / HTTPS 多线程下载".into(),
        "bt" => "BT / 磁力链下载与做种".into(),
        "ftp" => "FTP / FTPS 站点传输".into(),
        "sftp" => "SFTP / SSH 文件传输".into(),
        "media" => "网页音视频嗅探（HLS / DASH）".into(),
        "extractor" => "yt-dlp 视频站点支持".into(),
        "rtmp" => "RTMP 直播流录制".into(),
        "webdav" => "WebDAV 远程目录".into(),
        "s3" => "S3 / 对象存储".into(),
        "gdrive" => "Google Drive 分享链接".into(),
        "onedrive" => "OneDrive 分享链接".into(),
        "dropbox" => "Dropbox 分享链接".into(),
        "baidupan" => "百度网盘（官方 API）".into(),
        "aliyunpan" => "阿里云盘（官方 API）".into(),
        "thunder" => "迅雷链接".into(),
        "ipfs" => "IPFS / IPNS 内容".into(),
        other => {
            if info.protocols.is_empty() {
                format!("协议：{other}")
            } else {
                format!("协议：{}", info.protocols.join(", "))
            }
        }
    }
}

/// Catalogue grouping; mirrors the filter tabs (全部/内置/已启用).
fn plugin_category(id: &str) -> &'static str {
    match id {
        "http" => "HTTP",
        "bt" => "BT",
        "ftp" | "sftp" => "传输",
        "media" | "extractor" | "rtmp" => "媒体",
        "webdav" | "s3" | "ipfs" => "存储",
        "gdrive" | "onedrive" | "dropbox" | "baidupan" | "aliyunpan" | "thunder" => "网盘",
        _ => "其他",
    }
}

fn plugin_icon(id: &str) -> i32 {
    match id {
        "http" => 0,                // link
        "bt" => 10,                 // seed
        "ftp" | "sftp" => 5,        // drive
        "media" | "extractor" => 1, // play
        "rtmp" => 11,               // signal
        "webdav" | "s3" | "gdrive" | "onedrive" | "dropbox" | "baidupan" | "aliyunpan" => 3, // cloud
        "thunder" => 11,
        "ipfs" => 9, // layers
        _ => 6,      // list
    }
}

fn plugin_tone(id: &str) -> i32 {
    match id {
        "http" => 0,
        "bt" => 1,
        "ftp" | "sftp" => 2,
        "media" | "extractor" | "rtmp" => 3,
        "webdav" | "s3" | "ipfs" => 4,
        _ => 2,
    }
}

/// Standard native-messaging manifest locations; any `aether*.json` counts.
fn browser_extension_installed() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let home = PathBuf::from(home);
    let dirs = [
        ".mozilla/native-messaging-hosts",
        ".config/google-chrome/NativeMessagingHosts",
        ".config/chromium/NativeMessagingHosts",
        ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
    ];
    dirs.iter().any(|dir| {
        let dir = home.join(dir);
        dir.is_dir()
            && std::fs::read_dir(&dir).ok().is_some_and(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    name.starts_with("aether") && name.ends_with(".json")
                })
            })
    })
}

fn to_row(task: &model::Task, detail_id: i32) -> TaskRow {
    let status = task.status;
    let eta = match status {
        Status::Downloading => format_eta(task.remaining_seconds()),
        Status::Failed => "校验失败".to_string(),
        Status::Paused => "已暂停".to_string(),
        Status::Queued => "等待开始".to_string(),
        _ => String::new(),
    };
    let kind_tone = file_tone(&task.name);
    TaskRow {
        id: task.id,
        name: task.name.clone().into(),
        source: task.source.clone().into(),
        kind_tone,
        progress: task.progress(),
        progress_text: format_pair(task.done, task.total).into(),
        speed: if status == Status::Downloading || status == Status::Seeding {
            format_speed(task.speed).into()
        } else {
            "—".into()
        },
        eta: eta.into(),
        status: status.code(),
        status_text: status.label().into(),
        paused: status == Status::Paused,
        completed: status == Status::Completed,
        failed: status == Status::Failed,
        selected: task.id == detail_id,
    }
}

fn refresh_rows(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let mut st = state.borrow_mut();
    let snapshot = st.backend.snapshot();
    let ids = st.visible_ids(&snapshot.tasks);
    let same_shape = ids == st.row_ids;
    let detail_id = st.detail_id;
    let rows: Vec<TaskRow> = ids
        .iter()
        .filter_map(|id| snapshot.tasks.iter().find(|task| task.id == *id))
        .map(|task| to_row(task, detail_id))
        .collect();

    if same_shape {
        for (index, row) in rows.into_iter().enumerate() {
            st.rows.set_row_data(index, row);
        }
    } else {
        st.row_ids = ids;
        st.rows.set_vec(rows);
    }

    ui.set_tasks(ModelRc::from(st.rows.clone()));

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

    // Mirror the daemon's settings into the network panel when they change;
    // the page pushes edits back through `settings-changed`.
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
                (settings["bt_upload_bps"].as_u64().unwrap_or(0) as f32 / 1024.0).round() as f32,
            );
            ui.set_settings_bt_blocklist(settings["bt_blocklist"].as_str().unwrap_or("").into());
            ui.set_settings_bt_trackers(settings["bt_trackers"].as_str().unwrap_or("").into());
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
            if speed > 0 {
                // The slider works in whole KB/s, so a smaller cap rounds up
                // rather than showing a limit the daemon does not have.
                st.download_limit_kb = (speed / 1024).max(1) as f64;
                ui.set_settings_download_limit_kb(st.download_limit_kb as f32);
            }
        }
    }

    ui.set_count_all(total);
    ui.set_count_active(active);
    ui.set_count_done(done);
    ui.set_count_paused(paused);
    ui.set_count_failed(failed);
    ui.set_count_seed(seed);
    ui.set_total_speed(format_speed(speed).into());

    let history = &snapshot.history;
    let (line, area) = chart_paths(history);
    let peak = chart_peak(history).max(1.0);
    let latest = (history.back().copied().unwrap_or(0.0) / peak).clamp(0.0, 1.0) as f32;
    ui.set_chart_line(line.into());
    ui.set_chart_area(area.into());
    ui.set_chart_latest(latest);

    // ---- dashboard counters (daemon `stats`) --------------------------------
    if let Some(stats) = &snapshot.stats {
        ui.set_today_bytes(format_size(stats.today_bytes as f64).into());
        ui.set_week_bytes(format_size(stats.week_bytes as f64).into());
        ui.set_version_name(format!("Aether v{}", stats.version).into());
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
                .map(|bytes| {
                    slint::SharedString::from(format!(
                        "{}%",
                        if four > 0 {
                            (*bytes as f32 / four as f32 * 100.0).round() as i32
                        } else {
                            0
                        }
                    ))
                })
                .collect::<Vec<slint::SharedString>>(),
        )));
        let types = stats.categories.iter().filter(|bytes| **bytes > 0).count();
        ui.set_weekly_label(slint::SharedString::from(format!(
            "{} · {} 类",
            format_size(stats.week_bytes as f64),
            types
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
                format!(
                    "{}%",
                    (stats.completed as f64 / (stats.completed + stats.failed) as f64 * 100.0)
                        .round() as i32
                )
            } else {
                "—".to_string()
            }
            .into(),
        );
        ui.set_engine_name("内置插件宿主".into());
        ui.set_plugins_text(format!("{}/{} 个", stats.plugins_enabled, stats.plugins_total).into());
        ui.set_protocols_text(
            format!(
                "{} 种",
                snapshot
                    .plugins
                    .iter()
                    .flat_map(|plugin| plugin.protocols.iter())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            )
            .into(),
        );
        if stats.disk_total > 0 {
            let used = stats.disk_total.saturating_sub(stats.disk_free);
            let percent = (used as f64 / stats.disk_total as f64).clamp(0.0, 1.0);
            ui.set_disk_percent(percent as f32);
            ui.set_disk_percent_text(format!("{}%", (percent * 100.0).round() as i32).into());
            ui.set_disk_label(
                format!(
                    "{} / {}",
                    format_size(used as f64),
                    format_size(stats.disk_total as f64)
                )
                .into(),
            );
            ui.set_disk_free(format!("剩余 {}", format_size(stats.disk_free as f64)).into());
        }
    }
    let peak_now = snapshot.history.iter().copied().fold(0.0_f64, f64::max);
    ui.set_peak_speed(if peak_now > 0.0 {
        format_speed(peak_now).into()
    } else {
        "—".into()
    });
    let minutes = st.started.elapsed().as_secs() / 60;
    ui.set_uptime_text(
        if minutes < 60 {
            format!("{minutes} 分钟")
        } else {
            format!("{}h {}m", minutes / 60, minutes % 60)
        }
        .into(),
    );
    ui.set_extension_status(
        if st.extension_installed {
            "已安装"
        } else {
            "未安装"
        }
        .into(),
    );
    ui.set_extension_connected(st.extension_installed);

    // ---- plugin catalogue (daemon registry) ---------------------------------
    let sig = snapshot
        .plugins
        .iter()
        .map(|plugin| format!("{}:{}:{}", plugin.id, plugin.version, plugin.enabled))
        .collect::<Vec<_>>()
        .join("|");
    if sig != st.plugins_sig {
        st.plugins_sig = sig;
        st.plugins = snapshot.plugins.clone();
        let total = snapshot.plugins.len() as i32;
        let enabled = snapshot.plugins.iter().filter(|p| p.enabled).count() as i32;
        let rows: Vec<PluginRow> = snapshot
            .plugins
            .iter()
            .enumerate()
            .map(|(index, info)| PluginRow {
                id: index as i32,
                name: info.name.clone().into(),
                description: plugin_blurb(info).into(),
                version: info.version.clone().into(),
                category: plugin_category(&info.id).into(),
                kind: 0,
                enabled: info.enabled,
                update: false,
                featured: false,
                icon: plugin_icon(&info.id),
                tone: plugin_tone(&info.id),
            })
            .collect();
        ui.global::<PluginCatalog>()
            .set_all(ModelRc::new(VecModel::from(rows)));
        ui.global::<PluginCatalog>().set_total(total);
        ui.global::<PluginCatalog>().set_active(enabled);
        ui.global::<PluginCatalog>().set_builtin(total);
        ui.global::<PluginCatalog>().set_community(0);
        ui.global::<PluginCatalog>().set_updates(0);
    }

    let now_ms = st.backend.now_ms();
    let terminal: Vec<&Task> = tasks
        .iter()
        .filter(|task| matches!(task.status, Status::Completed | Status::Failed))
        .take(5)
        .collect();
    let record_ids: Vec<i32> = terminal.iter().map(|task| task.id).collect();
    let records: Vec<RecordRow> = terminal
        .iter()
        .map(|task| RecordRow {
            id: task.id,
            name: task.name.clone().into(),
            kind_tone: file_tone(&task.name),
            status: task.status.code(),
            status_text: task.status.label().into(),
            time: rel_time(task.time_ms, now_ms).into(),
        })
        .collect();
    // Keep the repeater stable so hover tips are not reset on every tick.
    if record_ids == st.record_ids {
        for (index, record) in records.into_iter().enumerate() {
            st.records.set_row_data(index, record);
        }
    } else {
        st.record_ids = record_ids;
        st.records.set_vec(records);
    }
    ui.set_records(ModelRc::from(st.records.clone()));
}

fn refresh_detail(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let mut st = state.borrow_mut();
    let detail_id = st.detail_id;
    let Some(task) = st
        .backend
        .snapshot()
        .tasks
        .into_iter()
        .find(|task| task.id == detail_id)
    else {
        return;
    };

    let (line, area) = chart_paths(&task.history);
    let peak = chart_peak(&task.history).max(1.0);
    let latest = (task.history.back().copied().unwrap_or(0.0) / peak).clamp(0.0, 1.0) as f32;
    let done_chunks = task.chunks().iter().filter(|value| **value >= 1.0).count();
    let note = if task.note.is_empty() {
        "—".to_string()
    } else {
        task.note.clone()
    };

    let kind_tone = file_tone(&task.name);
    ui.set_detail(TaskDetail {
        id: task.id,
        name: task.name.clone().into(),
        kind_tone,
        status: task.status.code(),
        status_text: task.status.label().into(),
        progress: task.progress(),
        progress_text: format!(
            "{} / {}  ·  {:.1}%",
            format_size(task.done),
            format_size(task.total as f64),
            task.progress() * 100.0
        )
        .into(),
        speed: format_speed(task.speed).into(),
        eta: format_eta(task.remaining_seconds()).into(),
        size: format_size(task.total as f64).into(),
        path: task.path.clone().into(),
        note: note.into(),
        source: task.source.clone().into(),
        line_path: line.into(),
        area_path: area.into(),
        peak: format_speed(chart_peak(&task.history)).into(),
        sample: latest,
        chunks_text: format!("{done_chunks} / {CHUNK_COUNT} 个分片已完成").into(),
    });

    if st.detail_data_for != st.detail_id {
        // Opening (or switching) a task: seed the chunk model once. Keeping
        // it stable afterwards avoids per-tick repeater resets.
        st.chunks.set_vec(task.chunks());
        st.detail_data_for = st.detail_id;
        ui.set_chunks(ModelRc::from(st.chunks.clone()));
    } else {
        // The chunk grid is the only per-tick model: update cells in place so
        // the repeater keeps its layout.
        for (index, value) in task.chunks().iter().enumerate() {
            if st.chunks.row_data(index) != Some(*value) {
                st.chunks.set_row_data(index, *value);
            }
        }
    }
}

fn open_detail(ui: &AppWindow, state: &Rc<RefCell<State>>, id: i32) {
    {
        let mut st = state.borrow_mut();
        st.detail_id = id;
    }
    refresh_detail(ui, state);
    refresh_rows(ui, state);
    ui.set_drawer_open(true);
}

/// Show a transient M3 snackbar. The 200 ms engine timer hides it again.
fn show_toast(ui: &AppWindow, state: &Rc<RefCell<State>>, message: &str) {
    state.borrow_mut().toast_until = Some(Instant::now() + Duration::from_millis(2600));
    ui.set_toast_text(message.into());
    ui.set_toast_open(true);
}

fn theme_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var("APPDATA").ok()?;
    #[cfg(target_os = "macos")]
    let base = format!(
        "{}/Library/Application Support",
        std::env::var("HOME").ok()?
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var("XDG_CONFIG_HOME").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|home| format!("{home}/.config"))
    })?;
    Some(PathBuf::from(base).join("aether").join("theme"))
}

fn read_theme_mode() -> Option<i32> {
    let value = fs::read_to_string(theme_config_path()?).ok()?;
    match value.trim() {
        "0" => Some(0),
        "1" => Some(1),
        "2" => Some(2),
        _ => None,
    }
}

fn save_theme_mode(mode: i32) {
    let Some(path) = theme_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, mode.to_string());
}

/// UI-local preferences that have no daemon counterpart. Stored as JSON in
/// the same config directory as the theme file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct UiPrefs {
    /// Accent preset index, see `apply-appearance()` in ui/app.slint.
    accent: i32,
    /// Transitions enabled; false zeroes the motion tokens (accessibility).
    motion: bool,
    /// Decorative flow lines at the window edges.
    flow: bool,
    /// 0 = 主页, 1 = 下载列表, 2 = 上次页面.
    start_page: i32,
    /// Last visited page index, used when `start_page == 2`.
    last_page: i32,
    /// 0 = 不操作, 1 = 打开所在文件夹, 2 = 打开文件.
    after_done: i32,
    /// Desktop notification when a task completes.
    notify_done: bool,
    /// Desktop notification when a task fails (cancellations never notify).
    notify_fail: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            accent: 0,
            motion: true,
            flow: true,
            start_page: 0,
            last_page: 0,
            after_done: 0,
            notify_done: true,
            notify_fail: true,
        }
    }
}

impl UiPrefs {
    fn config_path() -> Option<PathBuf> {
        theme_config_path().map(|theme| theme.with_file_name("ui.json"))
    }

    fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(raw) = serde_json::to_string_pretty(self) else {
            return;
        };
        let _ = fs::write(path, raw);
    }
}

/// Reveal or open a path with the desktop's default handler. Used by the
/// "after done" action; failures are ignored (the download already finished).
fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/c", "start", ""]);
        command.arg(path);
        command
    };
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Best-effort desktop notification. Linux uses `notify-send` (libnotify);
/// macOS uses a one-line AppleScript; other platforms stay silent (honestly
/// noted in the README — a real toast API needs an extra dependency).
fn send_notification(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("notify-send");
        command.arg("Aether").arg(title).arg(body);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        // AppleScript string escaping: backslash and double quote.
        let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"Aether {}\"",
            escape(body),
            escape(title)
        );
        let mut command = Command::new("osascript");
        command.args(["-e", &script]);
        command
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Resolve the desktop's light/dark preference without pulling in a
/// platform-specific theme crate. Unknown systems fall back to light.
fn system_is_dark() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            let value = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if value.contains("prefer-dark") {
                return true;
            }
            if value.contains("prefer-light") {
                return false;
            }
            if value.contains("default") {
                if let Ok(theme) = Command::new("gsettings")
                    .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
                    .output()
                {
                    return String::from_utf8_lossy(&theme.stdout)
                        .to_lowercase()
                        .contains("dark");
                }
            }
        }
        if let Ok(theme) = std::env::var("GTK_THEME") {
            return theme.to_lowercase().contains("dark");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
        {
            return String::from_utf8_lossy(&output.stdout).contains("0x0");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            return String::from_utf8_lossy(&output.stdout).contains("Dark");
        }
    }

    false
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Bind the window to aether.desktop so the dock / task switcher uses the
    // app icon instead of a generic fallback. This must run before `run()`.
    let _ = slint::set_xdg_app_id("aether");

    let state = Rc::new(RefCell::new(State::new()));

    // Theme: follow the desktop by default; AETHER_THEME can pin it for
    // screenshots and development (`light` or `dark`).
    ui.set_system_dark(system_is_dark());
    let env_theme = match std::env::var("AETHER_THEME").as_deref() {
        Ok("light") => Some(1),
        Ok("dark") => Some(2),
        _ => None,
    };
    if let Some(mode) = env_theme.or_else(read_theme_mode) {
        ui.set_theme_mode(mode);
    }
    // UI-local preferences: push into the window (which applies accent /
    // motion / flow through apply-appearance) and pick the start page.
    {
        let prefs = state.borrow().prefs.clone();
        ui.set_settings_accent(prefs.accent);
        ui.set_settings_motion(prefs.motion);
        ui.set_settings_flow(prefs.flow);
        ui.set_settings_start_page(prefs.start_page);
        ui.set_settings_after_done(prefs.after_done);
        ui.set_settings_notify_done(prefs.notify_done);
        ui.set_settings_notify_fail(prefs.notify_fail);
        let page = match prefs.start_page {
            1 => 1,
            2 => prefs.last_page.clamp(0, 2),
            _ => 0,
        };
        ui.set_page(page);
        state.borrow_mut().seen_page = page;
    }

    // Initial data.
    refresh_rows(&ui, &state);

    // Screenshot helper: open a specific surface on startup so the mockup can
    // be captured in every state without driving the mouse.
    match std::env::var("AETHER_VIEW").as_deref() {
        Ok("settings") => ui.set_page(2),
        Ok("downloads") => ui.set_page(1),
        Ok("dialog") => ui.set_dialog_open(true),
        Ok("drawer") => {
            ui.set_page(1);
            // Prefer an active torrent: it exercises the chart and chunk grid.
            let id = {
                let st = state.borrow();
                let snapshot = st.backend.snapshot();
                snapshot
                    .tasks
                    .iter()
                    .find(|task| task.torrent && task.status == Status::Downloading)
                    .or_else(|| {
                        snapshot
                            .tasks
                            .iter()
                            .find(|task| task.status == Status::Downloading)
                    })
                    .map(|task| task.id)
                    .unwrap_or(-1)
            };
            if id >= 0 {
                open_detail(&ui, &state, id);
            }
        }
        _ => {}
    }

    // ---- task table actions ------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_task_action(move |id, action| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let message = {
                let mut st = state.borrow_mut();
                let snapshot = st.backend.snapshot();
                let was_failed = snapshot
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.status == Status::Failed)
                    .unwrap_or(false);
                let existed = snapshot.tasks.iter().any(|task| task.id == id);
                st.backend.apply_action(id, action);
                if action == 2 && st.detail_id == id {
                    st.detail_id = -1;
                }
                match action {
                    1 if was_failed => Some("失败任务已重新开始下载"),
                    2 if existed => Some("任务已从列表中移除"),
                    3 if existed => Some("已在文件管理器中定位到文件"),
                    _ => None,
                }
            };
            let closed = state.borrow().detail_id < 0;
            if closed {
                ui.set_drawer_open(false);
            }
            refresh_rows(&ui, &state);
            if let Some(message) = message {
                show_toast(&ui, &state, message);
            }
        });
    }

    // ---- open detail drawer ------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_open_detail(move |id| {
            if let Some(ui) = ui_handle.upgrade() {
                open_detail(&ui, &state, id);
            }
        });
    }

    // ---- close detail drawer -----------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_drawer_dismissed(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            state.borrow_mut().detail_id = -1;
            refresh_rows(&ui, &state);
        });
    }

    // ---- filter / search ---------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_filter_changed(move |filter, query| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = filter;
                st.query = query.to_string();
            }
            refresh_rows(&ui, &state);
        });
    }

    // ---- reset filters -----------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_reset_filters(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query.clear();
            }
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
        });
    }

    // ---- new download ------------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_new_download(move |url, path, _segments, limit_kb, _note| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query = String::new();
                st.backend.add_task(&url, &path, limit_kb);
            }
            ui.set_page(1);
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
            show_toast(&ui, &state, "已添加下载任务");
        });
    }

    // ---- settings ----------------------------------------------------------
    {
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
            let mut hint: Option<String> = None;
            {
                let mut st = state.borrow_mut();
                match key.as_str() {
                    "proxy-url" => {
                        st.proxy_url = value.to_string();
                        // Editing the address after choosing 手动 applies it.
                        if st.proxy_mode == "手动" && !st.proxy_url.trim().is_empty() {
                            st.backend.set_proxy("手动", &st.proxy_url);
                        }
                    }
                    "proxy-mode" => {
                        st.proxy_mode = value.to_string();
                        match st.proxy_mode.as_str() {
                            // Display-only entry: the PAC URL lives in the
                            // daemon's start configuration, not in settings.
                            "PAC" => {
                                hint = Some(
                                    "PAC 由启动配置（AETHER_PAC_URL）决定，暂不支持在界面切换"
                                        .into(),
                                )
                            }
                            "手动" if st.proxy_url.trim().is_empty() => {
                                hint = Some("请先填写代理地址，再切换到手动模式".into())
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
                        st.backend
                            .set_setting(serde_json::json!({"dns": value.as_str()}));
                        hint = Some("DNS 已保存：空闲插件重启后生效".into());
                    }
                    "user-agent" => {
                        st.backend
                            .set_setting(serde_json::json!({"user_agent": value.as_str()}));
                        hint = Some("User-Agent 已保存：空闲插件重启后生效".into());
                    }
                    "ip-mode" => {
                        let patch = match value.as_str() {
                            "仅 IPv4" => serde_json::json!({"ip_mode": "ipv4_only"}),
                            "仅 IPv6" => serde_json::json!({"ip_mode": "ipv6_only"}),
                            _ => serde_json::json!({"ip_mode": null}),
                        };
                        st.backend.set_setting(patch);
                        hint = Some("IP 协议已保存：空闲插件重启后生效".into());
                    }
                    // Network extras and BT knobs: same set_settings channel.
                    "headers" => {
                        st.backend
                            .set_setting(serde_json::json!({"default_headers": value.as_str()}));
                        hint = Some("请求头已保存：空闲插件重启后生效".into());
                    }
                    "ftp-passive" => {
                        st.backend.set_setting(serde_json::json!({
                            "ftp_passive": value.as_str() == "true"
                        }));
                        hint = Some("FTP 模式已保存：空闲插件重启后生效".into());
                    }
                    "bt-port" => match value.trim().parse::<u16>() {
                        Ok(0) | Err(_) => hint = Some("监听端口需要 1-65535 的数字".into()),
                        Ok(port) => {
                            st.backend
                                .set_setting(serde_json::json!({"bt_listen_port": port}));
                            hint = Some("BT 端口已保存：空闲时重启生效".into());
                        }
                    },
                    "bt-dht" => {
                        st.backend.set_setting(serde_json::json!({
                            "bt_dht": value.as_str() == "true"
                        }));
                        hint = Some("DHT 已保存：空闲时重启生效".into());
                    }
                    "bt-upload-limit" => {
                        // Slider reports KB/s; 0 is the stored form of "no cap".
                        let kbps = value.trim().parse::<f64>().unwrap_or(0.0).max(0.0);
                        let bps = (kbps * 1024.0) as u64;
                        st.backend
                            .set_setting(serde_json::json!({"bt_upload_bps": bps}));
                        hint = Some("上传限速已保存：对新任务与重启后的做种生效".into());
                    }
                    "bt-blocklist" => {
                        st.backend
                            .set_setting(serde_json::json!({"bt_blocklist": value.as_str()}));
                        hint = Some("屏蔽列表已保存：空闲时重启生效".into());
                    }
                    "bt-trackers" => {
                        st.backend
                            .set_setting(serde_json::json!({"bt_trackers": value.as_str()}));
                        hint = Some("Tracker 已保存：附加到之后的任务".into());
                    }
                    "schedule-on" => {
                        let enabled = value.as_str() == "true";
                        let (start, end) = {
                            let ui = ui_handle.upgrade();
                            (
                                ui.as_ref()
                                    .map(|ui| ui.get_settings_schedule_start())
                                    .unwrap_or_default()
                                    .to_string(),
                                ui.as_ref()
                                    .map(|ui| ui.get_settings_schedule_end())
                                    .unwrap_or_default()
                                    .to_string(),
                            )
                        };
                        if enabled && (start.trim().is_empty() || end.trim().is_empty()) {
                            // Revert the switch: the daemon never heard about it.
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_schedule_on(false);
                            }
                            hint = Some("请先填写开始与结束时间".into());
                        } else {
                            st.backend
                                .set_setting(serde_json::json!({"schedule_on": enabled}));
                            hint = Some(if enabled {
                                "计划已开启：时段外自动暂停队列".into()
                            } else {
                                "计划已关闭：队列不再按时间暂停".into()
                            });
                        }
                    }
                    "schedule-start" | "schedule-end" => {
                        let key = if key.as_str() == "schedule-start" {
                            "schedule_start"
                        } else {
                            "schedule_end"
                        };
                        st.backend
                            .set_setting(serde_json::json!({key: value.as_str()}));
                        hint = Some("时间已保存（HH:MM）".into());
                    }
                    // Scheduler limits go through the same set_settings RPC;
                    // the daemon validates them and applies them to the
                    // running queue. Tasks already downloading keep the
                    // budget they started with.
                    "max-tasks" => match value.trim().parse::<u64>() {
                        Ok(number) => {
                            st.backend
                                .set_setting(serde_json::json!({"max_concurrent": number}));
                            hint = Some("并发上限已保存：排队任务按新上限启动".into());
                        }
                        Err(_) => hint = Some(format!("并发上限「{value}」无效")),
                    },
                    "max-connections" => match value.trim().parse::<u64>() {
                        Ok(number) => {
                            st.backend
                                .set_setting(serde_json::json!({"connections": number}));
                            hint = Some("每任务连接数已保存：新任务生效（HTTP 上限 16）".into());
                        }
                        Err(_) => hint = Some(format!("每任务连接数「{value}」无效")),
                    },
                    "retry-count" => {
                        let retries = match value.as_str() {
                            "不重试" => Some(0_u64),
                            other => other.trim().parse::<u64>().ok(),
                        };
                        match retries {
                            Some(number) => {
                                st.backend
                                    .set_setting(serde_json::json!({"retry_max": number}));
                                hint = Some(if number == 0 {
                                    "已关闭自动重试".into()
                                } else {
                                    "重试上限已保存：下次失败时生效".into()
                                });
                            }
                            None => hint = Some(format!("重试次数「{value}」无效")),
                        }
                    }
                    // One knob, two controls: the switch sends "no limit"
                    // (0), the slider sends the rate in bytes per second.
                    "global-limit" => {
                        st.global_limit = value == "true";
                        let bps = if st.global_limit {
                            (st.download_limit_kb * 1024.0) as u64
                        } else {
                            0
                        };
                        st.backend
                            .set_setting(serde_json::json!({"max_speed_bps": bps}));
                        hint = Some(if st.global_limit {
                            format!("全局限速已保存：{} KB/s，新任务生效", st.download_limit_kb)
                        } else {
                            "已关闭全局限速".into()
                        });
                    }
                    "download-limit" => match value.trim().parse::<f64>() {
                        Ok(kb) if kb >= 0.0 => {
                            st.download_limit_kb = kb;
                            if st.global_limit {
                                st.backend.set_setting(
                                    serde_json::json!({"max_speed_bps": (kb * 1024.0) as u64}),
                                );
                                hint = Some(format!("下载限速已保存：{kb} KB/s，新任务生效"));
                            } else {
                                hint = Some("已记录限速，启用全局限速后生效".into());
                            }
                        }
                        _ => hint = Some(format!("限速值「{value}」无效")),
                    },
                    // Persisted locally, no daemon round-trip.
                    "theme" => {}
                    // UI-local preferences: persist and apply immediately.
                    "accent" => {
                        let index = match value.as_str() {
                            "靛蓝" => 1,
                            "青碧" => 2,
                            "苔绿" => 3,
                            "琥珀" => 4,
                            "蔷薇" => 5,
                            _ => 0,
                        };
                        st.prefs.accent = index;
                        st.prefs.save();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_accent(index);
                        }
                    }
                    "motion" | "flow-background" => {
                        let enabled = value.as_str() == "true";
                        if key.as_str() == "motion" {
                            st.prefs.motion = enabled;
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_motion(enabled);
                            }
                        } else {
                            st.prefs.flow = enabled;
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_flow(enabled);
                            }
                        }
                        st.prefs.save();
                    }
                    "start-page" => {
                        st.prefs.start_page = match value.as_str() {
                            "下载列表" => 1,
                            "上次页面" => 2,
                            _ => 0,
                        };
                        st.prefs.save();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_start_page(st.prefs.start_page);
                        }
                    }
                    "after-done" => {
                        st.prefs.after_done = match value.as_str() {
                            "打开所在文件夹" => 1,
                            "打开文件" => 2,
                            _ => 0,
                        };
                        st.prefs.save();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_after_done(st.prefs.after_done);
                        }
                    }
                    "notify-done" | "notify-fail" => {
                        let enabled = value.as_str() == "true";
                        if key.as_str() == "notify-done" {
                            st.prefs.notify_done = enabled;
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_notify_done(enabled);
                            }
                        } else {
                            st.prefs.notify_fail = enabled;
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_notify_fail(enabled);
                            }
                        }
                        st.prefs.save();
                    }
                    "reset-settings" => {
                        st.prefs = UiPrefs::default();
                        st.prefs.save();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_accent(0);
                            ui.set_settings_motion(true);
                            ui.set_settings_flow(true);
                            ui.set_settings_start_page(0);
                            ui.set_settings_after_done(0);
                            ui.set_settings_notify_done(true);
                            ui.set_settings_notify_fail(true);
                        }
                        hint = Some("界面偏好已恢复默认".into());
                    }
                    // The settings page still shows controls that do nothing;
                    // saying so beats a silent no-op (honest UI).
                    other => hint = Some(format!("设置项「{other}」尚未接线")),
                }
            }
            if let (Some(hint), Some(ui)) = (hint, ui_handle.upgrade()) {
                show_toast(&ui, &state, &hint);
            }
            println!("[settings] {key} = {value}");
        });
    }

    // ---- plugin toggles go to the daemon ------------------------------------
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
                if enabled {
                    format!("已请求启用 {name}，有运行任务时会拒绝")
                } else {
                    format!("已请求停用 {name}")
                }
                .as_str(),
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
            let message = match action.as_str() {
                "install" => "插件安装器尚未接线",
                "update-all" => "更新检查尚未接线",
                "reload" => "daemon 重启时会重新扫描插件目录",
                _ => "该操作尚未接线",
            };
            show_toast(&ui, &state, message);
        });
    }

    // ---- speed limit pills (global throttle is not wired yet) --------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_speed_limit_picked(move |mode| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let name = match mode {
                1 => "低",
                2 => "中",
                3 => "高",
                _ => "不限",
            };
            show_toast(&ui, &state, &format!("全局限速「{name}」尚未接线"));
        });
    }

    // ---- support actions ---------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_support_action(move |key| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let message = match key.as_str() {
                "docs" => "文档 · docs.aether.dev（原型）",
                "community" => "社区 · forum.aether.dev（原型）",
                "feedback" => "反馈 · github.com/pengtang1024/aether/issues（原型）",
                "update" => "已检查更新：当前已是最新版本 0.1.0",
                "channel" => "更新通道已更新",
                "auto-update" => "自动更新设置已保存",
                "changelog" => "版本说明 · 0.1.0（原型）",
                "license" => "MIT License · Slint Royalty-free 2.0",
                _ => "操作已记录",
            };
            show_toast(&ui, &state, message);
        });
    }

    // ---- simulation clock (5 Hz) ------------------------------------------
    let timer = Timer::default();
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(200), move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let drawer_open = ui.get_drawer_open();
            let (expire_toast, notice) = {
                let mut st = state.borrow_mut();
                let drift = st.backend.drift();
                ui.set_drift(drift);
                let notice = st.backend.take_notice().filter(|notice| {
                    if st.last_notice.as_deref() == Some(notice.as_str()) {
                        false
                    } else {
                        st.last_notice = Some(notice.clone());
                        true
                    }
                });
                let expire_toast = st.toast_until.is_some_and(|until| Instant::now() >= until);
                if expire_toast {
                    st.toast_until = None;
                }
                (expire_toast, notice)
            };
            if expire_toast {
                ui.set_toast_open(false);
            }
            if let Some(notice) = notice {
                show_toast(&ui, &state, &notice);
            }
            refresh_rows(&ui, &state);
            if drawer_open {
                refresh_detail(&ui, &state);
            }
            // Persist the page for "上次页面"; run the after-done action for
            // tasks that completed since the last tick.
            {
                let mut st = state.borrow_mut();
                let page = ui.get_page();
                if page != st.seen_page {
                    st.seen_page = page;
                    st.prefs.last_page = page;
                    st.prefs.save();
                }
                let mut done = Vec::new();
                st.backend.take_finished(&mut done);
                let after = st.prefs.after_done;
                let notify_done = st.prefs.notify_done;
                let notify_fail = st.prefs.notify_fail;
                drop(st);
                for item in done {
                    if item.done {
                        if notify_done {
                            send_notification("下载完成", &format!("「{}」已下载完成", item.name));
                        }
                        match after {
                            1 => {
                                if let Some(dir) = &item.dir {
                                    open_path(std::path::Path::new(dir));
                                }
                            }
                            2 => {
                                if let (Some(dir), Some(file)) = (&item.dir, &item.file) {
                                    open_path(&std::path::Path::new(dir).join(file));
                                }
                            }
                            _ => {}
                        }
                    } else if notify_fail {
                        let detail = item.error.as_deref().unwrap_or_default();
                        let body = if detail.is_empty() {
                            format!("「{}」下载失败", item.name)
                        } else {
                            format!("「{}」下载失败：{detail}", item.name)
                        };
                        send_notification("下载失败", &body);
                    }
                }
            }
        });
    }

    // Follow live desktop theme changes while the user chose "follow system".
    let theme_timer = Timer::default();
    {
        let ui_handle = ui.as_weak();
        theme_timer.start(TimerMode::Repeated, Duration::from_secs(10), move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            if ui.get_theme_mode() == 0 {
                let dark = system_is_dark();
                if ui.get_system_dark() != dark {
                    ui.set_system_dark(dark);
                }
            }
        });
    }

    ui.run()
}
