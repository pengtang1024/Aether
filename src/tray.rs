//! System tray: one entry point, per-platform implementation behind cfg.
//!
//! - Unix (Linux/BSD): [`sni`] — StatusNotifierItem on the session bus.
//!   Needs a StatusNotifier host to display: KDE ships one, GNOME needs the
//!   AppIndicator extension; without a bus or a host, startup degrades to
//!   "no tray" with one log line.
//! - Windows: [`win`] (`tray_windows.rs`) — a notification-area icon over
//!   raw Shell_NotifyIconW FFI (zero dependencies, same philosophy as the
//!   pipe transport). A non-interactive session can refuse the icon — the
//!   same honest degradation.
//! - macOS: intentionally not implemented. An NSStatusItem needs objc2 and
//!   this repo's macOS CI is check-only, so there is no way to verify the
//!   icon behaves; shipping an untested tray is worse than none.
//!
//! Either way downloads keep running in the daemon: the tray only summons
//! or quits the window, and both entry paths return `None` (one log line)
//! when the session refuses an icon.

#[cfg(unix)]
#[path = "tray_sni.rs"]
mod sni;

#[cfg(windows)]
#[path = "tray_windows.rs"]
mod win;

use std::time::Duration;

/// Bring the window back to the user: map it, clear the minimized state and
/// ask the compositor for focus. `Window::show()` alone only maps a hidden
/// window — a minimized one stays minimized and a covered one stays behind,
/// so the tray click looks like a no-op. Wayland may still refuse activation
/// requested by a background process; that is the compositor's call.
pub(crate) fn present(ui: &crate::AppWindow) {
    use slint::ComponentHandle;

    let window = ui.window();
    let _ = window.show();
    if raise_now(&window) {
        return;
    }
    // Wayland destroys a hidden window and Slint recreates it on a later
    // event-loop turn, so the winit window does not exist yet. One retry, on
    // a thread-local timer (the handle must outlive this call), lands after
    // the window is back.
    PRESENT_TIMER.with(|timer| {
        let window = ui.as_weak();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(150),
            move || {
                if let Some(ui) = window.upgrade() {
                    let _ = raise_now(&ui.window());
                }
            },
        );
    });
}

thread_local! {
    static PRESENT_TIMER: slint::Timer = slint::Timer::default();
}

/// Un-minimize and ask for focus; `false` = no winit window exists (yet).
fn raise_now(window: &slint::Window) -> bool {
    use slint::winit_030::WinitWindowAccessor;

    window
        .with_winit_window(|winit_window| {
            winit_window.set_minimized(false);
            winit_window.focus_window();
        })
        .is_some()
}

/// Ask the already-running instance to surface its window. `false` = no
/// running UI answered (no tray endpoint on either platform), and the caller
/// falls back to the "already running" notification.
pub fn summon_existing() -> bool {
    #[cfg(unix)]
    {
        sni::summon()
    }
    #[cfg(windows)]
    {
        win::summon()
    }
}

/// Start the tray on a dedicated thread. `None` = the session refused an
/// icon (no bus/host on unix, no shell icon here); the reason is logged
/// once and the window stays the only entry point.
pub fn start(window: slint::Weak<crate::AppWindow>) -> Option<()> {
    #[cfg(unix)]
    {
        sni::start(window)
    }
    #[cfg(windows)]
    {
        // Tray callbacks arrive on the tray thread; hop to the Slint event
        // loop exactly like the unix path does. The Hooks shape is what
        // keeps `tray_windows` free of Slint types, so its tests start an
        // icon without a window or a display.
        win::start(win::Hooks {
            on_show: Box::new(move || {
                let window = window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = window.upgrade() {
                        present(&ui);
                    }
                });
            }),
            on_quit: Box::new(|| {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            }),
        })
    }
}
