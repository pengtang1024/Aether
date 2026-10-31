//! Aether — a native Material 3 download manager.
//!
//! Rust bridges the Slint window to `aether-daemon` (`src/backend.rs`); Slint
//! renders everything with its own Material 3 widget set. There is no WebView
//! anywhere in the stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod autostart;
mod backend;
mod chart;
mod clipboard;
mod i18n;
mod instance;
mod launch;
mod model;
mod tray;

slint::include_modules!();

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Model, ModelRc, Timer, TimerMode, VecModel};

use aether_daemon::settings::{generate_rpc_secret, RPC_LISTEN_DEFAULT};
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
    /// Clipboard link watcher: `None` when the session has no clipboard
    /// backend (the settings row says so instead of faking availability).
    clipboard: Option<clipboard::ClipboardWatch>,
    /// Flips the poller from the settings page.
    clipboard_control: Option<clipboard::ClipboardControl>,
    /// URL staged by the latest detection; the toast action opens it.
    clipboard_url: Option<String>,
    /// A user-triggered update check that has not reported its result yet.
    update_watch: Option<UpdateWatch>,
}

/// Tracks one on-demand update check so the UI can toast the final state
/// instead of leaving "checking…" hanging forever.
struct UpdateWatch {
    since: Instant,
    saw_checking: bool,
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
            clipboard: None,
            clipboard_control: None,
            clipboard_url: None,
            update_watch: None,
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
        "http" => i18n::tr(
            "HTTP / HTTPS 多线程下载",
            "Multi-threaded HTTP/HTTPS downloads",
        ),
        "bt" => i18n::tr(
            "BT / 磁力链下载与做种",
            "BitTorrent / magnet downloads and seeding",
        ),
        "ftp" => i18n::tr("FTP / FTPS 站点传输", "FTP / FTPS transfers"),
        "sftp" => i18n::tr("SFTP / SSH 文件传输", "SFTP / SSH file transfers"),
        "media" => i18n::tr(
            "网页音视频嗅探（HLS / DASH）",
            "Web video streams (HLS / DASH)",
        ),
        "extractor" => i18n::tr("yt-dlp 视频站点支持", "Video sites via yt-dlp"),
        "rtmp" => i18n::tr("RTMP 直播流录制", "RTMP live stream capture"),
        "webdav" => i18n::tr("WebDAV 远程目录", "Remote WebDAV directories"),
        "s3" => i18n::tr("S3 / 对象存储", "S3 / object storage"),
        "gdrive" => i18n::tr("Google Drive 分享链接", "Google Drive share links"),
        "onedrive" => i18n::tr("OneDrive 分享链接", "OneDrive share links"),
        "dropbox" => i18n::tr("Dropbox 分享链接", "Dropbox share links"),
        "baidupan" => i18n::tr("百度网盘（官方 API）", "Baidu Netdisk (official API)"),
        "aliyunpan" => i18n::tr("阿里云盘（官方 API）", "Aliyun Drive (official API)"),
        "thunder" => i18n::tr("迅雷链接", "Thunder links"),
        "ipfs" => i18n::tr("IPFS / IPNS 内容", "IPFS / IPNS content"),
        "aria2" => i18n::tr(
            "aria2 JSON-RPC 兼容（HTTP）",
            "aria2 JSON-RPC compatibility (HTTP)",
        ),
        other => {
            if info.protocols.is_empty() {
                i18n::tr(format!("协议：{other}"), format!("Protocol: {other}"))
            } else {
                i18n::tr(
                    format!("协议：{}", info.protocols.join(", ")),
                    format!("Protocols: {}", info.protocols.join(", ")),
                )
            }
        }
    }
}

