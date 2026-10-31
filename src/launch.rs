//! Startup flags for the window bootstrap.
//!
//! `--silent` (alias `--minimized`) asks for a tray-only start: the daemon,
//! the backend and the tray come up as usual, but the main window stays
//! hidden. [`crate::autostart`] writes this flag into the login entry, so a
//! login launch never steals focus from whatever the user is doing.
//!
//! The flag is a request, not a guarantee: without a real tray icon the
//! window is the only way back into the app, so startup falls back to a
//! visible window instead of hiding the process out of reach.

/// Flags that request a tray-only start.
pub const SILENT_FLAGS: [&str; 2] = ["--silent", "--minimized"];

/// Exact-match flag test: `--silent=false`, `silent` or `--silent-extra`
/// are not our flag and must not hide the window.
pub fn is_silent_flag(arg: &str) -> bool {
    SILENT_FLAGS.contains(&arg)
}

/// Would this command line (without the program name) hide the window?
pub fn requested_silent<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| is_silent_flag(arg.as_ref()))
}

/// Hide the window at startup only when it was requested *and* the tray can
/// summon it again. The caller logs the degraded case.
pub fn hide_on_start(silent: bool, tray_up: bool) -> bool {
    silent && tray_up
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_flag_spellings_are_recognised() {
        assert!(requested_silent(["--silent"]));
        assert!(requested_silent(["--minimized"]));
        assert!(requested_silent(["aether", "--silent"]));
    }

    #[test]
    fn look_alikes_do_not_hide_the_window() {
        for arg in ["silent", "--silent=false", "--silent-extra", "-s", ""] {
            assert!(!is_silent_flag(arg), "{arg} must not count as --silent");
        }
        assert!(!requested_silent(Vec::<String>::new()));
    }

    #[test]
    fn hiding_requires_a_tray_to_come_back_from() {
        assert!(hide_on_start(true, true));
        assert!(!hide_on_start(true, false));
        assert!(!hide_on_start(false, true));
        assert!(!hide_on_start(false, false));
    }
}
