//! Recent completed/failed task rows in the activity list.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Model, ModelRc};

use crate::backend::Snapshot;
use crate::format::file_tone;
use crate::model::{rel_time, Status, Task};
use crate::state::State;
use crate::{AppWindow, RecordRow};

pub(super) fn apply(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    let mut st = state.borrow_mut();
    let now_ms = st.backend.now_ms();
    let terminal: Vec<&Task> = snapshot
        .tasks
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
