//! Task list rows materialised from the daemon snapshot.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Model, ModelRc};

use crate::backend::Snapshot;
use crate::format::to_row;
use crate::state::State;
use crate::AppWindow;
use crate::TaskRow;

pub(super) fn apply(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    let mut st = state.borrow_mut();
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
}
