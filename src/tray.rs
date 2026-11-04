//! System tray: Slint's cross-platform `SystemTrayIcon`.
//!
//! One implementation for Linux (StatusNotifierItem via ksni), Windows
//! (`Shell_NotifyIconW`) and macOS (`NSStatusItem`): the icon, its menu and
//! the keepalive that lets the event loop outlive a hidden window all come
//! from Slint. What stays ours is the *decision* whether a tray really exists
//! (Linux needs a StatusNotifier host; without one the icon is invisible and
//! the app must keep the honest "close = quit" behaviour) and the endpoint a
//! second launch uses to summon the window.

use std::cell::RefCell;
use std::time::Duration;

use slint::ComponentHandle;

use crate::Tray;

thread_local! {
    /// The live tray, for language switches made after startup.
    static TRAY: RefCell<Option<slint::Weak<Tray>>> = const { RefCell::new(None) };
    /// Keeps the one-shot focus retry alive (dropping a `Timer` stops it).
    static PRESENT_TIMER: slint::Timer = slint::Timer::default();
}

/// Start the tray and keep it alive for the process lifetime.
pub fn start(window: slint::Weak<crate::AppWindow>, resident: bool) -> Option<Tray> {
    let tray = Tray::new().ok()?;
    let show = window.clone();
    tray.on_show_requested(move || {
        if let Some(ui) = show.upgrade() {
            present(&ui);
        }
    });
    tray.on_quit_requested(|| {
        let _ = slint::quit_event_loop();
    });
    apply_language(&tray);
    tray.set_resident(resident);
    TRAY.with(|slot| *slot.borrow_mut() = Some(tray.as_weak()));
    Some(tray)
}

/// Follow a language switch: the tray menu is not part of the window, so it
/// does not re-translate on its own.
pub fn refresh_language() {
    TRAY.with(|slot| {
        if let Some(tray) = slot.borrow().as_ref().and_then(|weak| weak.upgrade()) {
            apply_language(&tray);
        }
    });
}

fn apply_language(tray: &Tray) {
    tray.set_show_label(crate::i18n::tr("显示主窗口", "Show main window").into());
    tray.set_quit_label(crate::i18n::tr("退出", "Quit").into());
    tray.set_tray_tooltip(crate::i18n::tr("Aether 下载管理器", "Aether Download Manager").into());
}

/// Bring the window back to the user: map it, clear the minimized state and
/// ask the compositor for focus. `Window::show()` alone only maps a hidden
/// window — a minimized one stays minimized and a covered one stays behind,
/// so the tray click looks like a no-op. Wayland may still refuse activation
/// requested by a background process; that is the compositor's call.
pub(crate) fn present(ui: &crate::AppWindow) {
    let window = ui.window();

    #[cfg(all(unix, not(target_os = "macos")))]
    let wayland = wayland_session();
    #[cfg(any(windows, target_os = "macos"))]
    let wayland = false;

    if wayland {
        // Wayland surfaces created at startup are 0x0 and cannot be re-sized
        // into a mapped window, and winit cannot unminimize either. Give the
        // surface the model size, then drop and recreate it so the compositor
        // gets a fresh, mappable toplevel.
        let size = window.size();
        if !window.is_visible() {
            window.set_size(size);
        }
        let _ = window.show();
        let _ = window.hide();
    }
    let _ = window.show();

    if !wayland {
        // X11 and Windows un-minimize and focus synchronously.
        let _ = raise_now(window);
        crate::wiring::resume();
        return;
    }

    // Wayland: xdg-activation is asynchronous and focus-stealing prevention
    // may ignore a request sent before the new surface is mapped, so keep
    // asking until the window reports focus (bounded).
    present_attempt(ui.as_weak(), 0);
    // The surface is mapped again: restart the clock and the poller now.
    crate::wiring::resume();
}

/// One delayed show/focus attempt. The first lands after the recreate; later
/// attempts only run while the window still reports no focus.
fn present_attempt(window: slint::Weak<crate::AppWindow>, attempt: u8) {
    PRESENT_TIMER.with(|timer| {
        let weak = window.clone();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(if attempt == 0 { 250 } else { 400 }),
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let window = ui.window();
                let _ = window.show();
                let _ = raise_now(window);
                #[cfg(all(unix, not(target_os = "macos")))]
                if attempt == 1 {
                    gnome_raise();
                }
                if attempt < 3 {
                    present_attempt(ui.as_weak(), attempt + 1);
                }
            },
        );
    });
}

/// Is this process drawing through Wayland (not X11/XWayland)?
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("WINIT_UNIX_BACKEND").is_none_or(|backend| backend != "x11")
}

