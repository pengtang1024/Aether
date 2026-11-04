//! Task detail drawer: generic sections, chart and chunk grid.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Model, ModelRc};

use crate::chart::{chart_paths, chart_peak};
use crate::format::file_tone;
use crate::i18n;
use crate::model::{format_eta, format_size, format_speed, CHUNK_COUNT};
use crate::state::State;
use crate::{AppWindow, TaskDetail};

mod sections;

use sections::build_detail_sections;

pub(crate) fn refresh_detail(
    ui: &AppWindow,
    state: &Rc<RefCell<State>>,
    snapshot: &crate::backend::Snapshot,
) {
    let mut st = state.borrow_mut();
    let detail_id = st.detail_id;
    let Some(task) = snapshot.tasks.iter().find(|task| task.id == detail_id) else {
        return;
    };
    let detail = snapshot.details.get(&task.id);
    let seeding = detail
        .and_then(|detail| detail.get("seeding"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let ratio = detail
        .and_then(|detail| detail.get("ratio"))
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let uploaded = detail
        .and_then(|detail| detail.get("uploaded_bytes"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let seed_seconds = detail
        .and_then(|detail| detail.get("seed_seconds"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let peers = detail
        .and_then(|detail| detail.get("peers"))
        .and_then(|value| value.as_array())
        .map_or(0, Vec::len);
    let bt_text = if task.torrent {
        i18n::tr(
            format!(
                "{} · 分享率 {ratio:.2} · 已上传 {} · 做种 {} · 节点 {peers}",
                if seeding { "做种中" } else { "下载中" },
                format_size(uploaded as f64),
                format_eta(Some(seed_seconds as f64)),
            ),
            format!(
                "{} · ratio {ratio:.2} · uploaded {} · seeded {} · {peers} peers",
                if seeding { "seeding" } else { "downloading" },
                format_size(uploaded as f64),
                format_eta(Some(seed_seconds as f64)),
            ),
        )
    } else {
        String::new()
    };

    // Generic sections from the owning plugin's `detail_schema`. The UI
    // model carries no plugin id per task, so the payload is tried against
    // each plugin's schema and the first schema that yields rows wins.
    let sig = (
        task.id,
        detail.map(|value| value.to_string()).unwrap_or_default(),
    );
    if st.sections_sig.as_ref() != Some(&sig) {
        let sections = detail.map_or_else(Vec::new, |payload| {
            snapshot
                .plugins
                .iter()
                .find(|info| info.id == task.plugin_id)
                .map_or_else(Vec::new, |info| build_detail_sections(info, payload))
        });
        // Update rows in place when the shape is unchanged so the peers
        // list keeps its scroll position between payload refreshes.
        if st.sections.row_count() == sections.len() {
            for (index, section) in sections.into_iter().enumerate() {
                st.sections.set_row_data(index, section);
            }
        } else {
            st.sections.set_vec(sections);
        }
        if !st.sections_attached {
            ui.set_detail_sections(ModelRc::from(st.sections.clone()));
            st.sections_attached = true;
        }
        st.sections_sig = Some(sig);
    }

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
        torrent: task.torrent,
        bt_text: bt_text.into(),
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

pub(crate) fn open_detail(ui: &AppWindow, state: &Rc<RefCell<State>>, id: i32) {
    {
        let mut st = state.borrow_mut();
        st.detail_id = id;
    }
    let snapshot = state.borrow().backend.snapshot();
    refresh_detail(ui, state, &snapshot);
    crate::rows::apply_snapshot(ui, state, &snapshot);
    ui.set_drawer_open(true);
}
