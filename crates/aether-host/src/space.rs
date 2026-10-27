//! Free-space preflight.
//!
//! A download that dies halfway because the disk filled up wastes the user's
//! time and leaves a partial file. Checking up front turns that into one clear
//! error at add time. The check is advisory: the filesystem can still fill up
//! from other writers, which is why the plugins also map `ENOSPC` to
//! `ErrorKind::DiskFull`.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpaceError {
    #[error("cannot check free space in {}: {source}", dir.display())]
    Probe {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "not enough space in {}: need {}, {} free",
        dir.display(),
        human_bytes(*needed),
        human_bytes(*available)
    )]
    NotEnough {
        dir: PathBuf,
        needed: u64,
        available: u64,
    },
}

impl SpaceError {
    /// True when the directory is fine but simply too small.
    pub fn is_shortage(&self) -> bool {
        matches!(self, Self::NotEnough { .. })
    }
}

/// Ensure `dir` has at least `needed` bytes available.
pub fn ensure_space(dir: &Path, needed: u64) -> Result<u64, SpaceError> {
    let available = fs2::available_space(dir).map_err(|source| SpaceError::Probe {
        dir: dir.to_path_buf(),
        source,
    })?;
    if needed > available {
        return Err(SpaceError::NotEnough {
            dir: dir.to_path_buf(),
            needed,
            available,
        });
    }
    Ok(available)
}

/// `1.2 GiB` style formatting for user-facing messages.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn detects_a_shortage_without_touching_the_disk() {
        let dir = std::env::temp_dir();
        let err = ensure_space(&dir, u64::MAX / 2).unwrap_err();
        assert!(err.is_shortage());
        let message = err.to_string();
        assert!(message.contains("not enough space"), "{message}");
        assert!(message.contains("need "));

        // Ordinary requests fit in the temp directory.
        assert!(ensure_space(&dir, 1024).is_ok());
    }

    #[test]
    fn missing_directories_are_reported_as_probe_errors() {
        let missing = std::env::temp_dir().join("aether-space-does-not-exist");
        std::fs::remove_dir_all(&missing).ok();
        let err = ensure_space(&missing, 1).unwrap_err();
        assert!(!err.is_shortage());
    }
}
