//! The 5 Hz UI clock: drift, notices, toasts, notifications and page state.
//!
//! The clock owns visibility. While the window is hidden or minimized both
//! timers stop and the daemon poller parks, so a resident Aether that nobody
//! can see does no UI work at all. `resume` restarts the timers and paints the
//! last snapshot immediately, then the poller re-reads the daemon once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;
use slint::{Timer, TimerMode};

use crate::detail::refresh_detail;
use crate::i18n;
use crate::platform::{open_path, send_notification, system_is_dark};
use crate::state::State;
use crate::toast::{show_action_toast, show_toast};
use crate::update_status::update_status_text;
use crate::AppWindow;

/// 5 Hz: fast enough for the drift/speed animation, slow enough to stay cheap.
const TICK: Duration = Duration::from_millis(200);
/// Desktop theme changes are rare; 10 s is plenty.
const THEME_TICK: Duration = Duration::from_secs(10);
/// Sub-pixel drift cadence; see `tick_body`.
const DRIFT_INTERVAL: Duration = Duration::from_secs(1);

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
        tick_body(&ui, &state);
    })))
}

fn tick_body(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let drawer_open = ui.get_drawer_open();
    // The ambient drift is a 28 s sine: sampling it at 1 Hz moves the layers
    // by at most 0.22 px per step, visually identical to 5 Hz but only
    // repaints once a second. Everything else keeps its tick rate.
    let drift_due = {
        let mut st = state.borrow_mut();
        let due = st.drift_at.is_none_or(|at| at.elapsed() >= DRIFT_INTERVAL);
        if due {
            st.drift_at = Some(Instant::now());
        }
        due
    };
    if drift_due {
        let drift = state.borrow().backend.drift();
        ui.set_drift(drift);
    }
    let (expire_toast, notice) = {
        let mut st = state.borrow_mut();
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
    if let Some(text) = notice {
        show_toast(ui, state, &text);
    }
    // Clipboard detections: stage the URL and offer one-click start.
    {
        let detected = state
            .borrow()
            .clipboard
            .as_ref()
            .map(|watch| watch.drain())
            .unwrap_or_default();
        for link in detected {
            // Stage first, then drop the borrow: show_action_toast
            // borrows the state itself.
            state.borrow_mut().clipboard_url = Some(link.url);
            show_action_toast(
                ui,
                state,
                &i18n::tr("检测到下载链接", "Download link detected"),
                &i18n::tr("新建任务", "New task"),
            );
        }
    }
    // One snapshot per tick: rows and the detail drawer share it instead
    // of each cloning every task, plugin and detail payload. Applying a
    // snapshot dirties the window on every setter, so skip the pass while
    // the data (and the uptime minute) has not moved.
    let snapshot = state.borrow().backend.snapshot();
    let uptime_minute = state.borrow().started.elapsed().as_secs() / 60;
    let repaint = {
        let mut st = state.borrow_mut();
        let repaint = st
            .last_painted
            .as_ref()
            .is_none_or(|(last, minute)| last != &snapshot || *minute != uptime_minute);
        if repaint {
            st.last_painted = Some((snapshot.clone(), uptime_minute));
        }
        repaint
    };
    if repaint {
        crate::rows::apply_snapshot(ui, state, &snapshot);
    }
    // A user-triggered check is asynchronous: wait until the daemon
    // leaves `checking`, then toast the real result. Without this the
    // "checking…" toast vanished and the panel silently flipped back.
    {
        let outcome = {
            let mut st = state.borrow_mut();
            let update = st.backend.update_status();
            match (st.update_watch.as_mut(), update) {
                (None, _) => None,
                (Some(watch), None) => {
                    if watch.since.elapsed() > Duration::from_secs(150) {
                        st.update_watch = None;
                        Some(i18n::tr(
                            "检查更新超时：daemon 没有返回状态",
                            "Update check timed out: the daemon reported no state",
                        ))
                    } else {
                        None
                    }
                }
                (Some(watch), Some(update)) => {
                    let status = update["status"].as_str().unwrap_or("");
                    if status == "checking" {
                        watch.saw_checking = true;
                        None
                    } else if watch.saw_checking || watch.since.elapsed() > Duration::from_secs(2) {
                        st.update_watch = None;
                        Some(update_status_text(Some(&update)))
                    } else {
                        None
                    }
                }
            }
        };
        if let Some(message) = outcome {
            show_toast(ui, state, &message);
        }
    }
    if drawer_open && repaint {
        refresh_detail(ui, state, &snapshot);
    }
    // Persist the page for "上次页面"; run the after-done action for
    // tasks that completed since the last tick.
    {
        let mut st = state.borrow_mut();
        let page = ui.get_page();
        if page != st.seen_page {
            st.seen_page = page;
            st.prefs.last_page = page;
            st.prefs.save();
        }
        let mut done = Vec::new();
        st.backend.take_finished(&mut done);
        let after = st.prefs.after_done;
        let notify_done = st.prefs.notify_done;
        let notify_fail = st.prefs.notify_fail;
        drop(st);
        for item in done {
            if item.done {
                if notify_done {
                    send_notification(
                        &i18n::tr("下载完成", "Download completed"),
                        &i18n::tr(
                            format!("「{}」已下载完成", item.name),
                            format!("“{}” has finished downloading", item.name),
                        ),
                    );
                }
                match after {
                    1 => {
                        if let Some(dir) = &item.dir {
                            let _ = open_path(std::path::Path::new(dir));
                        }
                    }
                    2 => {
                        if let (Some(dir), Some(file)) = (&item.dir, &item.file) {
                            let _ = open_path(&std::path::Path::new(dir).join(file));
                        }
                    }
                    _ => {}
                }
            } else if notify_fail {
                let detail = item.error.as_deref().unwrap_or_default();
                let body = if detail.is_empty() {
                    i18n::tr(
                        format!("「{}」下载失败", item.name),
                        format!("“{}” failed to download", item.name),
                    )
                } else {
                    i18n::tr(
                        format!("「{}」下载失败：{detail}", item.name),
                        format!("“{}” failed to download: {detail}", item.name),
                    )
                };
                send_notification(&i18n::tr("下载失败", "Download failed"), &body);
            }
        }
    }
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
