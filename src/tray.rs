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
        use slint::ComponentHandle;
        // Tray callbacks arrive on the tray thread; hop to the Slint event
        // loop exactly like the unix path does. The Hooks shape is what
        // keeps `tray_windows` free of Slint types, so its tests start an
        // icon without a window or a display.
        win::start(win::Hooks {
            on_show: Box::new(move || {
                let window = window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = window.upgrade() {
                        let _ = ui.window().show();
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