/// X11 close-to-tray must minimize instead of unmap: winit's X11 event loop
/// panics ("Failed to unfocus input context", BadWindow) when the last window
/// is hidden while an input context is focused. A minimized window stays
/// mapped, and X11 can unminimize it again from the tray.
pub fn minimize_on_close() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        !wayland_session()
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        false
    }
}

/// GNOME/Wayland refuses to focus or raise a background app on its own:
/// winit's `focus_window` is a no-op and an xdg-activation request without an
/// input serial is ignored. The Window Calls shell extension exposes the
/// privileged window operations, so when it is installed the window is put on
/// top of the stack until the user actually focuses it. Best effort: every
/// failure is ignored and the window stays where the compositor put it.
#[cfg(all(unix, not(target_os = "macos")))]
fn gnome_available() -> bool {
    wayland_session()
        && std::env::var("XDG_CURRENT_DESKTOP")
            .map(|desktop| desktop.to_ascii_lowercase().contains("gnome"))
            .unwrap_or(false)
}

/// The Window Calls extension's current window list.
#[cfg(all(unix, not(target_os = "macos")))]
fn gnome_windows(conn: &zbus::blocking::Connection) -> Option<Vec<serde_json::Value>> {
    const SHELL: &str = "org.gnome.Shell";
    const PATH: &str = "/org/gnome/Shell/Extensions/Windows";
    const IFACE: &str = "org.gnome.Shell.Extensions.Windows";

    let reply = conn
        .call_method(Some(SHELL), PATH, Some(IFACE), "List", &())
        .ok()?;
    let body = reply.body();
    let json: String = body.deserialize().ok()?;
    serde_json::from_str(&json).ok()
}

/// The MetaWindow id of this process, as the shell sees it.
#[cfg(all(unix, not(target_os = "macos")))]
fn gnome_window_id(conn: &zbus::blocking::Connection) -> Option<u32> {
    let pid = std::process::id() as u64;
    gnome_windows(conn)?
        .iter()
        .find(|window| window["pid"].as_u64() == Some(pid))
        .and_then(|window| window["id"].as_u64())
        .map(|id| id as u32)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn gnome_call(conn: &zbus::blocking::Connection, method: &str, id: u32) {
    const SHELL: &str = "org.gnome.Shell";
    const PATH: &str = "/org/gnome/Shell/Extensions/Windows";
    const IFACE: &str = "org.gnome.Shell.Extensions.Windows";

    let _ = conn.call_method(Some(SHELL), PATH, Some(IFACE), method, &(id,));
}

/// Put the window on top until the user focuses it (or two minutes pass).
#[cfg(all(unix, not(target_os = "macos")))]
fn gnome_raise() {
    if !gnome_available() {
        return;
    }
    // The D-Bus round trips run off the UI thread: a wedged shell must never
    // freeze the event loop.
    std::thread::spawn(|| {
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return;
        };
        // The surface is recreated on a later event-loop turn; wait for it to
        // show up in the shell's window list before raising it.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let id = loop {
            if let Some(id) = gnome_window_id(&conn) {
                break id;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        };
        gnome_call(&conn, "MakeAbove", id);
        // Stay above only until the user actually focuses the window; if that
        // never happens, drop the flag after two minutes rather than leaving
        // the window always-on-top forever.
        let give_up = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < give_up {
            std::thread::sleep(Duration::from_secs(1));
            let Some(window) = gnome_windows(&conn).and_then(|windows| {
                windows
                    .into_iter()
                    .find(|w| w["id"].as_u64() == Some(id as u64))
            }) else {
                return;
            };
            if window["focus"].as_bool().unwrap_or(false) {
                gnome_call(&conn, "UnmakeAbove", id);
                return;
            }
        }
        gnome_call(&conn, "UnmakeAbove", id);
    });
}

/// Drop the temporary always-on-top flag (window closed or hidden).
pub fn release_above() {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if !gnome_available() {
            return;
        }
        std::thread::spawn(|| {
            let Ok(conn) = zbus::blocking::Connection::session() else {
                return;
            };
            if let Some(id) = gnome_window_id(&conn) {
                gnome_call(&conn, "UnmakeAbove", id);
            }
        });
    }
}

