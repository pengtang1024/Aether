//! Process startup: single instance, window, preferences and event loop.
use slint::ComponentHandle;

use std::cell::RefCell;
use std::rc::Rc;

use crate::detail::open_detail;
use crate::model::Status;
use crate::platform::{send_notification, system_is_dark};
use crate::prefs::read_theme_mode;
use crate::rows::refresh_rows;
use crate::state::State;
use crate::{autostart, clipboard, i18n, instance, launch, tray, wiring, AppWindow};

pub(crate) fn run() -> Result<(), slint::PlatformError> {
    // The autostart entries pass `--silent`, so a login launch stays in the
    // tray without stealing focus. Manual launches have no flag and show the
    // window; the request is still gated on a usable tray further down.
    let silent = launch::requested_silent(
        std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned()),
    );

    // One UI per user: a second launch would race for the daemon and the
    // tray, so it reports and exits. The guard lives for the whole process
    // (dropping it releases the lock).
    let _instance = match instance::acquire() {
        instance::Acquired::AlreadyRunning => {
            // The primary instance exposes its window through the tray
            // endpoint; only when that endpoint is missing (no tray, no
            // session bus) does the old "already running" toast remain.
            if !tray::summon_existing() {
                send_notification(
                    &i18n::tr("Aether 已在运行", "Aether is already running"),
                    &i18n::tr(
                        "请使用已经打开的窗口",
                        "Use the window that is already open",
                    ),
                );
            }
            return Ok(());
        }
        other => other,
    };

    // Keep the login entry we own healthy: an update may have moved the
    // binary and entries written before this release lack `--silent`. A
    // missing entry stays missing — deleting it is the user's off switch.
    autostart::refresh();

    let ui = AppWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Bind the window to aether.desktop so the dock / task switcher uses the
    // app icon instead of a generic fallback. This must run before `run()`.
    let _ = slint::set_xdg_app_id("aether");

    let state = Rc::new(RefCell::new(State::new()));
    // Clipboard link watch: start the poller (None = no backend in this
    // session; the settings row reports that instead of faking a switch).
    {
        let mut st = state.borrow_mut();
        if let Some((watch, control)) = clipboard::start() {
            control.set_enabled(st.prefs.clipboard_watch);
            st.clipboard = Some(watch);
            st.clipboard_control = Some(control);
        }
    }

    // Theme: follow the desktop by default; AETHER_THEME can pin it for
    // screenshots and development (`light` or `dark`).
    ui.set_system_dark(system_is_dark());
    let env_theme = match std::env::var("AETHER_THEME").as_deref() {
        Ok("light") => Some(1),
        Ok("dark") => Some(2),
        _ => None,
    };
    if let Some(mode) = env_theme.or_else(read_theme_mode) {
        ui.set_theme_mode(mode);
    }
    // Rust-side strings follow UiPrefs.lang; must precede any translated
    // push (extension status, refresh labels) below.
    i18n::set_language(state.borrow().prefs.lang.as_str());

    // UI-local preferences: push into the window (which applies accent /
    // motion / flow through apply-appearance) and pick the start page.
    {
        let prefs = state.borrow().prefs.clone();
        ui.set_settings_accent(prefs.accent);
        ui.set_settings_motion(prefs.motion);
        ui.set_settings_flow(prefs.flow);
        ui.set_settings_start_page(prefs.start_page);
        ui.set_settings_after_done(prefs.after_done);
        ui.set_settings_notify_done(prefs.notify_done);
        ui.set_settings_notify_fail(prefs.notify_fail);
        ui.set_settings_clipboard(prefs.clipboard_watch);
        ui.set_settings_clipboard_available(state.borrow().clipboard_control.is_some());
        // Start-at-login lives in the OS, not in ui.json: show the real
        // registration instead of a remembered preference.
        ui.set_settings_autostart(autostart::is_enabled());
        let page = match prefs.start_page {
            1 => 1,
            2 => prefs.last_page.clamp(0, 2),
            _ => 0,
        };
        ui.set_page(page);
        state.borrow_mut().seen_page = page;
        // Chinese is the source language (index 0, no .po needed); "en"
        // selects the bundled translation. Runs before the first render
        // so the window never flashes the wrong language.
        if let Err(error) =
            slint::select_bundled_translation(if prefs.lang == "zh" { "" } else { &prefs.lang })
        {
            eprintln!("aether: language selection failed: {error:?}");
        }
        ui.set_settings_language(i18n::index_of(&prefs.lang) as i32);
    }

    // Initial data.
    refresh_rows(&ui, &state);

    // Screenshot helper: open a specific surface on startup so the mockup can
    // be captured in every state without driving the mouse.
    match std::env::var("AETHER_VIEW").as_deref() {
        Ok("settings") => {
            ui.set_page(2);
            // Which category to open, for the documentation screenshots.
            if let Ok(tab) = std::env::var("AETHER_SETTINGS_TAB") {
                if let Ok(tab) = tab.parse::<i32>() {
                    ui.set_settings_tab(tab);
                }
            }
        }
        Ok("downloads") => ui.set_page(1),
        Ok("dialog") => ui.set_dialog_open(true),
        Ok("drawer") => {
            ui.set_page(1);
            // The backend worker connects on its own thread, so the very first
            // snapshot is usually still empty here; poll briefly for a running
            // task instead of giving up before the subscription delivers one.
            // Prefer an active torrent: it exercises the chart and chunk grid.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            loop {
                let id = {
                    let st = state.borrow();
                    let snapshot = st.backend.snapshot();
                    snapshot
                        .tasks
                        .iter()
                        .find(|task| task.torrent && task.status == Status::Downloading)
                        .or_else(|| {
                            snapshot
                                .tasks
                                .iter()
                                .find(|task| task.status == Status::Downloading)
                        })
                        .map(|task| task.id)
                        .unwrap_or(-1)
                };
                if id >= 0 {
                    open_detail(&ui, &state, id);
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        _ => {}
    }

    let runtime = wiring::install(&ui, &state);

    // A silent start hides the window and leaves the tray as the way back.
    // If the session refused a tray icon (GNOME without AppIndicator, macOS,
    // a locked-down login session) hiding would strand the app with no entry
    // point, so the request degrades to a normal visible start and says why.
    if silent && !runtime.tray_up {
        eprintln!("aether: --silent requested but no system tray is available; showing the window");
    }
    // A visible tray keeps the event loop alive on its own, so closing the
    // window can hide it and the process stays resident until the tray's
    // 退出 calls quit_event_loop. Without a tray the window is the only entry
    // point and the default rule (last window closed = quit) stays in force.
    if !launch::hide_on_start(silent, runtime.tray_up) {
        ui.show()?;
    } else {
        // Never shown: nothing to paint, so park the clock and the poller
        // until the tray summons the window.
        wiring::suspend();
    }
    let result = slint::run_event_loop();
    drop(runtime);
    result
}