/// Capability labels shown in the plugin catalogue. These are the manifest
/// contract, so they matter to users: "external process" means the plugin may
/// run yt-dlp/ffmpeg, and the daemon can refuse that class outright.
fn plugin_caps(info: &PluginInfo) -> String {
    if info.capabilities.is_empty() {
        return i18n::tr("无", "None");
    }
    info.capabilities
        .iter()
        .map(|capability| match capability.as_str() {
            "network" => i18n::tr("网络", "Network"),
            "filesystem:task" => i18n::tr("任务目录", "Task files"),
            "external-process" => i18n::tr("外部进程", "External process"),
            "clipboard" => i18n::tr("剪贴板", "Clipboard"),
            "http3" => "HTTP/3".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Render the daemon's `plugin_pins` map as the text field's
/// `id=version; id=version` form (sorted, so a refresh does not shuffle it).
fn format_plugin_pins(value: &serde_json::Value) -> String {
    let Some(pins) = value.as_object() else {
        return String::new();
    };
    let mut entries = pins
        .iter()
        .filter_map(|(id, version)| version.as_str().map(|version| format!("{id}={version}")))
        .collect::<Vec<_>>();
    entries.sort();
    entries.join("; ")
}

/// Parse the pin text field. Validation of ids/semver happens in the daemon
/// (`set_settings`); this only rejects entries without an `=`.
fn parse_plugin_pins(text: &str) -> Result<serde_json::Value, String> {
    let mut pins = serde_json::Map::new();
    for entry in text.split([';', '\n']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (id, version) = entry.split_once('=').ok_or_else(|| {
            i18n::tr(
                format!("条目缺少 =：{entry}"),
                format!("Missing = in {entry}"),
            )
        })?;
        let (id, version) = (id.trim(), version.trim());
        if id.is_empty() || version.is_empty() {
            return Err(i18n::tr(
                format!("条目不完整：{entry}"),
                format!("Incomplete entry: {entry}"),
            ));
        }
        pins.insert(
            id.to_string(),
            serde_json::Value::String(version.to_string()),
        );
    }
    Ok(serde_json::Value::Object(pins))
}

/// Catalogue grouping; mirrors the filter tabs (全部/内置/已启用).
fn plugin_category(id: &str) -> String {
    match id {
        "http" => "HTTP".into(),
        "bt" => "BT".into(),
        "ftp" | "sftp" => i18n::tr("传输", "Transfer"),
        "media" | "extractor" | "rtmp" => i18n::tr("媒体", "Media"),
        "webdav" | "s3" | "ipfs" => i18n::tr("存储", "Storage"),
        "gdrive" | "onedrive" | "dropbox" | "baidupan" | "aliyunpan" | "thunder" => {
            i18n::tr("网盘", "Cloud drive")
        }
        "aria2" => "RPC".into(),
        _ => i18n::tr("其他", "Other"),
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
        "thunder" | "aria2" => 11, // signal
        "ipfs" => 9,               // layers
        _ => 6,                    // list
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
    // Windows keeps the Chrome-family manifest path in HKCU; Firefox uses a
    // plain file under %APPDATA%.
    #[cfg(windows)]
    {
        let registry = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Google\Chrome\NativeMessagingHosts\com.pengtang1024.aether",
            ])
            .output();
        if registry.ok().is_some_and(|out| out.status.success()) {
            return true;
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let manifest = PathBuf::from(appdata)
                .join("Mozilla")
                .join("NativeMessagingHosts")
                .join("com.pengtang1024.aether.json");
            if manifest.is_file() {
                return true;
            }
        }
        return false;
    }
    #[cfg(not(windows))]
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    #[cfg(not(windows))]
    let home = PathBuf::from(home);
    #[cfg(not(windows))]
    {
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
}

fn to_row(task: &model::Task, detail_id: i32) -> TaskRow {
    let status = task.status;
    let eta = match status {
        Status::Downloading => format_eta(task.remaining_seconds()),
        Status::Failed => i18n::tr("校验失败", "Checksum failed"),
        Status::Paused => i18n::tr("已暂停", "Paused"),
        Status::Queued => i18n::tr("等待开始", "Waiting to start"),
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
            ui.set_disk_percent_text(format!("{}%", (percent * 100.0).round() as i32).into());
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
    let update_text = match snapshot.stats.as_ref() {
        Some(stats) => update_status_text(stats.update.as_ref()),
        None => i18n::tr("daemon 未连接", "Daemon is not connected"),
    };
    ui.set_settings_update_status(update_text.into());

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
                caps: plugin_caps(info).into(),
                version: info.version.clone().into(),
                category: plugin_category(&info.id).into(),
                kind: 0,
                enabled: info.enabled,
                blocked: info.blocked,
                update: false,
                featured: false,
                can_rollback: info.has_backup,
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
        chunks_text: i18n::tr(
            format!("{done_chunks} / {CHUNK_COUNT} 个分片已完成"),
            format!("{done_chunks} / {CHUNK_COUNT} chunks done"),
        )
        .into(),
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
    ui.set_toast_action("".into());
    ui.set_toast_open(true);
}

/// Snackbar with a click-through action (clipboard detection's "download"
/// offer). The action payload lives in `State`, keyed by this toast.
fn show_action_toast(ui: &AppWindow, state: &Rc<RefCell<State>>, message: &str, action: &str) {
    state.borrow_mut().toast_until = Some(Instant::now() + Duration::from_millis(7000));
    ui.set_toast_text(message.into());
    ui.set_toast_action(action.into());
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
    /// Watch the clipboard for a fresh download URL and offer a one-click
    /// toast. Off by default: clipboard access is opt-in.
    clipboard_watch: bool,
    /// Interface language: "zh" (Chinese source, default) or "en"
    /// (translations/en/LC_MESSAGES/aether.po).
    lang: String,
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
            clipboard_watch: false,
            lang: "zh".into(),
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
fn open_path(path: &std::path::Path) -> bool {
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
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Open an http(s) address with the desktop's default handler. Used by the
/// About panel; returns false when no opener could be started.
fn open_url(url: &str) -> bool {
    open_path(std::path::Path::new(url))
}

/// One line describing the daemon's persisted RPC configuration. The
/// listener only rebinds at daemon start, so the wording avoids claiming
/// the address is live right now.
fn rpc_status_text(listen: &str, has_secret: bool) -> String {
    let listen = listen.trim();
    if listen.is_empty() {
        return i18n::tr("远程控制未开启", "Remote control is off");
    }
    if has_secret {
        i18n::tr(
            format!("已配置 {listen}，需要令牌（重启 daemon 后按此地址监听）"),
            format!(
                "Configured on {listen} with a token (the daemon listens there after a restart)"
            ),
        )
    } else {
        i18n::tr(
            format!("已配置 {listen}，无令牌（仅回环地址允许；重启 daemon 后生效）"),
            format!("Configured on {listen} without a token (loopback only; applies after a daemon restart)"),
        )
    }
}

/// Human-readable app-update state for the About panel. `None` means the
/// daemon has no update source configured at all.
fn update_status_text(update: Option<&serde_json::Value>) -> String {
    let Some(update) = update else {
        return i18n::tr(
            "未配置更新源（daemon 需要 --update-url 与更新公钥）",
            "No update source (the daemon needs --update-url and the public key)",
        );
    };
    let latest = update["latest"].as_str().unwrap_or_default();
    let error = update["error"].as_str().unwrap_or_default();
    match update["status"].as_str().unwrap_or_default() {
        "checking" => i18n::tr("正在检查更新…", "Checking for updates…"),
        "idle" => i18n::tr(
            "等待首次后台检查（每 6 小时一次）",
            "Waiting for the first background check (every 6 hours)",
        ),
        "up_to_date" => i18n::tr(
            format!("已是最新版本 {}", env!("CARGO_PKG_VERSION")),
            format!("Up to date ({})", env!("CARGO_PKG_VERSION")),
        ),
        "available" => i18n::tr(
            format!("发现新版本 {latest}，自动下载已关闭"),
            format!("Version {latest} is available; automatic downloads are off"),
        ),
        "ready" => i18n::tr(
            format!("新版本 {latest} 已下载，重启后生效"),
            format!("Version {latest} downloaded; restart to apply"),
        ),
        "error" => i18n::tr(
            format!("检查失败：{error}"),
            format!("Check failed: {error}"),
        ),
        _ => i18n::tr("更新状态未知", "Unknown update state"),
    }
}

/// Best-effort desktop notification. Linux uses `notify-send` (libnotify),
/// macOS a one-line AppleScript, Windows a WinRT toast through notify-rust
/// (the one platform whose toast API is not reachable via an external
/// command). Every path tolerates refusal: at most one log line, no panic.
fn send_notification(title: &str, body: &str) {
    #[cfg(windows)]
    {
        let mut notification = notify_rust::Notification::new();
        notification.summary(title).body(body);
        // Toasts are keyed by an AppUserModelID. Register ours under HKCU
        // on first use and attribute the toast to Aether; if registration
        // is refused, leave notify-rust's own fallback in place —
        // misattributed beats missing.
        if ensure_app_id() {
            notification.app_id(AUMID);
        }
        if let Err(error) = notification.show() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!("aether: windows toast failed ({error})");
            }
        }
        return;
    }
    #[cfg(not(windows))]
    {
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
}

/// AppUserModelID Windows toasts are attributed to. Registered under HKCU
/// on first use (see [`ensure_app_id`]) — no installer step, no admin.
#[cfg(windows)]
const AUMID: &str = "com.pengtang1024.Aether";

/// Register [`AUMID`] once per process; `true` = Aether attribution is
/// available. `reg add` creates the key itself, and HKCU needs no admin —
/// the same external-command philosophy as notify-send/osascript above.
/// A refusal is cached too: notifications keep working (notify-rust falls
/// back to its own registered id) without re-running reg every toast.
#[cfg(windows)]
fn ensure_app_id() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    /// 0 = untried, 1 = registered, 2 = refused.
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{AUMID}");
    let registered = reg_add(&key, "DisplayName", "Aether");
    if registered {
        // Icon beside the exe when packaged; cosmetic, so a failure (or a
        // dev tree without logo.ico) changes nothing.
        if let Ok(icon) = std::env::current_exe().map(|exe| exe.with_file_name("logo.ico")) {
            if icon.is_file() {
                let _ = reg_add(&key, "IconUri", &icon.to_string_lossy());
            }
        }
    }
    STATE.store(if registered { 1 } else { 2 }, Ordering::Relaxed);
    registered
}

/// One `reg add` write; `false` when reg.exe is unavailable or refuses.
#[cfg(windows)]
fn reg_add(key: &str, value: &str, data: &str) -> bool {
    std::process::Command::new("reg")
        .args(["add", key, "/v", value, "/t", "REG_SZ", "/d", data, "/f"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
    // The autostart entries pass `--silent`, so a login launch stays in the
    // tray without stealing focus. Manual launches have no flag and show the
    // window; the request is still gated on a usable tray further down.
    let silent = launch::requested_silent(
        std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned()),
    );

    // One UI per user: a second launch would race for the daemon and the
    // tray, so it reports and exits. The guard lives for the whole process
    // (dropping it releases the lock).
    let _instance = match instance::acquire() {
        instance::Acquired::AlreadyRunning => {
            send_notification(
                &i18n::tr("Aether 已在运行", "Aether is already running"),
                &i18n::tr(
                    "请使用已经打开的窗口",
                    "Use the window that is already open",
                ),
            );
            return Ok(());
        }
        other => other,
    };

    // Keep the login entry we own healthy: an update may have moved the
    // binary and entries written before this release lack `--silent`. A
    // missing entry stays missing — deleting it is the user's off switch.
    autostart::refresh();

    let ui = AppWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Bind the window to aether.desktop so the dock / task switcher uses the
    // app icon instead of a generic fallback. This must run before `run()`.
    let _ = slint::set_xdg_app_id("aether");

    let state = Rc::new(RefCell::new(State::new()));
    // Clipboard link watch: start the poller (None = no backend in this
    // session; the settings row reports that instead of faking a switch).
    {
        let mut st = state.borrow_mut();
        if let Some((watch, control)) = clipboard::start() {
            control.set_enabled(st.prefs.clipboard_watch);
            st.clipboard = Some(watch);
            st.clipboard_control = Some(control);
        }
    }

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
    // Rust-side strings follow UiPrefs.lang; must precede any translated
    // push (extension status, refresh labels) below.
    i18n::set_language(state.borrow().prefs.lang.as_str());

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
        ui.set_settings_clipboard(prefs.clipboard_watch);
        ui.set_settings_clipboard_available(state.borrow().clipboard_control.is_some());
        // Start-at-login lives in the OS, not in ui.json: show the real
        // registration instead of a remembered preference.
        ui.set_settings_autostart(autostart::is_enabled());
        let page = match prefs.start_page {
            1 => 1,
            2 => prefs.last_page.clamp(0, 2),
            _ => 0,
        };
        ui.set_page(page);
        state.borrow_mut().seen_page = page;
        // Chinese is the source language (index 0, no .po needed); "en"
        // selects the bundled translation. Runs before the first render
        // so the window never flashes the wrong language.
        if let Err(error) =
            slint::select_bundled_translation(if prefs.lang == "zh" { "" } else { &prefs.lang })
        {
            eprintln!("aether: language selection failed: {error:?}");
        }
        ui.set_settings_language(i18n::index_of(&prefs.lang) as i32);
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
                    1 if was_failed => {
                        Some(i18n::tr("失败任务已重新开始下载", "Failed task restarted"))
                    }
                    2 if existed => {
                        Some(i18n::tr("任务已从列表中移除", "Task removed from the list"))
                    }
                    3 if existed => Some(i18n::tr(
                        "已在文件管理器中定位到文件",
                        "Revealed in the file manager",
                    )),
                    _ => None,
                }
            };
            let closed = state.borrow().detail_id < 0;
            if closed {
                ui.set_drawer_open(false);
            }
            refresh_rows(&ui, &state);
            if let Some(message) = message {
                show_toast(&ui, &state, &message);
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
            let urls = backend::split_urls(&url);
            let count = urls.len();
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query = String::new();
                for url in &urls {
                    st.backend.add_task(url, &path, limit_kb);
                }
            }
            ui.set_page(1);
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
            // One URL is the plain case; a pasted batch reports how many
            // tasks were queued (per-URL failures arrive as daemon notices
            // afterwards).
            let message = if count > 1 {
                i18n::tr(
                    format!("已添加 {count} 个下载任务"),
                    format!("{count} downloads queued"),
                )
            } else {
                i18n::tr("已添加下载任务", "Download queued")
            };
            show_toast(&ui, &state, &message);
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
                        let previous = st.proxy_mode.clone();
                        st.proxy_mode = value.to_string();
                        match st.proxy_mode.as_str() {
                            // PAC needs a URL from the daemon's start
                            // configuration. Bounce the combo box back so it
                            // never shows a mode the daemon is not using.
                            "PAC" if previous != "PAC" => {
                                st.proxy_mode = previous;
                                if let Some(ui) = ui_handle.upgrade() {
                                    let index = match st.proxy_mode.as_str() {
                                        "直连" => 1,
                                        "手动" => 2,
                                        "PAC" => 3,
                                        _ => 0,
                                    };
                                    ui.set_settings_proxy_mode(index);
                                }
                                hint = Some(
                                    i18n::tr("PAC 由启动配置（AETHER_PAC_URL）决定，请用启动参数切换", "PAC comes from the launch configuration (AETHER_PAC_URL); switch it with launch options")
                                        .into(),
                                )
                            }
                            "手动" if st.proxy_url.trim().is_empty() => {
                                hint = Some(i18n::tr("请先填写代理地址，再切换到手动模式", "Enter the proxy address before switching to manual mode").into())
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
                        hint = Some(i18n::tr("DNS 已保存：空闲插件重启后生效", "DNS saved; idle plugins pick it up after a restart").into());
                    }
                    "user-agent" => {
                        st.backend
                            .set_setting(serde_json::json!({"user_agent": value.as_str()}));
                        hint = Some(i18n::tr("User-Agent 已保存：空闲插件重启后生效", "User-Agent saved; idle plugins pick it up after a restart").into());
                    }
                    "ip-mode" => {
                        let patch = match value.as_str() {
                            "仅 IPv4" => serde_json::json!({"ip_mode": "ipv4_only"}),
                            "仅 IPv6" => serde_json::json!({"ip_mode": "ipv6_only"}),
                            _ => serde_json::json!({"ip_mode": null}),
                        };
                        st.backend.set_setting(patch);
                        hint = Some(i18n::tr("IP 协议已保存：空闲插件重启后生效", "IP protocol saved; idle plugins pick it up after a restart").into());
                    }
                    // Network extras and BT knobs: same set_settings channel.
                    "headers" => {
                        st.backend
                            .set_setting(serde_json::json!({"default_headers": value.as_str()}));
                        hint = Some(i18n::tr("请求头已保存：空闲插件重启后生效", "Custom headers saved; idle plugins pick it up after a restart").into());
                    }
                    "ffmpeg-path" => {
                        st.backend
                            .set_setting(serde_json::json!({"ffmpeg_path": value.as_str()}));
                        hint = Some(i18n::tr("ffmpeg 路径已保存：对之后开始的任务生效", "ffmpeg path saved; applies to tasks started afterwards").into());
                    }
                    "plugin-auto-update" => {
                        let enabled = value.as_str() == "true";
                        st.backend
                            .set_setting(serde_json::json!({"plugin_auto_update": enabled}));
                        hint = Some(if enabled {
                            i18n::tr("已开启：daemon 每 6 小时检查并安装插件更新", "On; the daemon checks and installs plugin updates every 6 hours").into()
                        } else {
                            i18n::tr("已关闭自动更新插件", "Automatic plugin updates are off").into()
                        });
                    }
                    "plugin-external-process" => {
                        let allowed = value.as_str() == "true";
                        st.backend
                            .set_setting(serde_json::json!({"plugin_external_process": allowed}));
                        hint = Some(if allowed {
                            i18n::tr(
                                "已允许外部进程插件启动",
                                "Plugins that need an external process may start",
                            )
                            .into()
                        } else {
                            i18n::tr(
                                "已阻止外部进程插件：空闲实例已停止，运行中的任务当前进程跑完",
                                "External-process plugins are blocked; idle instances stopped, running tasks finish their process",
                            )
                            .into()
                        });
                    }
                    "plugin-pins" => match parse_plugin_pins(value.as_str()) {
                        Ok(pins) => {
                            st.backend
                                .set_setting(serde_json::json!({"plugin_pins": pins}));
                            hint = Some(i18n::tr(
                                "版本固定已保存：更新检查会跳过这些插件",
                                "Pins saved; update checks skip these plugins",
                            ).into());
                        }
                        Err(error) => hint = Some(error),
                    },
                    "app-auto-update" => {
                        let enabled = value.as_str() == "true";
                        st.backend
                            .set_setting(serde_json::json!({"app_auto_update": enabled}));
                        hint = Some(if enabled {
                            i18n::tr(
                                "已开启：发现新版本后后台下载",
                                "On; new releases download in the background",
                            )
                            .into()
                        } else {
                            i18n::tr(
                                "已关闭自动下载：发现新版本只提示",
                                "Off; new releases are reported but not downloaded",
                            )
                            .into()
                        });
                    }
                    "plugin-install" => {
                        let path = value.as_str().trim();
                        if path.is_empty() {
                            hint = Some(i18n::tr("请输入插件包目录路径（含 plugin.json）", "Enter the plugin package directory (containing plugin.json)").into());
                        } else {
                            // Result (installed / refused: signature, hashes,
                            // busy) arrives through the notice channel.
                            st.backend.install_plugin(path);
                            hint = Some(i18n::tr("正在安装插件…", "Installing the plugin…").into());
                        }
                    }
                    "ftp-passive" => {
                        st.backend.set_setting(serde_json::json!({
                            "ftp_passive": value.as_str() == "true"
                        }));
                        hint = Some(i18n::tr("FTP 模式已保存：空闲插件重启后生效", "FTP mode saved; idle plugins pick it up after a restart").into());
                    }
                    "bt-port" => match value.trim().parse::<u16>() {
                        Ok(0) | Err(_) => hint = Some(i18n::tr("监听端口需要 1-65535 的数字", "The listen port must be between 1 and 65535").into()),
                        Ok(port) => {
                            st.backend
                                .set_setting(serde_json::json!({"bt_listen_port": port}));
                            hint = Some(i18n::tr("BT 端口已保存：空闲时重启生效", "BT port saved; applies when idle / after a restart").into());
                        }
                    },
                    "bt-dht" => {
                        st.backend.set_setting(serde_json::json!({
                            "bt_dht": value.as_str() == "true"
                        }));
                        hint = Some(i18n::tr("DHT 已保存：空闲时重启生效", "DHT saved; applies when idle / after a restart").into());
                    }
                    "bt-upload-limit" => {
                        // Slider reports KB/s; 0 is the stored form of "no cap".
                        let kbps = value.trim().parse::<f64>().unwrap_or(0.0).max(0.0);
                        let bps = (kbps * 1024.0) as u64;
                        st.backend
                            .set_setting(serde_json::json!({"bt_upload_bps": bps}));
                        hint = Some(i18n::tr("上传限速已保存：对新任务与重启后的做种生效", "Upload limit saved; applies to new tasks and to seeding after a restart").into());
                    }
                    "bt-blocklist" => {
                        st.backend
                            .set_setting(serde_json::json!({"bt_blocklist": value.as_str()}));
                        hint = Some(i18n::tr("屏蔽列表已保存：空闲时重启生效", "Blocklist saved; applies when idle / after a restart").into());
                    }
                    "bt-trackers" => {
                        st.backend
                            .set_setting(serde_json::json!({"bt_trackers": value.as_str()}));
                        hint = Some(i18n::tr("Tracker 已保存：附加到之后的任务", "Tracker saved; attached to tasks added afterwards").into());
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
                            hint = Some(i18n::tr("请先填写开始与结束时间", "Enter the start and end times first").into());
                        } else {
                            st.backend
                                .set_setting(serde_json::json!({"schedule_on": enabled}));
                            hint = Some(if enabled {
                                i18n::tr("计划已开启：时段外自动暂停队列", "Schedule on; the queue pauses outside the window").into()
                            } else {
                                i18n::tr("计划已关闭：队列不再按时间暂停", "Schedule off; the queue no longer pauses by time").into()
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
                        hint = Some(i18n::tr("时间已保存（HH:MM）", "Times saved (HH:MM)").into());
                    }
                    // Scheduler limits go through the same set_settings RPC;
                    // the daemon validates them and applies them to the
                    // running queue. Tasks already downloading keep the
                    // budget they started with.
                    "max-tasks" => match value.trim().parse::<u64>() {
                        Ok(number) => {
                            st.backend
                                .set_setting(serde_json::json!({"max_concurrent": number}));
                            hint = Some(i18n::tr("并发上限已保存：排队任务按新上限启动", "Concurrency cap saved; queued tasks start under the new cap").into());
                        }
                        Err(_) => hint = Some(i18n::tr(format!("并发上限「{value}」无效"), format!("Invalid concurrency cap: {value}"))),
                    },
                    "max-connections" => match value.trim().parse::<u64>() {
                        Ok(number) => {
                            st.backend
                                .set_setting(serde_json::json!({"connections": number}));
                            hint = Some(i18n::tr("每任务连接数已保存：新任务生效（HTTP 上限 16）", "Connections per task saved; applies to new tasks (HTTP capped at 16)").into());
                        }
                        Err(_) => hint = Some(i18n::tr(format!("每任务连接数「{value}」无效"), format!("Invalid connection count: {value}"))),
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
                                    i18n::tr("已关闭自动重试", "Automatic retries are off").into()
                                } else {
                                    i18n::tr("重试上限已保存：下次失败时生效", "Retry cap saved; applies from the next failure").into()
                                });
                            }
                            None => hint = Some(i18n::tr(format!("重试次数「{value}」无效"), format!("Invalid retry count: {value}"))),
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
                            i18n::tr(
                                format!("全局限速已保存：{} KB/s，新任务生效", st.download_limit_kb),
                                format!("Global cap saved: {} KB/s, applies to new tasks", st.download_limit_kb),
                            )
                        } else {
                            i18n::tr("已关闭全局限速", "The global speed cap is off")
                        });
                    }
                    "download-limit" => match value.trim().parse::<f64>() {
                        Ok(kb) if kb >= 0.0 => {
                            st.download_limit_kb = kb;
                            if st.global_limit {
                                st.backend.set_setting(
                                    serde_json::json!({"max_speed_bps": (kb * 1024.0) as u64}),
                                );
                                hint = Some(i18n::tr(format!("下载限速已保存：{kb} KB/s，新任务生效"), format!("Download cap saved: {kb} KB/s, applies to new tasks")));
                            } else {
                                hint = Some(i18n::tr("已记录限速，启用全局限速后生效", "Rate recorded; applies once the global speed cap is on").into());
                            }
                        }
                        _ => hint = Some(i18n::tr(format!("限速值「{value}」无效"), format!("Invalid speed limit: {value}"))),
                    },
                    // Persisted locally, no daemon round-trip.
                    "language" => {
                        // The ComboBox shows the display names; resolve the
                        // language code through the shared table so the UI
                        // list and the runtime cannot drift.
                        let code = i18n::LANGUAGES
                            .iter()
                            .find(|(_, name)| *name == value.as_str())
                            .map(|(code, _)| *code)
                            .unwrap_or("zh");
                        st.prefs.lang = code.to_string();
                        st.prefs.save();
                        i18n::set_language(code);
                        if let Err(error) = slint::select_bundled_translation(if code == "zh" {
                            ""
                        } else {
                            code
                        }) {
                            eprintln!("aether: language selection failed: {error:?}");
                        }
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_language(i18n::index_of(code) as i32);
                        }
                        hint = Some(if code == "zh" {
                            "界面语言已切回简体中文".into()
                        } else {
                            i18n::tr(
                                "界面语言已切换（Rust 侧字符串仅完整支持中文与英文）",
                                format!("Language switched to {code} (Rust-side strings cover Chinese and English)"),
                            )
                        });
                    }
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
                    "clipboard-watch" => {
                        let enabled = value.as_str() == "true";
                        // No backend in this session: refuse the switch
                        // instead of saving a preference that does nothing.
                        if st.clipboard_control.is_none() {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_clipboard(false);
                            }
                            hint = Some(i18n::tr("当前会话没有可用的剪贴板后端", "No clipboard backend in this session").into());
                        } else {
                            st.prefs.clipboard_watch = enabled;
                            if let Some(control) = st.clipboard_control.as_ref() {
                                control.set_enabled(enabled);
                            }
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_settings_clipboard(enabled);
                            }
                            st.prefs.save();
                            hint = Some(if enabled {
                                i18n::tr("剪贴板检测已开启：复制下载链接后会弹出提示", "Clipboard detection on; copying a download link offers it").into()
                            } else {
                                i18n::tr("剪贴板检测已关闭", "Clipboard detection is off").into()
                            });
                        }
                    }
                    "autostart" => {
                        let enabled = value.as_str() == "true";
                        match autostart::set_enabled(enabled) {
                            Ok(()) => {
                                if let Some(ui) = ui_handle.upgrade() {
                                    ui.set_settings_autostart(enabled);
                                }
                                hint = Some(if enabled {
                                    i18n::tr(
                                        "已设置为登录时启动",
                                        "Aether will start at login",
                                    )
                                    .into()
                                } else {
                                    i18n::tr(
                                        "已取消登录时启动",
                                        "Aether no longer starts at login",
                                    )
                                    .into()
                                });
                            }
                            Err(error) => {
                                // The OS refused: bounce the switch back and
                                // say why instead of showing a fake "on".
                                if let Some(ui) = ui_handle.upgrade() {
                                    ui.set_settings_autostart(!enabled);
                                }
                                hint = Some(i18n::tr(
                                    format!("无法修改开机自启：{error}"),
                                    format!("Could not change start-at-login: {error}"),
                                ));
                            }
                        }
                    }
                    "reset-settings" => {
                        st.prefs = UiPrefs::default();
                        st.prefs.save();
                        if let Some(control) = st.clipboard_control.as_ref() {
                            control.set_enabled(false);
                        }
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_clipboard(false);
                        }
                        // Language and theme count as appearance: reset them
                        // now instead of leaving the runtime on the old value
                        // until the next launch.
                        i18n::set_language("zh");
                        if let Err(error) = slint::select_bundled_translation("") {
                            eprintln!("aether: language selection failed: {error:?}");
                        }
                        save_theme_mode(0);
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_settings_accent(0);
                            ui.set_settings_motion(true);
                            ui.set_settings_flow(true);
                            ui.set_settings_start_page(0);
                            ui.set_settings_after_done(0);
                            ui.set_settings_notify_done(true);
                            ui.set_settings_notify_fail(true);
                            ui.set_settings_language(0);
                            ui.set_theme_mode(0);
                        }
                        hint = Some(i18n::tr("界面偏好已恢复默认（含语言与主题）", "UI preferences restored to defaults (including language and theme)").into());
                    }
                    // The settings page still shows controls that do nothing;
                    // saying so beats a silent no-op (honest UI).
                    other => hint = Some(i18n::tr(format!("设置项「{other}」尚未接线"), format!("Setting {other} is not wired up yet"))),
                }
            }
            if let (Some(hint), Some(ui)) = (hint, ui_handle.upgrade()) {
                show_toast(&ui, &state, &hint);
            }
            println!("[settings] {key} = {value}");
        });
    }

    // ---- plugin removal / rollback ------------------------------------------
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
            show_toast(
                &ui,
                &state,
                &i18n::tr(
                    format!("全局限速「{name}」尚未接线"),
                    format!("The global speed cap {name} is not wired up yet"),
                ),
            );
        });
    }

    // ---- support actions ---------------------------------------------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        // ---- RPC config ---------------------------------------------------------
        {
            let ui_handle = ui.as_weak();
            let state = state.clone();
            ui.on_rpc_apply(move |listen, secret| {
                let Some(ui) = ui_handle.upgrade() else {
                    return;
                };
                let listen = listen.trim().to_string();
                let secret = secret.trim().to_string();
                // The daemon rejects these too, but a local message beats a
                // noticed-less optimistic toast: an empty token is legal on
                // loopback only, and the address must be a socket address.
                if !listen.is_empty() {
                    match listen.parse::<std::net::SocketAddr>() {
                        Ok(addr) if !addr.ip().is_loopback() && secret.is_empty() => {
                            show_toast(
                                &ui,
                                &state,
                                &i18n::tr(
                                    "非回环地址必须设置访问令牌",
                                    "A non-loopback address requires an access token",
                                ),
                            );
                            return;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            show_toast(
                                &ui,
                                &state,
                                &i18n::tr(
                                    "监听地址需要 ip:port，例如 127.0.0.1:6800",
                                    "The listen address must be ip:port, e.g. 127.0.0.1:6800",
                                ),
                            );
                            return;
                        }
                    }
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

        ui.on_support_action(move |key| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let opened = |url: &str, zh: &str, en: &str| {
                if open_url(url) {
                    i18n::tr(zh.to_string(), en.to_string())
                } else {
                    i18n::tr(
                        format!("无法打开浏览器，请访问 {url}"),
                        format!("Could not open a browser; visit {url}"),
                    )
                }
            };
            let message = match key.as_str() {
                "docs" => opened(
                    "https://github.com/pengtang1024/Aether/tree/main/docs",
                    "已在浏览器打开文档",
                    "Documentation opened in the browser",
                ),
                "community" => opened(
                    "https://github.com/pengtang1024/Aether/discussions",
                    "已在浏览器打开社区",
                    "Community opened in the browser",
                ),
                "feedback" => opened(
                    "https://github.com/pengtang1024/Aether/issues",
                    "已在浏览器打开问题反馈",
                    "Issue tracker opened in the browser",
                ),
                "changelog" => opened(
                    "https://github.com/pengtang1024/Aether/blob/main/CHANGELOG.md",
                    "已在浏览器打开版本说明",
                    "Release notes opened in the browser",
                ),
                "license" => opened(
                    "https://github.com/pengtang1024/Aether/blob/main/LICENSE",
                    "已在浏览器打开许可证",
                    "License opened in the browser",
                ),
                "update" => {
                    let (connected, configured) = {
                        let st = state.borrow();
                        let snapshot = st.backend.snapshot();
                        (
                            snapshot.connected,
                            snapshot
                                .stats
                                .as_ref()
                                .is_some_and(|stats| stats.update.is_some()),
                        )
                    };
                    if !connected {
                        i18n::tr("daemon 未连接，无法检查更新", "The daemon is not connected")
                    } else if !configured {
                        i18n::tr(
                            "未配置更新源：daemon 需要 --update-url 与更新公钥",
                            "No update source: the daemon needs --update-url and the public key",
                        )
                    } else {
                        {
                            let mut st = state.borrow_mut();
                            st.backend.check_update();
                            st.update_watch = Some(UpdateWatch {
                                since: Instant::now(),
                                saw_checking: false,
                            });
                        }
                        i18n::tr("正在检查更新…", "Checking for updates…")
                    }
                }
                _ => i18n::tr("操作已记录", "Done"),
            };
            show_toast(&ui, &state, &message);
        });
    }

    // ---- simulation clock (5 Hz) ------------------------------------------
    // Clipboard toast action: open the new-download dialog with the staged
    // URL. The dialog keeps whatever was last typed afterwards (existing
    // behavior), so a plain manual open still starts empty only when the
    // field was cleared by the user.
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_toast_action_clicked(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let staged = state.borrow_mut().clipboard_url.take();
            let Some(url) = staged else {
                // Stale click after expiry: just close the toast.
                ui.set_toast_open(false);
                return;
            };
            ui.set_dialog_url(url.into());
            ui.set_dialog_open(true);
            ui.set_toast_open(false);
            state.borrow_mut().toast_until = None;
        });
    }

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
            // Clipboard detections: stage the URL and offer one-click start.
            {
                let detected = state
                    .borrow()
                    .clipboard
                    .as_ref()
                    .map(|watch| watch.drain())
                    .unwrap_or_default();
                for link in detected {
                    // Stage first, then drop the borrow: show_action_toast
                    // borrows the state itself.
                    state.borrow_mut().clipboard_url = Some(link.url);
                    show_action_toast(
                        &ui,
                        &state,
                        &i18n::tr("检测到下载链接", "Download link detected"),
                        &i18n::tr("新建任务", "New task"),
                    );
                }
            }
            refresh_rows(&ui, &state);
            // A user-triggered check is asynchronous: wait until the daemon
            // leaves `checking`, then toast the real result. Without this the
            // "checking…" toast vanished and the panel silently flipped back.
            {
                let outcome = {
                    let mut st = state.borrow_mut();
                    let update = st
                        .backend
                        .snapshot()
                        .stats
                        .as_ref()
                        .and_then(|stats| stats.update.clone());
                    match (st.update_watch.as_mut(), update) {
                        (None, _) => None,
                        (Some(watch), None) => {
                            if watch.since.elapsed() > Duration::from_secs(150) {
                                st.update_watch = None;
                                Some(i18n::tr(
                                    "检查更新超时：daemon 没有返回状态",
                                    "Update check timed out: the daemon reported no state",
                                ))
                            } else {
                                None
                            }
                        }
                        (Some(watch), Some(update)) => {
                            let status = update["status"].as_str().unwrap_or("");
                            if status == "checking" {
                                watch.saw_checking = true;
                                None
                            } else if watch.saw_checking
                                || watch.since.elapsed() > Duration::from_secs(2)
                            {
                                st.update_watch = None;
                                Some(update_status_text(Some(&update)))
                            } else {
                                None
                            }
                        }
                    }
                };
                if let Some(message) = outcome {
                    show_toast(&ui, &state, &message);
                }
            }
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
                            send_notification(
                                &i18n::tr("下载完成", "Download completed"),
                                &i18n::tr(
                                    format!("「{}」已下载完成", item.name),
                                    format!("“{}” has finished downloading", item.name),
                                ),
                            );
                        }
                        match after {
                            1 => {
                                if let Some(dir) = &item.dir {
                                    let _ = open_path(std::path::Path::new(dir));
                                }
                            }
                            2 => {
                                if let (Some(dir), Some(file)) = (&item.dir, &item.file) {
                                    let _ = open_path(&std::path::Path::new(dir).join(file));
                                }
                            }
                            _ => {}
                        }
                    } else if notify_fail {
                        let detail = item.error.as_deref().unwrap_or_default();
                        let body = if detail.is_empty() {
                            i18n::tr(
                                format!("「{}」下载失败", item.name),
                                format!("“{}” failed to download", item.name),
                            )
                        } else {
                            i18n::tr(
                                format!("「{}」下载失败：{detail}", item.name),
                                format!("“{}” failed to download: {detail}", item.name),
                            )
                        };
                        send_notification(&i18n::tr("下载失败", "Download failed"), &body);
                    }
                }
            }
        });
    }

    // Follow live desktop theme changes while the user chose "follow system".
    // System tray (Linux StatusNotifier / Windows Shell_NotifyIcon): best
    // effort — without a host it logs one line and the window keeps working
    // as the only entry point. Only a *visible* tray earns close-to-tray:
    // closing then hides the window (the tray menu quits), while a session
    // without a tray keeps the default where closing quits.
    let tray_up = tray::start(ui.as_weak()).is_some();
    if tray_up {
        ui.window()
            .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    }

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

    // A silent start hides the window and leaves the tray as the way back.
    // If the session refused a tray icon (GNOME without AppIndicator, macOS,
    // a locked-down login session) hiding would strand the app with no entry
    // point, so the request degrades to a normal visible start and says why.
    if silent && !tray_up {
        eprintln!("aether: --silent requested but no system tray is available; showing the window");
    }
    if launch::hide_on_start(silent, tray_up) {
        // Nothing is visible, so the loop must survive "the last window was
        // closed" and quit only when the tray menu asks it to.
        slint::run_event_loop_until_quit()
    } else {
        ui.show()?;
        slint::run_event_loop()
    }
}

