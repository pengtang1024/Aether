//! Window signal wiring, grouped by the surface each handler serves.

use std::cell::RefCell;
use std::rc::Rc;

use crate::state::State;
use crate::AppWindow;

mod clipboard;
mod clock;
mod plugin_dialog;
mod plugins;
mod rpc;
mod settings;
mod support;
mod tasks;
mod tray;

pub(crate) use clock::{resume, suspend};

pub(crate) struct Runtime {
    pub(crate) tray_up: bool,
    _tray: Option<crate::Tray>,
}

pub(crate) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) -> Runtime {
    tasks::install(ui, state);
    settings::install(ui, state);
    plugins::install(ui, state);
    plugin_dialog::install(ui, state);
    rpc::install(ui, state);
    support::install(ui, state);
    clipboard::install(ui, state);
    // The clock owns its timers; it stops them while the window is hidden.
    clock::install(ui, state);
    let (tray_up, tray) = tray::install(ui);
    Runtime {
        tray_up,
        _tray: tray,
    }
}
