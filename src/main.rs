//! Aether — a native Material 3 download manager.
//!
//! Rust bridges the Slint window to `aether-daemon` (`src/backend.rs`); Slint
//! renders everything with its own Material 3 widget set. There is no WebView
//! anywhere in the stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod autostart;
mod backend;
mod chart;
mod clipboard;
mod detail;
mod format;
mod i18n;
mod instance;
mod launch;
mod model;
mod platform;
mod plugin_settings;
mod plugins;
mod prefs;
mod rows;
mod settings_search;
mod state;
mod status_text;
mod toast;
mod tray;
mod update_status;
mod wiring;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    app::run()
}