#[cfg(test)]
mod notification_tests {
    /// CI runs this on a headless runner: the HKCU registration must really
    /// land in the registry (that part needs no desktop), the toast itself
    /// is best-effort — a session without a shell can refuse it — and the
    /// whole path must never panic.
    #[cfg(windows)]
    #[test]
    fn toast_registers_the_aumid_and_never_panics() {
        super::send_notification("Aether test", "hello from CI");
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{}", super::AUMID);
        let status = std::process::Command::new("reg")
            .args(["query", &key, "/v", "DisplayName"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("reg query must run");
        assert!(status.success(), "AUMID must be registered under HKCU");
    }
}

#[cfg(test)]
mod update_status_tests {
    use serde_json::{json, Value};

    use super::{rpc_status_text, update_status_text};

    /// The process language is global and other test threads flip it, so a
    /// line is accepted in either language instead of setting the flag here
    /// (ja/zh-TW fall back to the Chinese string for Rust-side strings).
    fn assert_line(update: Option<Value>, zh: &str, en: &str) {
        let text = update_status_text(update.as_ref());
        assert!(
            text == zh || text == en,
            "expected {zh:?} or {en:?}, got {text:?}"
        );
    }

    /// The About panel renders exactly one line per daemon state; a missing
    /// source must read as "not configured", never as "up to date".
    #[test]
    fn every_update_state_has_a_line() {
        assert_line(
            None,
            "未配置更新源（daemon 需要 --update-url 与更新公钥）",
            "No update source (the daemon needs --update-url and the public key)",
        );
        assert_line(
            Some(json!({"status": "checking"})),
            "正在检查更新…",
            "Checking for updates…",
        );
        assert_line(
            Some(json!({"status": "idle"})),
            "等待首次后台检查（每 6 小时一次）",
            "Waiting for the first background check (every 6 hours)",
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_line(
            Some(json!({"status": "up_to_date"})),
            &format!("已是最新版本 {version}"),
            &format!("Up to date ({version})"),
        );
        assert_line(
            Some(json!({"status": "available", "latest": "0.3.0"})),
            "发现新版本 0.3.0，自动下载已关闭",
            "Version 0.3.0 is available; automatic downloads are off",
        );
        assert_line(
            Some(json!({"status": "ready", "latest": "0.3.0"})),
            "新版本 0.3.0 已下载，重启后生效",
            "Version 0.3.0 downloaded; restart to apply",
        );
        assert_line(
            Some(json!({"status": "error", "error": "timeout"})),
            "检查失败：timeout",
            "Check failed: timeout",
        );
        assert_line(
            Some(json!({"status": "what"})),
            "更新状态未知",
            "Unknown update state",
        );
    }

    #[test]
    fn rpc_status_names_the_configured_address() {
        let off = rpc_status_text("", false);
        assert!(
            off == "远程控制未开启" || off == "Remote control is off",
            "unexpected: {off:?}"
        );
        let with_token = rpc_status_text("127.0.0.1:7777", true);
        assert!(with_token.contains("127.0.0.1:7777"), "{with_token}");
        assert!(
            with_token.contains("令牌") || with_token.contains("token"),
            "{with_token}"
        );
        let no_token = rpc_status_text("0.0.0.0:7777", false);
        assert!(no_token.contains("0.0.0.0:7777"), "{no_token}");
        assert!(
            no_token.contains("回环") || no_token.contains("loopback"),
            "{no_token}"
        );
    }
}

#[cfg(test)]
mod plugin_pin_tests {
    use super::{format_plugin_pins, parse_plugin_pins};
    use serde_json::json;

    #[test]
    fn pin_text_roundtrips_through_the_daemon_shape() {
        let value = parse_plugin_pins("http=0.2.0; bt=1.0.0\ns3 = 0.1.0").unwrap();
        assert_eq!(
            value,
            json!({"http": "0.2.0", "bt": "1.0.0", "s3": "0.1.0"})
        );
        assert_eq!(format_plugin_pins(&value), "bt=1.0.0; http=0.2.0; s3=0.1.0");
        assert_eq!(format_plugin_pins(&json!(null)), "");
        assert_eq!(parse_plugin_pins("  ; \n").unwrap(), json!({}));
    }

    #[test]
    fn pin_text_rejects_entries_without_a_version() {
        assert!(parse_plugin_pins("http").is_err());
        assert!(parse_plugin_pins("http=").is_err());
        assert!(parse_plugin_pins("=0.2.0").is_err());
    }
}
