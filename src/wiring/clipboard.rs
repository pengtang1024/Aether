//! Clipboard detection toast: open the staged link in the new-task dialog.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;

use crate::state::State;
use crate::AppWindow;

pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
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
