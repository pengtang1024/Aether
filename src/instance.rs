//! Single-instance guard for the desktop app.
//!
//! Two UIs would each try to spawn a daemon and fight over the tray; the
//! first process owns a lock file in the config directory and a second
//! launch reports "already running" and exits. The lock is advisory
//! (`flock`/`LockFileEx` through fs2) and released by the OS when the
//! process dies, so a crash never leaves a stale lock behind.
//!
//! Known limit: a second launch does not summon the existing window (that
//! needs a UI-level IPC channel); it notifies instead and exits.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// Outcome of the single-instance check.
pub enum Acquired {
    /// This process owns the lock, held until the process exits.
    Primary(Guard),
    /// Another process owns it.
    AlreadyRunning,
    /// No lock could be prepared (no config dir, a filesystem without
    /// locking): the app runs anyway — running twice beats not running.
    Unlocked,
}

/// Keeps the lock for as long as it lives; dropping it releases the lock.
pub struct Guard {
    _file: File,
}

/// Try to become the one running instance.
pub fn acquire() -> Acquired {
    let Some(path) = lock_path() else {
        return Acquired::Unlocked;
    };
    acquire_at(&path)
}

fn lock_path() -> Option<PathBuf> {
    let path = crate::theme_config_path()?.with_file_name("instance.lock");
    fs::create_dir_all(path.parent()?).ok()?;
    Some(path)
}

fn acquire_at(path: &Path) -> Acquired {
    let Ok(file) = OpenOptions::new().create(true).write(true).open(path) else {
        return Acquired::Unlocked;
    };
    match file.try_lock_exclusive() {
        Ok(()) => Acquired::Primary(Guard { _file: file }),
        Err(error) if is_lock_conflict(&error) => Acquired::AlreadyRunning,
        // Anything else (e.g. a filesystem without locking support) must
        // not stop the app from starting.
        Err(_) => Acquired::Unlocked,
    }
}

fn is_lock_conflict(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    if error.raw_os_error() == Some(33) {
        // ERROR_LOCK_VIOLATION: fs2 reports the raw code on Windows.
        return true;
    }
    error.kind() == std::io::ErrorKind::WouldBlock
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_guard_is_refused_until_the_first_drops() {
        let dir = std::env::temp_dir().join(format!("aether-instance-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("instance.lock");

        let first = acquire_at(&path);
        assert!(matches!(&first, Acquired::Primary(_)), "first acquire wins");
        let second = acquire_at(&path);
        assert!(
            matches!(&second, Acquired::AlreadyRunning),
            "second acquire must be refused"
        );

        drop(first);
        let third = acquire_at(&path);
        assert!(
            matches!(&third, Acquired::Primary(_)),
            "dropping the guard releases the lock"
        );

        drop(third);
        let _ = fs::remove_dir_all(&dir);
    }
}
