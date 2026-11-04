use crate::server::*;

/// Reload automatic retries that were pending when the daemon last stopped.
/// Due entries go straight back to the queue; future ones keep their remaining
/// delay so a restart does not restart the backoff from scratch.
pub(crate) fn restore_retries(state: &mut State) {
    let pending = match state.store.pending_retries() {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("aether-daemon: cannot load pending retries: {error}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    let now = now_ms();
    for (task_id, attempts, at) in pending {
        let remaining = at.saturating_sub(now);
        if remaining <= 0 {
            let _ = state.store.clear_pending_retry(&task_id);
            if state.store.set_state(&task_id, TaskState::Queued).is_ok() {
                let _ = state.store.append_log(
                    &task_id,
                    "info",
                    "auto-retry resumed after restart",
                    None,
                );
            }
        } else {
            state.retries.insert(
                task_id.clone(),
                RetrySchedule {
                    next: Instant::now() + Duration::from_millis(remaining as u64),
                },
            );
            let _ = state.store.append_log(
                &task_id,
                "info",
                &format!(
                    "auto-retry {attempts}/{} resumes in {} s after restart",
                    state.retry_max,
                    remaining / 1000
                ),
                None,
            );
        }
    }
    promote_queued(state);
}
