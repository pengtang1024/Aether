//! System tray registration and close-to-tray behaviour.
use slint::ComponentHandle;

use crate::tray;
use crate::AppWindow;

// System tray (Linux StatusNotifier / Windows Shell_NotifyIcon): best
// effort — without a host it logs one line and the window keeps working
// as the only entry point. Only a *live* tray earns close-to-tray:
// closing then hides the window (the tray menu quits), while a session
// without a tray keeps the default where closing quits. One Slint
// `SystemTrayIcon` covers Linux (ksni), Windows (Shell_NotifyIcon) and
// macOS (NSStatusItem); the component itself is the loop keepalive.
pub(super) fn install(ui: &AppWindow) -> (bool, Option<crate::Tray>) {
    let tray = if tray::available() {
        tray::start(ui.as_weak(), true)
    } else {
        None
    };
    let tray_up = tray.is_some();
    if tray_up {
        let close_ui = ui.as_weak();
        ui.window().on_close_requested(move || {
            tray::release_above();
            // Hidden either way: stop the clock and the daemon poller before
            // the window leaves the screen.
            crate::wiring::suspend();
            // X11: minimize instead of unmap (winit panics on hiding a focused
            // window there); the tray restores it with set_minimized(false).
            if tray::minimize_on_close() {
                if let Some(ui) = close_ui.upgrade() {
                    use slint::winit_030::WinitWindowAccessor;
                    let window = ui.window();
                    if window.has_winit_window() {
                        window.with_winit_window(|winit_window| {
                            winit_window.set_minimized(true);
                        });
                        return slint::CloseRequestResponse::KeepWindowShown;
                    }
                }
            }
            slint::CloseRequestResponse::HideWindow
        });
        tray::start_endpoint(ui.as_weak());
    }
    (tray_up, tray)
}