/// Un-minimize and ask for focus; `false` = no winit window exists (yet).
fn raise_now(window: &slint::Window) -> bool {
    use slint::winit_030::WinitWindowAccessor;

    window
        .with_winit_window(|winit_window| {
            winit_window.set_minimized(false);
            // winit's `focus_window` is a no-op on Wayland; its user-attention
            // request goes through xdg-activation, which is the only way a
            // background app can ask GNOME to raise/focus the window.
            #[cfg(all(unix, not(target_os = "macos")))]
            if wayland_session() {
                winit_window.request_user_attention(Some(
                    slint::winit_030::winit::window::UserAttentionType::Critical,
                ));
            }
            winit_window.focus_window();
        })
        .is_some()
}

/// Does this session actually have a tray host? On Linux the icon can only
/// appear when a StatusNotifierWatcher is on the bus (GNOME needs the
/// AppIndicator extension); Windows and macOS always have one.
pub fn available() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dbus::watcher_present()
    }
    #[cfg(windows)]
    {
        // A headless/service session has no shell and would refuse the icon;
        // close-to-tray must not hide the window into nothing there.
        win::shell_available()
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
}

/// Ask the already-running instance to surface its window. `false` = no
/// running UI endpoint (the caller then shows the "already running" toast).
pub fn summon_existing() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dbus::summon()
    }
    #[cfg(windows)]
    {
        win::summon()
    }
    #[cfg(target_os = "macos")]
    {
        false
    }
}

/// Start the endpoint a second launch talks to. Only meaningful while the
/// tray exists: it is what keeps a `--silent`/close-to-tray window reachable.
pub fn start_endpoint(window: slint::Weak<crate::AppWindow>) {
    #[cfg(all(unix, not(target_os = "macos")))]
    dbus::start_endpoint(window);
    #[cfg(windows)]
    win::start_endpoint(window);
    #[cfg(target_os = "macos")]
    {
        let _ = window;
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod dbus {
    use std::thread;

    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    const UI_SERVICE: &str = "org.aether.UI";
    const UI_PATH: &str = "/org/aether/UI";

    pub fn watcher_present() -> bool {
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return false;
        };
        let Ok(reply) = conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(WATCHER,),
        ) else {
            return false;
        };
        let body = reply.body();
        body.deserialize::<bool>().unwrap_or(false)
    }

    struct Ui {
        window: slint::Weak<crate::AppWindow>,
    }

    #[zbus::interface(name = "org.aether.UI")]
    impl Ui {
        fn activate(&self) {
            let window = self.window.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = window.upgrade() {
                    crate::tray::present(&ui);
                }
            });
        }
    }

    pub fn start_endpoint(window: slint::Weak<crate::AppWindow>) {
        thread::spawn(move || {
            let Ok(conn) = zbus::blocking::Connection::session() else {
                return;
            };
            if conn.object_server().at(UI_PATH, Ui { window }).is_err() {
                return;
            }
            if conn.request_name(UI_SERVICE).is_err() {
                return;
            }
            loop {
                thread::park();
            }
        });
    }

    pub fn summon() -> bool {
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return false;
        };
        conn.call_method(
            Some(UI_SERVICE),
            UI_PATH,
            Some("org.aether.UI"),
            "Activate",
            &(),
        )
        .is_ok()
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::thread;

    type Handle = *mut c_void;

    const SUMMON_EVENT: &str = "aether-ui-summon";
    const EVENT_MODIFY_STATE: u32 = 0x0002;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateEventW(
            attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> Handle;
        fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
        fn SetEvent(event: Handle) -> i32;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn start_endpoint(window: slint::Weak<crate::AppWindow>) {
        let name = wide(SUMMON_EVENT);
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if event.is_null() {
            eprintln!(
                "aether: no second-launch summon ({})",
                std::io::Error::last_os_error()
            );
            return;
        }
        // Raw handles are not `Send`; the integer round-trip is what lets the
        // waiter live on its own thread.
        let event = event as isize;
        thread::spawn(move || {
            let event = event as Handle;
            loop {
                if unsafe { WaitForSingleObject(event, INFINITE) } != WAIT_OBJECT_0 {
                    return;
                }
                let window = window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = window.upgrade() {
                        crate::tray::present(&ui);
                    }
                });
            }
        });
    }

    /// Is a shell (notification area) present in this session?
    pub fn shell_available() -> bool {
        #[link(name = "user32")]
        extern "system" {
            fn FindWindowW(class_name: *const u16, window_name: *const u16) -> Handle;
        }
        let class = wide("Shell_TrayWnd");
        !unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) }.is_null()
    }

    pub fn summon() -> bool {
        let name = wide(SUMMON_EVENT);
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if event.is_null() {
            return false;
        }
        let set = unsafe { SetEvent(event) } != 0;
        let _ = unsafe { CloseHandle(event) };
        set
    }
}
