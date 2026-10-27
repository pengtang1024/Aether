//! Aether's domain model.
//!
//! The app is a UI mockup, so downloads are simulated; this module holds the
//! task state the simulation operates on, plus every formatting helper the UI
//! needs. Keeping it independent of the generated Slint types makes the engine
//! easy to test and to swap for a real backend later.

use std::collections::VecDeque;

/// Number of speed samples kept per task (one per second).
pub const HISTORY_LEN: usize = 60;
/// Number of chunks (segments) drawn in the detail drawer.
pub const CHUNK_COUNT: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Downloading,
    Completed,
    Paused,
    Failed,
    Queued,
    Seeding,
}

impl Status {
    /// Status code shared with `ui/theme.slint` (`StatusColor.container`).
    pub fn code(self) -> i32 {
        match self {
            Status::Downloading => 0,
            Status::Completed => 1,
            Status::Paused => 2,
            Status::Failed => 3,
            Status::Queued => 4,
            Status::Seeding => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Downloading => "下载中",
            Status::Completed => "已完成",
            Status::Paused => "已暂停",
            Status::Failed => "失败",
            Status::Queued => "等待中",
            Status::Seeding => "做种中",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Status::Downloading | Status::Seeding)
    }
}

pub struct Task {
    pub id: i32,
    pub name: String,
    /// Where the file comes from (`releases.ubuntu.com`, `BT · 42 个节点`, …).
    pub source: String,
    pub path: String,
    pub note: String,
    pub total: u64,
    /// Bytes transferred so far. `f64` keeps sub-byte increments smooth.
    pub done: f64,
    pub status: Status,
    pub torrent: bool,
    /// Speed the simulation aims at, in bytes per second.
    pub nominal_speed: f64,
    pub speed: f64,
    pub history: VecDeque<f64>,
    /// `order[chunk] = rank`: chunks complete in this pseudo-random order.
    pub order: [u8; CHUNK_COUNT],
}

impl Task {
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.done / self.total as f64).clamp(0.0, 1.0) as f32
    }

    pub fn remaining_seconds(&self) -> Option<f64> {
        if !matches!(self.status, Status::Downloading) || self.speed <= 0.0 {
            return None;
        }
        let left = (self.total as f64 - self.done).max(0.0);
        Some(left / self.speed)
    }

    /// Fill ratio of one chunk, driven by the task's completion order.
    pub fn chunk_fill(&self, chunk: usize) -> f32 {
        let rank = self.order[chunk] as f32;
        (self.progress() * CHUNK_COUNT as f32 - rank).clamp(0.0, 1.0)
    }

    pub fn chunks(&self) -> Vec<f32> {
        (0..CHUNK_COUNT).map(|i| self.chunk_fill(i)).collect()
    }

    pub fn push_sample(&mut self, sample: f64) {
        if self.history.len() == HISTORY_LEN {
            self.history.pop_front();
        }
        self.history.push_back(sample);
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

pub fn format_size(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        "0 B/s".to_string()
    } else {
        format!("{}/s", format_size(bytes_per_sec))
    }
}

/// Compact ETA for the table: `3 分 12 秒`, `1 小时 04 分`, `—`.
pub fn format_eta(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "—".to_string();
    };
    let total = seconds.max(0.0).round() as u64;
    if total >= 3600 {
        format!("{} 小时 {:02} 分", total / 3600, (total % 3600) / 60)
    } else if total >= 60 {
        format!("{} 分 {:02} 秒", total / 60, total % 60)
    } else {
        format!("{total} 秒")
    }
}

/// Human readable size pair for the progress column: `1.2 GB / 6.0 GB`.
pub fn format_pair(done: f64, total: u64) -> String {
    format!("{} / {}", format_size(done), format_size(total as f64))
}
