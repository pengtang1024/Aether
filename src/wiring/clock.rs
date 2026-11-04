//! The 5 Hz UI clock: lifecycle, drift sampling and page state.
//!
//! The clock owns visibility. While the window is hidden or minimized both
//! timers stop and the daemon poller parks, so a resident Aether that nobody
//! can see does no UI work at all. `resume` restarts the timers and paints the
//! last snapshot immediately, then the poller re-reads the daemon once.
//! The per-tick body lives in `clock/tick.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;
use slint::{Timer, TimerMode};

use crate::backend::Snapshot;
use crate::detail::refresh_detail;
use crate::platform::system_is_dark;
use crate::state::State;
use crate::AppWindow;

mod tick;

#[cfg(test)]
mod tests;

/// 5 Hz: fast enough for the drift/speed animation, slow enough to stay cheap.
const TICK: Duration = Duration::from_millis(200);
/// Desktop theme changes are rare; 10 s is plenty.
const THEME_TICK: Duration = Duration::from_secs(10);

type Callback = Rc<RefCell<Box<dyn FnMut()>>>;

thread_local! {
    /// The running clock, reachable from the tray (show) and the close
    /// handler (hide) without threading state through every call site.
    static CLOCK: RefCell<Option<Rc<Clock>>> = const { RefCell::new(None) };
}

struct Clock {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
    timer: Timer,
    theme_timer: Timer,
    tick: Callback,
    theme_tick: Callback,
    paused: Cell<bool>,
}

/// Start both timers. The clock owns them: dropping the `Clock` stops them.
pub(super) fn install(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let tick = make_tick(ui, state);
    let theme_tick = make_theme_tick(ui);
    let timer = Timer::default();
    let theme_timer = Timer::default();
    timer.start(TimerMode::Repeated, TICK, driver(tick.clone()));
    theme_timer.start(TimerMode::Repeated, THEME_TICK, driver(theme_tick.clone()));
    CLOCK.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(Clock {
            ui: ui.as_weak(),
            state: state.clone(),
            timer,
            theme_timer,
            tick,
            theme_tick,
            paused: Cell::new(false),
        }));
    });
}

/// Stop every timer and park the daemon poller. Idempotent.
pub(crate) fn suspend() {
    CLOCK.with(|slot| {
        let Some(clock) = slot.borrow().as_ref().map(Rc::clone) else {
            return;
        };
        if clock.paused.get() {
            return;
        }
        clock.paused.set(true);
        clock.timer.stop();
        clock.theme_timer.stop();
        clock.state.borrow().backend.set_paused(true);
    });
}

/// Restart the timers and show current data at once. Idempotent.
pub(crate) fn resume() {
    CLOCK.with(|slot| {
        let Some(clock) = slot.borrow().as_ref().map(Rc::clone) else {
            return;
        };
        if !clock.paused.get() {
            return;
        }
        clock.paused.set(false);
        clock
            .timer
            .start(TimerMode::Repeated, TICK, driver(clock.tick.clone()));
        clock.theme_timer.start(
            TimerMode::Repeated,
            THEME_TICK,
            driver(clock.theme_tick.clone()),
        );
        clock.state.borrow().backend.set_paused(false);
        // Paint what we have before the first tick; the poller's forced
        // refresh lands on the next tick, so the window never opens blank.
        let Some(ui) = clock.ui.upgrade() else {
            return;
        };
        let snapshot = { clock.state.borrow().backend.snapshot() };
        crate::rows::apply_snapshot(&ui, &clock.state, &snapshot);
        if ui.get_drawer_open() {
            refresh_detail(&ui, &clock.state, &snapshot);
        }
    });
}

fn driver(callback: Callback) -> impl FnMut() {
    move || (callback.borrow_mut())()
}

fn make_tick(ui: &AppWindow, state: &Rc<RefCell<State>>) -> Callback {
    let ui_handle = ui.as_weak();
    let state = state.clone();
    Rc::new(RefCell::new(Box::new(move || {
        let Some(ui) = ui_handle.upgrade() else {
            return;
        };
        // A hide we did not initiate (window manager, `hide()`) is caught
        // here: the tick cancels itself so nothing paints while it is gone.
        if !ui.window().is_visible() {
            suspend();
            return;
        }
        tick::tick_body(&ui, &state);
    })))
}

/// Repaint only when something visible moved: first paint, a changed
/// snapshot, or a new uptime minute. Drift is applied before this check and
/// does not force a repaint by itself.
fn needs_repaint(last: Option<&(Snapshot, u64)>, snapshot: &Snapshot, minute: u64) -> bool {
    last.is_none_or(|(painted, painted_minute)| painted != snapshot || *painted_minute != minute)
}

// Follow live desktop theme changes while the user chose "follow system".
fn make_theme_tick(ui: &AppWindow) -> Callback {
    let ui_handle = ui.as_weak();
    Rc::new(RefCell::new(Box::new(move || {
        let Some(ui) = ui_handle.upgrade() else {
            return;
        };
        if ui.get_theme_mode() == 0 {
            let dark = system_is_dark();
            if ui.get_system_dark() != dark {
                ui.set_system_dark(dark);
            }
        }
    })))
}
