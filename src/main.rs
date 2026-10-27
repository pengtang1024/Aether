//! Aether — a native Material 3 download manager (UI mockup).
//!
//! Rust drives a small simulated download engine; Slint renders everything with
//! its own Material 3 widget set. There is no WebView anywhere in the stack.

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

use backend::Backend;
use chart::{chart_paths, chart_peak};
use model::{format_eta, format_pair, format_size, format_speed, Status, CHUNK_COUNT};

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
    last_notice: Option<String>,
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
            last_notice: None,
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

    let times = ["刚刚", "3 分钟前", "今天 09:12", "今天 08:40", "昨天 22:18"];
    let record_ids: Vec<i32> = tasks.iter().take(times.len()).map(|task| task.id).collect();
    let records: Vec<RecordRow> = tasks
        .iter()
        .take(times.len())
        .enumerate()
        .map(|(index, task)| RecordRow {
            id: task.id,
            name: task.name.clone().into(),
            kind_tone: file_tone(&task.name),
            status: task.status.code(),
            status_text: task.status.label().into(),
            time: times[index].into(),
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
            let mut hint = None;
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
                        if st.proxy_mode == "手动" && st.proxy_url.trim().is_empty() {
                            hint = Some("请先填写代理地址，再切换到手动模式");
                        } else {
                            let url = st.proxy_url.clone();
                            st.backend.set_proxy(&st.proxy_mode, &url);
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(hint), Some(ui)) = (hint, ui_handle.upgrade()) {
                show_toast(&ui, &state, hint);
            }
            println!("[settings] {key} = {value}");
        });
    }

    // ---- plugin feedback (catalogue lives in the UI mockup) ----------------
    {
        let ui_handle = ui.as_weak();
        let state = state.clone();
        ui.on_plugin_toggled(move |_id, enabled| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            show_toast(
                &ui,
                &state,
                if enabled {
                    "插件已启用（原型）"
                } else {
                    "插件已停用（原型）"
                },
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
                "install" => "插件安装器已打开（原型）",
                "update-all" => "已开始更新全部可更新插件",
                "reload" => "插件已重新加载（原型）",
                _ => "插件操作已记录",
            };
            show_toast(&ui, &state, message);
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
