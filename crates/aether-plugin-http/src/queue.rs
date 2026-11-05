//! Work queue that hands fixed-size chunks to download workers.
//!
//! The queue replaces the static split: workers claim the next unclaimed
//! chunk instead of owning one range for the whole download, so a slow chunk
//! only delays the worker that holds it. When the queue runs dry and one
//! worker is clearly behind, its chunk is cut in half and the tail is pushed
//! back so an idle worker can pick it up.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::state::{RangeState, ResumeState};

/// A chunk is never split below this much remaining work: cutting a nearly
/// finished chunk only adds a request without creating parallelism.
const MIN_SPLIT_REMAINING: u64 = 8 * 1024 * 1024;
/// Split points are aligned to 1 MiB so resume ranges stay readable.
const SPLIT_ALIGN: u64 = 1024 * 1024;
/// Completed chunks needed before their median is considered meaningful.
const MEDIAN_SAMPLES: usize = 2;

pub struct WorkQueue {
    state: ResumeState,
    /// Chunk indices nobody is working on yet.
    pending: VecDeque<usize>,
    /// Parallel to `state.ranges`; a claimed chunk belongs to one worker.
    claimed: Vec<bool>,
    /// Whether a chunk was already cut in half (at most once per chunk).
    split: Vec<bool>,
    /// Wall time each unsplit chunk took, used for the 2x slowdown check.
    durations: Vec<Duration>,
}

impl WorkQueue {
    pub fn new(state: ResumeState) -> Self {
        let mut queue = Self {
            state,
            pending: VecDeque::new(),
            claimed: Vec::new(),
            split: Vec::new(),
            durations: Vec::new(),
        };
        queue.rebuild();
        queue
    }

    pub fn state(&self) -> &ResumeState {
        &self.state
    }

    /// Mutable access for per-chunk progress. Chunk boundaries only change
    /// through [`WorkQueue::split`], which keeps the claim bookkeeping in sync.
    pub fn state_mut(&mut self) -> &mut ResumeState {
        &mut self.state
    }

    /// Re-enqueue every incomplete range. Called at the start of each mirror
    /// attempt: claims from the previous source are meaningless, and ranges a
    /// split appended must be queued again for the new attempt.
    pub fn rebuild(&mut self) {
        self.pending.clear();
        self.claimed = vec![false; self.state.ranges.len()];
        self.split = vec![false; self.state.ranges.len()];
        self.durations.clear();
        for (index, range) in self.state.ranges.iter().enumerate() {
            if !range.is_complete() {
                self.pending.push_back(index);
            }
        }
    }

    /// Next chunk for a worker, or `None` when everything is claimed or done.
    pub fn claim(&mut self) -> Option<usize> {
        while let Some(index) = self.pending.pop_front() {
            if self.state.ranges[index].is_complete() {
                continue;
            }
            self.claimed[index] = true;
            return Some(index);
        }
        None
    }

    /// Mark a claimed chunk as finished. Only unsplit chunks feed the median,
    /// because a truncated chunk's duration says nothing about chunk speed.
    pub fn finish(&mut self, index: usize, elapsed: Duration) {
        self.claimed[index] = false;
        if self.state.ranges[index].is_complete() && !self.split[index] {
            self.durations.push(elapsed);
        }
    }

    pub fn chunks(&self) -> usize {
        self.state.ranges.len()
    }

    /// Number of chunks still unclaimed; `0` means idle workers would starve.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Cut the tail off `index` when its ETA is more than 2x the median
    /// completed chunk and no other work is queued. Returns the new chunk.
    ///
    /// The downloaded prefix of `index` is untouched, so no byte is ever
    /// requested twice: the tail starts exactly at the byte after the split.
    pub fn split(&mut self, index: usize, started: Instant) -> Option<usize> {
        if self.split[index] || !self.pending.is_empty() {
            return None;
        }
        if self.durations.len() < MEDIAN_SAMPLES {
            return None;
        }
        let range = self.state.ranges[index];
        let remaining = range.remaining();
        if remaining < MIN_SPLIT_REMAINING {
            return None;
        }
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed <= 0.0 || range.done == 0 {
            return None;
        }
        let eta = remaining as f64 / (range.done as f64 / elapsed);
        let median = median_duration(&self.durations).as_secs_f64();
        if eta <= 2.0 * median {
            return None;
        }

        let min_split = range.start + range.done;
        let candidate = (min_split + remaining / 2) / SPLIT_ALIGN * SPLIT_ALIGN;
        let split_at = candidate.min(range.end.saturating_sub(1));
        if split_at < min_split {
            return None;
        }

        self.state.ranges[index].end = split_at;
        self.state.ranges.push(RangeState {
            start: split_at + 1,
            end: range.end,
            done: 0,
        });
        self.claimed.push(false);
        self.split.push(true);
        self.split[index] = true;
        let new_index = self.state.ranges.len() - 1;
        self.pending.push_back(new_index);
        Some(new_index)
    }
}

