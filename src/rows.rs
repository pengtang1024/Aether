//! Window model refresh: tasks, dashboard, catalogue and records.

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::Snapshot;
use crate::state::State;
use crate::AppWindow;

mod plugin_catalog;
mod records;
mod settings_mirror;
mod stats;
mod tasks;

pub(crate) fn refresh_rows(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let snapshot = state.borrow().backend.snapshot();
    apply_snapshot(ui, state, &snapshot);
}

/// Apply an already-cloned snapshot. The 5 Hz tick and the detail drawer
/// share one read so a refresh no longer copies the whole model twice.
pub(crate) fn apply_snapshot(ui: &AppWindow, state: &Rc<RefCell<State>>, snapshot: &Snapshot) {
    tasks::apply(ui, state, snapshot);
    settings_mirror::apply(ui, state, snapshot);
    stats::apply(ui, state, snapshot);
    plugin_catalog::apply(ui, state, snapshot);
    records::apply(ui, state, snapshot);
}
