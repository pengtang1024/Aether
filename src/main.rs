//! Aether — a native Material 3 download manager (UI mockup).
//!
//! Rust drives a small simulated download engine; Slint renders everything with
//! its own Material 3 widget set. There is no WebView anywhere in the stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod engine;
mod model;

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Model, ModelRc, Timer, TimerMode, VecModel};

use engine::{chart_paths, chart_peak, Engine, LogEntry, Peer};
use model::{format_eta, format_pair, format_size, format_speed, Status, CHUNK_COUNT};

struct State {
    engine: Engine,
    filter: i32,
    query: String,
    detail_id: i32,
    rows: Rc<VecModel<TaskRow>>,
    peers: Rc<VecModel<PeerRow>>,
    logs: Rc<VecModel<LogRow>>,
    chunks: Rc<VecModel<f32>>,
    /// Ids of the rows currently materialised in `rows`, in order.
    row_ids: Vec<i32>,
    /// When the current toast should disappear.
    toast_until: Option<Instant>,
    /// Task id the chunk/peer/log models were seeded for. The peers and logs
    /// are static for a task, so refreshing them every tick would reset their
    /// repeaters and make the drawer content height pulse.
    detail_data_for: i32,
}

impl State {
    fn new() -> Self {
        let engine = Engine::new();
        let rows: Vec<TaskRow> = Vec::new().into();
        let _ = rows;
        State {
            engine,
            filter: 0,
            query: String::new(),
            detail_id: -1,
            rows: Rc::new(VecModel::default()),
            peers: Rc::new(VecModel::default()),
            logs: Rc::new(VecModel::default()),
            chunks: Rc::new(VecModel::from(vec![0.0_f32; CHUNK_COUNT])),
            row_ids: Vec::new(),
            toast_until: None,
            detail_data_for: -1,
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

    fn visible_ids(&self) -> Vec<i32> {
        self.engine
            .tasks()
            .iter()
            .filter(|t| self.matches(t))
            .map(|t| t.id)
            .collect()
    }
}

/// Extension label shown in the row's leading badge plus its colour family.
fn file_badge(name: &str) -> (String, i32) {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.zst")
    {
        return ("TAR".to_string(), 1);
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let (label, tone) = match ext.as_str() {
        "iso" => ("ISO", 2),
        "img" => ("IMG", 2),
        "dmg" => ("DMG", 2),
        "zip" => ("ZIP", 1),
        "tar" => ("TAR", 1),
        "gz" => ("GZ", 1),
        "xz" => ("XZ", 1),
        "7z" => ("7Z", 1),
        "rar" => ("RAR", 1),
        "mkv" => ("MKV", 3),
        "mp4" => ("MP4", 3),
        "avi" => ("AVI", 3),
        "mov" => ("MOV", 3),
        "webm" => ("WEBM", 3),
        "appimage" => ("APP", 4),
        "exe" => ("EXE", 4),
        "deb" => ("DEB", 4),
        "rpm" => ("RPM", 4),
        "pdf" => ("PDF", 0),
        "epub" => ("EPUB", 0),
        _ => ("FILE", 0),
    };
    (label.to_string(), tone)
}

/// Secondary line under the speed read-out. Kept in Rust so the table does not
/// accumulate a long conditional chain for every task state.
fn speed_label(status: Status) -> &'static str {
    match status {
        Status::Downloading => "实时速度",
        Status::Seeding => "上传速度",
        Status::Completed => "已保存",
        Status::Paused => "进度已保留",
        Status::Failed => "已停止",
        Status::Queued => "队列中",
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
    let (kind, kind_tone) = file_badge(&task.name);
    TaskRow {
        id: task.id,
        name: task.name.clone().into(),
        subtitle: task.subtitle().into(),
        kind: kind.into(),
        kind_tone,
        progress: task.progress(),
        progress_text: format_pair(task.done, task.total).into(),
        speed: if status == Status::Downloading || status == Status::Seeding {
            format_speed(task.speed).into()
        } else {
            "—".into()
        },
        speed_label: speed_label(status).into(),
        eta: eta.into(),
        status: status.code(),
        status_text: status.label().into(),
        paused: status == Status::Paused,
        completed: status == Status::Completed,
        failed: status == Status::Failed,
        torrent: task.torrent,
        note: task.note.clone().into(),
        selected: task.id == detail_id,
    }
}

fn refresh_rows(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let mut st = state.borrow_mut();
    let ids = st.visible_ids();
    let same_shape = ids == st.row_ids;
    let detail_id = st.detail_id;
    let rows: Vec<TaskRow> = ids
        .iter()
        .filter_map(|id| st.engine.task(*id))
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
    let total = st.engine.tasks().len();
    let active = st.engine.active_count();
    ui.set_stat_active(active.to_string().into());
    ui.set_stat_done(st.engine.completed_count().to_string().into());
    ui.set_stat_total(total.to_string().into());
    ui.set_total_speed(format_speed(st.engine.total_speed()).into());
    ui.set_page_subtitle(format!("{total} 个任务 · {active} 个正在传输").into());
}

fn refresh_detail(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let mut st = state.borrow_mut();
    let Some(task) = st.engine.task(st.detail_id) else {
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

    let (kind, kind_tone) = file_badge(&task.name);
    ui.set_detail(TaskDetail {
        name: task.name.clone().into(),
        subtitle: task.subtitle().into(),
        kind: kind.into(),
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
        // Opening (or switching) a task: seed the repeater models once. Keeping
        // the models stable afterwards avoids per-tick repeater resets, which
        // would collapse the peers/log sections and clamp the scroll offset.
        st.chunks.set_vec(task.chunks());
        let peers: Vec<PeerRow> = if task.torrent {
            st.engine
                .peers(st.detail_id)
                .into_iter()
                .map(peer_row)
                .collect()
        } else {
            Vec::new()
        };
        st.peers.set_vec(peers);
        let logs: Vec<LogRow> = st
            .engine
            .logs(st.detail_id)
            .into_iter()
            .map(log_row)
            .collect();
        st.logs.set_vec(logs);

        st.detail_data_for = st.detail_id;

        ui.set_chunks(ModelRc::from(st.chunks.clone()));
        ui.set_peers(ModelRc::from(st.peers.clone()));
        ui.set_logs(ModelRc::from(st.logs.clone()));
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

fn peer_row(peer: Peer) -> PeerRow {
    PeerRow {
        address: peer.address.into(),
        speed: format_speed(peer.speed).into(),
        ratio: peer.ratio,
        kind: peer.kind,
    }
}

fn log_row(entry: LogEntry) -> LogRow {
    LogRow {
        time: entry.time.into(),
        level: entry.level.into(),
        message: entry.message.into(),
    }
}

fn open_detail(ui: &AppWindow, state: &Rc<RefCell<State>>, id: i32) {
    {
        let mut st = state.borrow_mut();
        st.detail_id = id;
        st.peers.set_vec(Vec::new());
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

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    // Bind the window to aether.desktop so the dock / task switcher uses the
    // app icon instead of a generic fallback. This must run before `run()`.
    let _ = slint::set_xdg_app_id("aether");

    let state = Rc::new(RefCell::new(State::new()));

    // Initial data.
    refresh_rows(&ui, &state);

    // Screenshot helper: open a specific surface on startup so the mockup can
    // be captured in every state without driving the mouse.
    match std::env::var("AETHER_VIEW").as_deref() {
        Ok("settings") => ui.set_page(1),
        Ok("dialog") => ui.set_dialog_open(true),
        Ok("drawer") => {
            // Prefer an active torrent: it exercises chunks, peers and logs.
            let id = {
                let st = state.borrow();
                let tasks = st.engine.tasks();
                tasks
                    .iter()
                    .find(|task| task.torrent && task.status == Status::Downloading)
                    .or_else(|| tasks.iter().find(|task| task.status == Status::Downloading))
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
                let was_failed = st
                    .engine
                    .task(id)
                    .map(|task| task.status == Status::Failed)
                    .unwrap_or(false);
                let existed = st.engine.task(id).is_some();
                st.engine.apply_action(id, action);
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
        ui.on_new_download(move |url, path, _segments, limit_kb, note| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.filter = 0;
                st.query = String::new();
                st.engine.add_task(&url, &path, limit_kb, &note);
            }
            ui.set_active_filter(0);
            ui.set_search_text("".into());
            refresh_rows(&ui, &state);
            show_toast(&ui, &state, "已添加下载任务");
        });
    }

    // ---- settings ----------------------------------------------------------
    {
        let state = state.clone();
        ui.on_settings_changed(move |key, value| {
            // A mockup stores settings in memory; a real build would persist them.
            let _ = &state;
            println!("[settings] {key} = {value}");
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
            let expire_toast = {
                let mut st = state.borrow_mut();
                st.engine.tick(0.2);
                ui.set_drift(st.engine.drift());
                if st.toast_until.is_some_and(|until| Instant::now() >= until) {
                    st.toast_until = None;
                    true
                } else {
                    false
                }
            };
            if expire_toast {
                ui.set_toast_open(false);
            }
            refresh_rows(&ui, &state);
            if drawer_open {
                refresh_detail(&ui, &state);
            }
        });
    }

    ui.run()
}