fn median_duration(durations: &[Duration]) -> Duration {
    let mut sorted: Vec<Duration> = durations.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CHUNK_BYTES;

    fn state(total: u64) -> ResumeState {
        ResumeState::new("https://example.test/a", None, total, "a", true, 4)
    }

    #[test]
    fn claims_every_incomplete_chunk_once() {
        let mut queue = WorkQueue::new(state(64 * 1024 * 1024));
        assert_eq!(queue.chunks(), 32); // 2 MiB chunks at four workers
        let mut claimed = Vec::new();
        while let Some(index) = queue.claim() {
            claimed.push(index);
        }
        claimed.sort_unstable();
        assert_eq!(claimed, (0..32).collect::<Vec<_>>());
        assert_eq!(queue.pending(), 0);
        assert!(queue.claim().is_none());
    }

    #[test]
    fn completed_chunks_are_not_claimed_again() {
        let mut initial = state(64 * 1024 * 1024);
        for range in &mut initial.ranges[..4] {
            range.done = range.len();
        }
        let mut queue = WorkQueue::new(initial);
        assert_eq!(queue.pending(), 28);
        let first = queue.claim().unwrap();
        assert_eq!(first, 4);
    }

    fn complete_chunk(queue: &mut WorkQueue, index: usize, millis: u64) {
        let len = queue.state().ranges[index].len();
        queue.state_mut().ranges[index].done = len;
        queue.finish(index, Duration::from_millis(millis));
    }

    /// Mirror failover: claims and split bookkeeping from the previous source
    /// must be dropped and all remaining ranges re-queued, including the tail
    /// a split appended.
    #[test]
    fn rebuild_requeues_every_incomplete_range_after_split() {
        let mut queue = WorkQueue::new(state(512 * 1024 * 1024));
        let index = queue.claim().unwrap();
        // Drain the queue so the split gate opens.
        while queue.claim().is_some() {}
        complete_chunk(&mut queue, index + 1, 100);
        complete_chunk(&mut queue, index + 2, 100);
        queue.state.ranges[index].done = 2 * 1024 * 1024;
        let new_index = queue
            .split(index, Instant::now() - Duration::from_secs(5))
            .expect("slow chunk with an empty queue must split");
        assert_eq!(queue.pending(), 1, "the tail is queued for idle workers");
        assert!(queue.state.ranges[new_index].start > queue.state.ranges[index].start);

        queue.rebuild();
        assert_eq!(queue.pending(), queue.chunks() - 2, "two chunks finished");
        assert_eq!(
            queue.chunks(),
            33,
            "split adds a range, so the rebuilt queue has 33 chunks"
        );
        assert!(queue.claim().is_some());
    }

    #[test]
    fn split_keeps_coverage_without_overlap() {
        let mut queue = WorkQueue::new(state(512 * 1024 * 1024));
        let index = queue.claim().unwrap();
        while queue.claim().is_some() {}
        complete_chunk(&mut queue, index + 1, 100);
        complete_chunk(&mut queue, index + 2, 100);
        let done = 3 * 1024 * 1024;
        queue.state.ranges[index].done = done;
        let before = queue.state.ranges.len();
        let new_index = queue
            .split(index, Instant::now() - Duration::from_secs(10))
            .unwrap();

        assert_eq!(queue.state.ranges.len(), before + 1);
        assert_eq!(
            queue.state.ranges[index].end + 1,
            queue.state.ranges[new_index].start
        );
        assert_eq!(queue.state.ranges[new_index].done, 0);
        assert_eq!(
            queue.state.ranges[index].end,
            queue.state.ranges[new_index].start - 1
        );
        // The old downloaded prefix stays where it was.
        assert_eq!(queue.state.ranges[index].done, done);
        assert!(queue.state.invariant_error().is_none());
        assert_eq!(
            queue.state.ranges.iter().map(RangeState::len).sum::<u64>(),
            512 * 1024 * 1024
        );
    }

    #[test]
    fn split_requires_idle_workers_and_slow_chunk() {
        let mut queue = WorkQueue::new(state(512 * 1024 * 1024));
        let index = queue.claim().unwrap();
        queue.state.ranges[index].done = 2 * 1024 * 1024;
        let started = Instant::now() - Duration::from_secs(10);

        // Other pending chunks: stealing is enough, no split.
        assert!(queue.split(index, started).is_none());

        while queue.claim().is_some() {}
        // No completed samples yet: the median is not meaningful.
        assert!(queue.split(index, started).is_none());

        // Only completed chunks feed the median.
        for offset in [1, 2] {
            let i = index + offset;
            let len = queue.state.ranges[i].len();
            queue.state_mut().ranges[i].done = len;
            queue.finish(i, Duration::from_millis(100));
        }
        // The chunk is slow (2 MiB in 10 s) and the queue is empty.
        assert!(queue.split(index, started).is_some());
        // A chunk is cut at most once.
        assert!(queue.split(index, started).is_none());
    }

    #[test]
    fn fast_chunks_are_not_split() {
        let mut queue = WorkQueue::new(state(512 * 1024 * 1024));
        let index = queue.claim().unwrap();
        while queue.claim().is_some() {}
        queue.state.ranges[index].done = CHUNK_BYTES;
        let started = Instant::now() - Duration::from_millis(120);
        for offset in [1, 2] {
            let i = index + offset;
            let len = queue.state.ranges[i].len();
            queue.state_mut().ranges[i].done = len;
            queue.finish(i, Duration::from_millis(100));
        }
        // ETA is already zero (the chunk completed), nothing to split.
        assert!(queue.split(index, started).is_none());
    }

    #[test]
    fn median_handles_even_and_odd_samples() {
        let odd = [
            Duration::from_millis(100),
            Duration::from_millis(300),
            Duration::from_millis(200),
        ];
        assert_eq!(median_duration(&odd), Duration::from_millis(200));
        let even = [Duration::from_millis(100), Duration::from_millis(300)];
        assert_eq!(median_duration(&even), Duration::from_millis(200));
    }
}
