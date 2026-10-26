//! Persistent HTTP resume state.
//!
//! One state file lives in the task's staging directory. It records the byte
//! ranges assigned to each connection and how many bytes of each range are
//! already on disk. The file is written atomically and refreshed while the
//! download runs, so a `kill -9` loses at most the last refresh interval.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const STATE_FILE: &str = ".aether-http-state.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeState {
    pub url: String,
    #[serde(default)]
    pub etag: Option<String>,
    /// Total size in bytes; `0` means the server did not report a size.
    pub total: u64,
    pub filename: String,
    pub supports_range: bool,
    pub ranges: Vec<RangeState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeState {
    pub start: u64,
    pub end: u64,
    /// Bytes already written inside this range.
    pub done: u64,
}

impl RangeState {
    pub fn remaining(&self) -> u64 {
        if self.is_complete() {
            return 0;
        }
        self.end
            .saturating_sub(self.start + self.done)
            .saturating_add(1)
    }

    pub fn is_complete(&self) -> bool {
        self.start + self.done > self.end
    }

    pub fn next_offset(&self) -> u64 {
        self.start + self.done
    }
}

impl ResumeState {
    pub fn new(
        url: impl Into<String>,
        etag: Option<String>,
        total: u64,
        filename: impl Into<String>,
        supports_range: bool,
        connections: u32,
    ) -> Self {
        let ranges = if total > 0 {
            split_ranges(total, connections)
        } else {
            vec![RangeState {
                start: 0,
                end: u64::MAX,
                done: 0,
            }]
        };

        Self {
            url: url.into(),
            etag,
            total,
            filename: filename.into(),
            supports_range,
            ranges,
        }
    }

    /// Whether this state can be resumed for the given URL/ETag/size.
    pub fn matches(&self, url: &str, etag: Option<&str>, total: Option<u64>) -> bool {
        self.url == url
            && self.etag.as_deref() == etag
            && total.is_none_or(|total| self.total == total)
            && !self.ranges.is_empty()
            && self
                .ranges
                .iter()
                .all(|range| range.done <= range.end - range.start + 1)
    }

    pub fn downloaded(&self) -> u64 {
        self.ranges.iter().map(|range| range.done).sum()
    }

    pub fn is_complete(&self) -> bool {
        !self.ranges.is_empty() && self.ranges.iter().all(RangeState::is_complete)
    }
}

pub fn split_ranges(total: u64, connections: u32) -> Vec<RangeState> {
    // Do not split tiny files into more ranges than useful: one connection
    // should own at least 64 KiB where possible.
    let useful = total.div_ceil(64 * 1024).max(1) as u32;
    let count = connections.clamp(1, 16).min(useful) as u64;
    let chunk = total.div_ceil(count).max(1);
    let mut ranges = Vec::new();
    let mut start = 0_u64;

    while start < total {
        let end = (start + chunk - 1).min(total - 1);
        ranges.push(RangeState {
            start,
            end,
            done: 0,
        });
        start = end + 1;
    }

    ranges
}

pub fn state_path(staging: &Path) -> PathBuf {
    staging.join(STATE_FILE)
}

pub fn read(staging: &Path) -> Result<Option<ResumeState>, StateError> {
    let path = state_path(staging);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StateError::Io(error)),
    }
}

pub fn write(staging: &Path, state: &ResumeState) -> Result<(), StateError> {
    let path = state_path(staging);
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    fs::write(&temp, bytes)?;

    // POSIX rename replaces atomically. Windows refuses an existing target.
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temp, &path)?;
    Ok(())
}

pub fn remove(staging: &Path) -> Result<(), StateError> {
    match fs::remove_file(state_path(staging)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StateError::Io(error)),
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state io error: {0}")]
    Io(#[from] io::Error),

    #[error("state json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_ranges_without_gaps_or_overlap() {
        let total = 1024 * 1024;
        let ranges = split_ranges(total, 4);
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[3].end, total - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
        assert_eq!(ranges.iter().map(|r| r.remaining()).sum::<u64>(), total);
    }

    #[test]
    fn small_file_uses_one_range() {
        let ranges = split_ranges(3, 8);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].end, 2);
    }

    #[test]
    fn roundtrips_state() {
        let root = std::env::temp_dir().join(format!("aether-http-state-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut state =
            ResumeState::new("https://example.com/a", Some("e".into()), 100, "a", true, 2);
        state.ranges[0].done = 10;
        write(&root, &state).unwrap();
        let loaded = read(&root).unwrap().unwrap();
        assert_eq!(loaded, state);
        assert_eq!(loaded.downloaded(), 10);
        remove(&root).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unknown_size_has_open_ended_range() {
        let state = ResumeState::new("https://example.com/a", None, 0, "a", false, 4);
        assert_eq!(state.ranges.len(), 1);
        assert_eq!(state.ranges[0].end, u64::MAX);
    }
}
