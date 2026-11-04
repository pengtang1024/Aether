//! Transient snackbars.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::state::State;
use crate::AppWindow;

/// Show a transient M3 snackbar. The 200 ms engine timer hides it again.
pub(crate) fn show_toast(ui: &AppWindow, state: &Rc<RefCell<State>>, message: &str) {
    state.borrow_mut().toast_until = Some(Instant::now() + Duration::from_millis(2600));
    ui.set_toast_text(message.into());
    ui.set_toast_action("".into());
    ui.set_toast_open(true);
}

/// Snackbar with a click-through action (clipboard detection's "download"
/// offer). The action payload lives in `State`, keyed by this toast.
pub(crate) fn show_action_toast(
    ui: &AppWindow,
    state: &Rc<RefCell<State>>,
    message: &str,
    action: &str,
) {
    state.borrow_mut().toast_until = Some(Instant::now() + Duration::from_millis(7000));
    ui.set_toast_text(message.into());
    ui.set_toast_action(action.into());
    ui.set_toast_open(true);
}
