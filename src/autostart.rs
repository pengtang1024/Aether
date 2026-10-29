//! "Start Aether at login", per platform.
//!
//! The enabled state lives in the OS (XDG autostart entry, HKCU Run value,
//! LaunchAgent plist), never in `ui.json`: the switch reads the real state
//! at startup, so removing the entry by hand is reflected honestly.
//!
//! macOS is implemented as a LaunchAgent but this repo's macOS CI is
//! check-only — the plist shape is unit-tested, runtime behaviour is not.

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn dir() -> Option<PathBuf> {
        let base = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(home).join(".config"))
            })?;
        Some(base.join("autostart"))
    }

    /// The desktop entry; `Exec` follows the spec's quoting rule so paths
    /// with spaces survive as a single argument.
    fn entry(exe: &Path) -> String {
        let exe = exe.display().to_string();
        let exec = if exe.contains(' ') || exe.contains('"') {
            format!("\"{}\"", exe.replace('\\', "\\\\").replace('"', "\\\""))
        } else {
            exe
        };
        format!(
            "[Desktop Entry]\nType=Application\nName=Aether\nComment=Aether download manager\nExec={exec}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        )
    }

    pub fn is_enabled() -> bool {
        dir()
            .map(|dir| dir.join("aether.desktop").is_file())
            .unwrap_or(false)
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let dir = dir().ok_or("no config directory")?;
        set_in(&dir, enabled)
    }

    fn set_in(dir: &Path, enabled: bool) -> Result<(), String> {
        let path = dir.join("aether.desktop");
        if enabled {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            fs::create_dir_all(dir).map_err(|error| error.to_string())?;
            fs::write(&path, entry(&exe)).map_err(|error| error.to_string())
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn autostart_entry_round_trips_in_a_temp_dir() {
            let dir = std::env::temp_dir().join(format!("aether-autostart-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);

            set_in(&dir, true).expect("enable");
            let text = fs::read_to_string(dir.join("aether.desktop")).expect("entry written");
            assert!(text.starts_with("[Desktop Entry]"), "{text}");
            assert!(text.contains("Type=Application"));
            assert!(text.contains("Exec="));

            set_in(&dir, false).expect("disable");
            assert!(!dir.join("aether.desktop").exists());

            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn exec_is_quoted_when_the_path_has_spaces() {
            let entry = entry(Path::new("/opt/My Apps/aether"));
            assert!(entry.contains("Exec=\"/opt/My Apps/aether\""), "{entry}");
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::process::{Command, Stdio};

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Aether";

    pub fn is_enabled() -> bool {
        reg(&["query", KEY, "/v", VALUE]).is_some()
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        if enabled {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            // Quoted so a path with spaces survives the login launch.
            let data = format!("\"{}\"", exe.display());
            reg(&["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &data, "/f"])
                .ok_or("reg add failed")?;
            Ok(())
        } else if is_enabled() {
            reg(&["delete", KEY, "/v", VALUE, "/f"]).ok_or("reg delete failed")?;
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Run `reg`; `Some(true)` only on a successful exit.
    fn reg(args: &[&str]) -> Option<bool> {
        Command::new("reg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .map(|status| status.success())
            .filter(|ok| *ok)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The Run value must really be written and removed. CI runs this on
        /// an ephemeral runner; `reg` needs no desktop session.
        #[test]
        fn autostart_value_round_trips_through_the_registry() {
            set(true).expect("enable");
            assert!(is_enabled(), "Run value must exist after enable");
            set(false).expect("disable");
            assert!(!is_enabled(), "Run value must be gone after disable");
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};

    const LABEL: &str = "com.pengtang1024.Aether";

    fn dir() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
    }

    fn plist(exe: &Path) -> String {
        let exe = xml_escape(&exe.display().to_string());
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key><string>{LABEL}</string>\n\
             \t<key>ProgramArguments</key><array><string>{exe}</string></array>\n\
             \t<key>RunAtLoad</key><true/>\n\
             </dict>\n\
             </plist>\n"
        )
    }

    fn xml_escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    pub fn is_enabled() -> bool {
        dir()
            .map(|dir| dir.join(format!("{LABEL}.plist")).is_file())
            .unwrap_or(false)
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let dir = dir().ok_or("no home directory")?;
        let path = dir.join(format!("{LABEL}.plist"));
        if enabled {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            fs::write(&path, plist(&exe)).map_err(|error| error.to_string())
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn launch_agent_names_the_exe_and_label() {
            let plist = plist(Path::new("/Applications/Aether & Co/aether"));
            assert!(plist.contains("<key>Label</key><string>com.pengtang1024.Aether</string>"));
            assert!(plist.contains("Aether &amp; Co"), "{plist}");
            assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        }
    }
}

/// Is start-at-login currently registered with the OS?
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

/// Register or remove start-at-login. `Err` carries the reason so the UI
/// can bounce the switch back and say why instead of pretending.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    imp::set(enabled)
}
