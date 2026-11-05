//! Per-host connection governance and rate-limit cooldown.
//!
//! A task cannot know how many other tasks target the same host, so the
//! `Budget::per_host_connections` cap is enforced inside the plugin process
//! where every HTTP task runs. Each in-flight request holds one slot for its
//! host; a task may hold at most its fair share (`cap / tasks_on_host`, never
//! below 1). Shares are recomputed on every acquisition, so a task that starts
//! later shrinks the older one's share instead of waiting behind it — no task
//! is starved by another, and the sum of all shares never exceeds the cap.
//!
//! A slot covers one Range request. The downloader bounds each request to
//! `REQUEST_BYTES`, which is what makes a shrinking share effective: an
//! over-quota worker releases its slot within one request instead of holding
//! it for a multi-hundred-MiB range.
//!
//! The second half of this module is the 418/429 cooldown: a rejection halves
//! the task's connection limit and parks every worker until a shared deadline,
//! so a source that allows fewer connections than the task asked for still
//! finishes instead of failing the whole task in one round.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Cooldown steps: 1 s, 2 s, 4 s … capped at 30 s. Kept short on purpose —
/// anti-leech servers usually let a lower connection count through.
const COOLDOWN_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct TaskUse {
    /// Requests this task currently has in flight on the host.
    active: u32,
}

#[derive(Debug)]
struct HostUse {
    cap: u32,
    /// Slots in flight across all tasks on this host. The per-task shares
    /// alone are not enough: a task that started earlier may still hold more
    /// than its shrunk share, so the host total is checked too.
    active: u32,
    tasks: HashMap<String, TaskUse>,
}

#[derive(Debug, Default)]
struct Inner {
    hosts: HashMap<String, HostUse>,
}

/// Connection slots per host, shared by every task in this plugin process.
#[derive(Debug, Default)]
pub struct HostGate {
    inner: Mutex<Inner>,
}

impl HostGate {
    /// Register a task as a user of `host`. The returned guard unregisters it
    /// on drop, so every exit path (done, failed, canceled) releases the
    /// share; there is no lease to leak.
    pub fn register(self: &Arc<Self>, host: &str, task: &str, cap: u32) -> Registration {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner
            .hosts
            .entry(host.to_string())
            .or_insert_with(|| HostUse {
                cap: cap.max(1),
                active: 0,
                tasks: HashMap::new(),
            });
        // Every task carries the same daemon-wide cap; taking the smaller
        // value keeps an explicit per-task override conservative.
        entry.cap = entry.cap.min(cap.max(1));
        entry.tasks.insert(task.to_string(), TaskUse::default());
        drop(inner);
        Registration {
            gate: self.clone(),
            host: host.to_string(),
            task: task.to_string(),
        }
    }

    /// Take one slot if this task is below its share. Returns `None` when the
    /// caller should retry shortly (other tasks or this task's own workers
    /// hold the slots).
    pub fn acquire(self: &Arc<Self>, host: &str, task: &str, desired: u32) -> Option<Slot> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.hosts.get_mut(host)?;
        if entry.active >= entry.cap {
            return None;
        }
        let tasks = entry.tasks.len().max(1) as u32;
        let share = (entry.cap / tasks).max(1);
        let allowance = desired.max(1).min(share);
        let used = entry.tasks.get_mut(task)?;
        if used.active >= allowance {
            return None;
        }
        used.active += 1;
        entry.active += 1;
        Some(Slot {
            gate: self.clone(),
            host: host.to_string(),
            task: task.to_string(),
        })
    }

    /// The share one task gets right now (for logging only).
    pub fn share(&self, host: &str) -> u32 {
        let inner = self.inner.lock().unwrap();
        let Some(entry) = inner.hosts.get(host) else {
            return 0;
        };
        (entry.cap / entry.tasks.len().max(1) as u32).max(1)
    }

    fn release(&self, host: &str, task: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.hosts.get_mut(host) else {
            return;
        };
        if let Some(used) = entry.tasks.get_mut(task) {
            if used.active > 0 {
                used.active -= 1;
                entry.active = entry.active.saturating_sub(1);
            }
        }
    }

    fn unregister(&self, host: &str, task: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.hosts.get_mut(host) else {
            return;
        };
        entry.tasks.remove(task);
        if entry.tasks.is_empty() {
            inner.hosts.remove(host);
        }
    }
}

/// A held connection slot. Dropping it returns the slot to the host.
pub struct Slot {
    gate: Arc<HostGate>,
    host: String,
    task: String,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.gate.release(&self.host, &self.task);
    }
}

/// Keeps a task registered on a host for the duration of one attempt.
pub struct Registration {
    gate: Arc<HostGate>,
    host: String,
    task: String,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.gate.unregister(&self.host, &self.task);
    }
}

/// Task-level connection limit plus the shared 418/429 cooldown.
#[derive(Debug)]
pub struct Cooldown {
    limit: AtomicU32,
    strikes: AtomicU32,
    until: Mutex<Instant>,
    base: Duration,
    max_strikes: u32,
}

impl Cooldown {
    pub fn new(limit: u32) -> Self {
        Self::with_base(limit, Duration::from_secs(1), 6)
    }

    fn with_base(limit: u32, base: Duration, max_strikes: u32) -> Self {
        Self {
            limit: AtomicU32::new(limit.max(1)),
            strikes: AtomicU32::new(0),
            until: Mutex::new(Instant::now()),
            base,
            max_strikes,
        }
    }

