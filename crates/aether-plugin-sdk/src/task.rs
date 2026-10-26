//! Shared task control and progress state for worker-based plugins.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const RUNNING: u8 = 0;
const PAUSED: u8 = 1;
const CANCELED: u8 = 2;

/// Pause/resume/cancel flag shared by the plugin command loop and workers.
pub struct TaskControl {
    mode: AtomicU8,
}

impl TaskControl {
    pub fn new() -> Self {
        Self {
            mode: AtomicU8::new(RUNNING),
        }
    }

    pub fn pause(&self) {
        let _ = self
            .mode
            .compare_exchange(RUNNING, PAUSED, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        let _ = self
            .mode
            .compare_exchange(PAUSED, RUNNING, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.mode.store(CANCELED, Ordering::SeqCst);
    }

    pub fn is_canceled(&self) -> bool {
        self.mode.load(Ordering::SeqCst) == CANCELED
    }

    pub fn is_paused(&self) -> bool {
        self.mode.load(Ordering::SeqCst) == PAUSED
    }

    /// Blocks while paused. Returns false if the task was canceled.
    pub fn wait_until_running(&self) -> bool {
        loop {
            match self.mode.load(Ordering::SeqCst) {
                RUNNING => return true,
                PAUSED => thread::sleep(Duration::from_millis(50)),
                _ => return false,
            }
        }
    }
}

impl Default for TaskControl {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProgressSnapshot {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed_bps: u64,
    pub connections: u32,
    pub chunks_done: u32,
    pub chunks_total: u32,
    pub seq: u64,
}

pub type SharedProgress = Arc<Mutex<ProgressSnapshot>>;

pub fn shared_progress() -> SharedProgress {
    Arc::new(Mutex::new(ProgressSnapshot::default()))
}
