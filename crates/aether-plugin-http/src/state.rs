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

/// Format version of the state file. `0` (absent) is the legacy static split,
/// which loaded as a plain range list and is migrated on the first write.
pub const VERSION: u32 = 1;

/// One unit of work: the downloader enqueues chunks of this size and workers
/// claim them one at a time, so a slow chunk no longer pins a whole range.
pub const CHUNK_BYTES: u64 = 16 * 1024 * 1024;
/// Smallest useful slice. Below this a request costs more than it reveals
/// about the source, and small files keep one chunk per 64 KiB anyway.
const MIN_CHUNK_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeState {
    /// Written as `1` by this version. Legacy files have no field and load as
    /// `0`; [`ResumeState::normalize`] splits their oversized ranges.
    #[serde(default)]
    pub version: u32,
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
    pub(crate) fn len(&self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }

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
            plan_chunks(total, connections)
        } else {
            vec![RangeState {
                start: 0,
                end: u64::MAX,
                done: 0,
            }]
        };

        Self {
            version: VERSION,
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
            && self.invariant_error().is_none()
    }

    /// Resume safety check: the ranges must tile `[0, total)` exactly once,
    /// with `done` inside each range. A gap would silently leave bytes at zero
    /// and an overlap would make two workers write the same offset, so any
    /// violation means the whole state and partial file must be discarded.
    pub fn invariant_error(&self) -> Option<String> {
        if self.ranges.is_empty() {
            return Some("state has no ranges".into());
        }
        for range in &self.ranges {
            if range.done > range.len() {
                return Some(format!(
                    "range {}-{} has done={} beyond its length",
                    range.start, range.end, range.done
                ));
            }
        }
        if self.total == 0 {
            // Unknown size: only the single open-ended range is meaningful.
            return (self.ranges.len() != 1)
                .then(|| format!("open-ended state has {} ranges", self.ranges.len()));
        }
        // Splits append their tail to the end of the list, so coverage is
        // checked on a start-sorted view rather than on insertion order.
        let mut sorted: Vec<&RangeState> = self.ranges.iter().collect();
        sorted.sort_by_key(|range| range.start);
        if sorted[0].start != 0 {
            return Some(format!("first range starts at {}", sorted[0].start));
        }
        if sorted
            .last()
            .is_some_and(|range| range.end + 1 != self.total)
        {
            return Some(format!(
                "last range ends at {} but total is {}",
                sorted.last().unwrap().end,
                self.total
            ));
        }
        for pair in sorted.windows(2) {
            if pair[0].end + 1 != pair[1].start {
                return Some(format!(
                    "ranges {}-{} and {}-{} are not contiguous",
                    pair[0].start, pair[0].end, pair[1].start, pair[1].end
                ));
            }
        }
        None
    }

    /// Split any oversized range into [`CHUNK_BYTES`] chunks. Legacy state
    /// files (version 0, static split) are migrated on load; the operation is
    /// idempotent, so it also runs on every resume for safety.
    pub fn normalize(&mut self) {
        self.version = VERSION;
        if self.total == 0 {
            return;
        }
        let mut ranges = Vec::with_capacity(self.ranges.len());
        for range in std::mem::take(&mut self.ranges) {
            if range.is_complete() || range.len() <= CHUNK_BYTES {
                ranges.push(range);
                continue;
            }
            let mut start = range.start;
            let mut done = range.done;
            while start <= range.end {
                let end = start.saturating_add(CHUNK_BYTES - 1).min(range.end);
                let len = end - start + 1;
                ranges.push(RangeState {
                    start,
                    end,
                    done: done.min(len),
                });
                done = done.saturating_sub(len);
                start = end + 1;
            }
        }
        self.ranges = ranges;
    }

    pub fn downloaded(&self) -> u64 {
        self.ranges.iter().map(|range| range.done).sum()
    }

    pub fn is_complete(&self) -> bool {
        !self.ranges.is_empty() && self.ranges.iter().all(RangeState::is_complete)
    }
}