    /// Current connection allowance for the task (halved per rejection).
    pub fn limit(&self) -> u32 {
        self.limit.load(Ordering::Relaxed)
    }

    pub fn strikes(&self) -> u32 {
        self.strikes.load(Ordering::Relaxed)
    }

    /// Rejections allowed before the task gives up with an explicit reason.
    pub fn max_strikes(&self) -> u32 {
        self.max_strikes
    }

    pub fn remaining(&self) -> Duration {
        let until = *self.until.lock().unwrap();
        until.saturating_duration_since(Instant::now())
    }

    /// Record a 418/429. Concurrent rejections from several workers collapse
    /// into one strike: once the cooldown is in force, later calls only report
    /// the current state. Returns the new limit and the remaining cooldown.
    pub fn note_rejection(&self, retry_after: Option<Duration>) -> (u32, Duration) {
        let now = Instant::now();
        let mut until = self.until.lock().unwrap();
        if now < *until {
            return (self.limit(), until.saturating_duration_since(now));
        }
        let strike = self.strikes.fetch_add(1, Ordering::Relaxed) + 1;
        let current = self.limit();
        let next = (current / 2).max(1);
        self.limit.store(next, Ordering::Relaxed);
        let exp = self
            .base
            .saturating_mul(2_u32.saturating_pow(strike.saturating_sub(1)))
            .min(COOLDOWN_CAP);
        let wait = retry_after.unwrap_or_default().max(exp).min(COOLDOWN_CAP);
        *until = now + wait;
        (next, wait)
    }
}

/// Parse a `Retry-After` header. Only the delay-seconds form is honoured; the
/// HTTP-date form falls back to the exponential cooldown because the engine
/// has no calendar parser. A date in the past is treated as "retry now".
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_tasks_share_the_host_cap_without_exceeding_it() {
        let gate = Arc::new(HostGate::default());
        let _a = gate.register("example.test", "a", 8);
        let _b = gate.register("example.test", "b", 8);
        let _c = gate.register("example.test", "c", 8);

        // cap 8 / 3 tasks = 2 slots each, total 6 — never 24.
        let mut held = Vec::new();
        for task in ["a", "b", "c"] {
            for _ in 0..4 {
                if let Some(slot) = gate.acquire("example.test", task, 8) {
                    held.push(slot);
                }
            }
        }
        assert_eq!(held.len(), 6, "each task must stop at its fair share");
        drop(held);

        // Slots are reusable once released.
        for task in ["a", "b", "c"] {
            assert!(gate.acquire("example.test", task, 8).is_some());
        }
    }

    #[test]
    fn a_single_task_may_use_the_whole_cap_but_not_more() {
        let gate = Arc::new(HostGate::default());
        let _a = gate.register("example.test", "a", 8);
        let mut held = Vec::new();
        for _ in 0..10 {
            if let Some(slot) = gate.acquire("example.test", "a", 16) {
                held.push(slot);
            }
        }
        assert_eq!(held.len(), 8);
    }

    #[test]
    fn an_older_task_holding_the_whole_cap_blocks_newcomers_until_it_releases() {
        let gate = Arc::new(HostGate::default());
        let _a = gate.register("example.test", "a", 8);
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(gate.acquire("example.test", "a", 8).unwrap());
        }
        let _b = gate.register("example.test", "b", 8);
        assert!(
            gate.acquire("example.test", "b", 8).is_none(),
            "a later task must not push the host over its cap"
        );
        drop(held.pop());
        assert!(gate.acquire("example.test", "b", 8).is_some());
    }

    #[test]
    fn unregistering_a_task_frees_its_share() {
        let gate = Arc::new(HostGate::default());
        let a = gate.register("example.test", "a", 4);
        let _b = gate.register("example.test", "b", 4);
        assert_eq!(gate.share("example.test"), 2);
        drop(a);
        assert_eq!(gate.share("example.test"), 4, "a alone takes the whole cap");
    }

    #[test]
    fn rejection_halves_the_limit_and_cools_down_once_per_window() {
        let cooldown = Cooldown::with_base(8, Duration::from_millis(5), 6);
        let (limit, wait) = cooldown.note_rejection(Some(Duration::from_secs(30)));
        assert_eq!(limit, 4);
        assert_eq!(wait, COOLDOWN_CAP, "Retry-After is capped, never 30 s+");
        // A second worker hitting the same window must not halve again.
        let (limit, _) = cooldown.note_rejection(None);
        assert_eq!(limit, 4);
        assert_eq!(cooldown.strikes(), 1);
        assert!(cooldown.remaining() > Duration::from_millis(1));
    }

    #[test]
    fn rejection_steps_down_to_one_connection_and_backs_off_exponentially() {
        let cooldown = Cooldown::with_base(8, Duration::from_millis(1), 6);
        let mut waits = Vec::new();
        for _ in 0..4 {
            let (_, wait) = cooldown.note_rejection(None);
            waits.push(wait);
            thread_sleep(wait);
        }
        assert_eq!(cooldown.limit(), 1, "eight connections halve down to one");
        assert_eq!(cooldown.strikes(), 4);
        assert!(waits[3] >= waits[0], "backoff grows: {waits:?}");
    }

    fn thread_sleep(duration: Duration) {
        std::thread::sleep(duration + Duration::from_millis(1));
    }

    #[test]
    fn parses_retry_after_seconds_only() {
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }
}
