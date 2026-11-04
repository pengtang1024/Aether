//! OS integration: open files/URLs, desktop notifications, theme.

use std::process::Command;

pub(crate) fn open_path(path: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/c", "start", ""]);
        command.arg(path);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Open an http(s) address with the desktop's default handler. Used by the
/// About panel; returns false when no opener could be started.
pub(crate) fn open_url(url: &str) -> bool {
    open_path(std::path::Path::new(url))
}

/// Best-effort desktop notification. Linux uses `notify-send` (libnotify),
/// macOS a one-line AppleScript, Windows a WinRT toast through notify-rust
/// (the one platform whose toast API is not reachable via an external
/// command). Every path tolerates refusal: at most one log line, no panic.
pub(crate) fn send_notification(title: &str, body: &str) {
    #[cfg(windows)]
    {
        let mut notification = notify_rust::Notification::new();
        notification.summary(title).body(body);
        // Toasts are keyed by an AppUserModelID. Register ours under HKCU
        // on first use and attribute the toast to Aether; if registration
        // is refused, leave notify-rust's own fallback in place —
        // misattributed beats missing.
        if ensure_app_id() {
            notification.app_id(AUMID);
        }
        if let Err(error) = notification.show() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!("aether: windows toast failed ({error})");
            }
        }
        return;
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        let mut command = {
            let mut command = Command::new("notify-send");
            command.arg("Aether").arg(title).arg(body);
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            // AppleScript string escaping: backslash and double quote.
            let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(
                "display notification \"{}\" with title \"Aether {}\"",
                escape(body),
                escape(title)
            );
            let mut command = Command::new("osascript");
            command.args(["-e", &script]);
            command
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let _ = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// AppUserModelID Windows toasts are attributed to. Registered under HKCU
/// on first use (see [`ensure_app_id`]) — no installer step, no admin.
#[cfg(windows)]
const AUMID: &str = "com.pengtang1024.Aether";

/// Register [`AUMID`] once per process; `true` = Aether attribution is
/// available. `reg add` creates the key itself, and HKCU needs no admin —
/// the same external-command philosophy as notify-send/osascript above.
/// A refusal is cached too: notifications keep working (notify-rust falls
/// back to its own registered id) without re-running reg every toast.
#[cfg(windows)]
pub(crate) fn ensure_app_id() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    /// 0 = untried, 1 = registered, 2 = refused.
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{AUMID}");
    let registered = reg_add(&key, "DisplayName", "Aether");
    if registered {
        // Icon beside the exe when packaged; cosmetic, so a failure (or a
        // dev tree without logo.ico) changes nothing.
        if let Ok(icon) = std::env::current_exe().map(|exe| exe.with_file_name("logo.ico")) {
            if icon.is_file() {
                let _ = reg_add(&key, "IconUri", &icon.to_string_lossy());
            }
        }
    }
    STATE.store(if registered { 1 } else { 2 }, Ordering::Relaxed);
    registered
}

/// One `reg add` write; `false` when reg.exe is unavailable or refuses.
#[cfg(windows)]
pub(crate) fn reg_add(key: &str, value: &str, data: &str) -> bool {
    std::process::Command::new("reg")
        .args(["add", key, "/v", value, "/t", "REG_SZ", "/d", data, "/f"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Resolve the desktop's light/dark preference without pulling in a
/// platform-specific theme crate. Unknown systems fall back to light.
pub(crate) fn system_is_dark() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            let value = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if value.contains("prefer-dark") {
                return true;
            }
            if value.contains("prefer-light") {
                return false;
            }
            if value.contains("default") {
                if let Ok(theme) = Command::new("gsettings")
                    .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
                    .output()
                {
                    return String::from_utf8_lossy(&theme.stdout)
                        .to_lowercase()
                        .contains("dark");
                }
            }
        }
        if let Ok(theme) = std::env::var("GTK_THEME") {
            return theme.to_lowercase().contains("dark");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
        {
            return String::from_utf8_lossy(&output.stdout).contains("0x0");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            return String::from_utf8_lossy(&output.stdout).contains("Dark");
        }
    }

    false
}

#[cfg(test)]
mod tests {
    /// CI runs this on a headless runner: the HKCU registration must really
    /// land in the registry (that part needs no desktop), the toast itself
    /// is best-effort — a session without a shell can refuse it — and the
    /// whole path must never panic.
    #[cfg(windows)]
    #[test]
    fn toast_registers_the_aumid_and_never_panics() {
        super::send_notification("Aether test", "hello from CI");
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{}", super::AUMID);
        let status = std::process::Command::new("reg")
            .args(["query", &key, "/v", "DisplayName"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("reg query must run");
        assert!(status.success(), "AUMID must be registered under HKCU");
    }
}