/// Chunk plan for a new download: 16 MiB units, shrinking for small files so
/// a short download still has enough chunks to keep every worker busy.
pub fn plan_chunks(total: u64, connections: u32) -> Vec<RangeState> {
    let connections = u64::from(connections.clamp(1, 16));
    // ~8 chunks per worker give the queue slack for stealing without turning
    // a 1 GiB download into thousands of requests.
    let useful = (total / MIN_CHUNK_BYTES).max(1);
    let count = (connections * 8).min(useful);
    let target = (total / count).clamp(MIN_CHUNK_BYTES, CHUNK_BYTES);
    let mut ranges = Vec::new();
    let mut start = 0_u64;

    while start < total {
        let end = start.saturating_add(target - 1).min(total - 1);
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
    fn chunk_plan_tiles_the_file_without_gaps_or_overlap() {
        let total = 512 * 1024 * 1024;
        let ranges = plan_chunks(total, 4);
        assert_eq!(ranges.len(), 32);
        assert!(ranges.iter().all(|range| range.len() == CHUNK_BYTES));
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[31].end, total - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
        assert_eq!(ranges.iter().map(|r| r.remaining()).sum::<u64>(), total);
    }

    #[test]
    fn small_files_get_smaller_chunks() {
        // 40 MiB with four workers: 32 chunks of 1.25 MiB keep the queue fed.
        let ranges = plan_chunks(40 * 1024 * 1024, 4);
        assert_eq!(ranges.len(), 32);
        assert!(ranges.iter().all(|range| range.len() == 1_310_720));
        // A 512 KiB fixture still splits into 64 KiB chunks, as before.
        let small = plan_chunks(512 * 1024, 8);
        assert_eq!(small.len(), 8);
        assert!(small.iter().all(|range| range.len() == MIN_CHUNK_BYTES));
        let tiny = plan_chunks(3, 8);
        assert_eq!(tiny.len(), 1);
        assert_eq!(tiny[0].end, 2);
    }

    #[test]
    fn invariant_check_rejects_gaps_and_overlaps() {
        let mut state =
            ResumeState::new("https://example.com/a", Some("e".into()), 100, "a", true, 2);
        state.ranges = vec![
            RangeState {
                start: 0,
                end: 49,
                done: 10,
            },
            RangeState {
                start: 60,
                end: 99,
                done: 0,
            },
        ];
        assert!(state.invariant_error().unwrap().contains("not contiguous"));

        state.ranges[1].start = 50;
        state.ranges[0].end = 49;
        assert!(state.invariant_error().is_none());

        state.ranges[0].end = 51;
        assert!(state.invariant_error().unwrap().contains("not contiguous"));

        state.ranges[0].done = 100;
        assert!(state
            .invariant_error()
            .unwrap()
            .contains("beyond its length"));
    }

    #[test]
    fn invariant_check_rejects_short_or_shifted_coverage() {
        let state = ResumeState::new("https://example.com/a", None, 100, "a", true, 2);
        assert!(state.invariant_error().is_none());

        let mut shifted = state.clone();
        shifted.ranges[0].start = 5;
        assert!(shifted.invariant_error().unwrap().contains("first range"));

        let mut short = state.clone();
        short.ranges.last_mut().unwrap().end -= 1;
        assert!(short.invariant_error().unwrap().contains("last range"));
    }

    /// Legacy files (no `version`, static 128 MiB ranges) must load and turn
    /// into 16 MiB chunks without losing the already-written prefix.
    #[test]
    fn legacy_state_loads_and_migrates_to_chunks() {
        let json = serde_json::json!({
            "url": "https://example.com/a",
            "etag": "e",
            "total": 256 * 1024 * 1024_u64,
            "filename": "a",
            "supports_range": true,
            "ranges": [
                {"start": 0, "end": 128 * 1024 * 1024_u64 - 1, "done": 40 * 1024 * 1024_u64},
                {"start": 128 * 1024 * 1024_u64, "end": 256 * 1024 * 1024_u64 - 1, "done": 0}
            ]
        });
        let mut state: ResumeState = serde_json::from_value(json).unwrap();
        assert_eq!(state.version, 0, "legacy file has no version field");
        assert!(state.invariant_error().is_none());
        assert_eq!(state.downloaded(), 40 * 1024 * 1024);

        state.normalize();
        assert_eq!(state.version, VERSION);
        assert_eq!(state.ranges.len(), 16);
        assert!(state.ranges.iter().all(|range| range.len() == CHUNK_BYTES));
        assert!(state.invariant_error().is_none());
        assert_eq!(state.downloaded(), 40 * 1024 * 1024);
        assert_eq!(state.ranges[0].done, CHUNK_BYTES);
        assert_eq!(state.ranges[1].done, CHUNK_BYTES);
        assert_eq!(state.ranges[2].done, 8 * 1024 * 1024);
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut state = ResumeState::new(
            "https://example.com/a",
            None,
            512 * 1024 * 1024,
            "a",
            true,
            4,
        );
        let before = state.ranges.len();
        state.normalize();
        assert_eq!(state.ranges.len(), before);
        assert!(state.invariant_error().is_none());
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
