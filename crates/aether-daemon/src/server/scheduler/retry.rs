use crate::server::*;

/// Fail tasks whose plugin process died. Paused tasks are left paused: their
/// resume path restarts the plugin from the stored record.
pub(crate) fn recover_dead_plugins(state: &mut State) {
    let dead = state.manager.prune_dead();
    if dead.is_empty() {
        return;
    }
    for plugin_id in dead {
        let affected: Vec<aether_host::StoredTask> = state
            .live
            .iter()
            .filter_map(|task_id| state.store.task(task_id).ok().flatten())
            .filter(|task| task.plugin_id == plugin_id)
            .collect();
        let mut failed_any = false;
        for task in affected {
            state.active.remove(&task.id);
            state.live.remove(&task.id);
            state.canceling.remove(&task.id);
            state.last_progress.remove(&task.id);
            if task.state == TaskState::Paused {
                let _ = state.store.append_log(
                    &task.id,
                    "warn",
                    &format!("plugin {plugin_id} exited while paused; resume will restart it"),
                    None,
                );
                continue;
            }
            let message = format!("plugin {plugin_id} exited while the task was running");
            let _ = state.store.fail_task(&task.id, "plugin", &message, true);
            let _ = state.store.append_log(&task.id, "error", &message, None);
            broadcast(
                state,
                json!({"event": "failed", "task_id": task.id, "error": message}),
            );
            schedule_retry(state, &task.id, &message);
            failed_any = true;
        }
        if failed_any {
            apply_speed_budgets(state);
            promote_queued(state);
        }
    }
}

/// A plugin that is alive but never reports progress again must not pin a
/// concurrency slot forever. Five minutes is deliberately generous: the HTTP
/// client has a 30 s read timeout, so a live task normally reports far sooner.
pub(crate) fn check_stalled_tasks(state: &mut State) {
    const STALL: Duration = Duration::from_secs(900);
    let stalled: Vec<String> = state
        .active
        .iter()
        .filter(|task_id| {
            state
                .last_progress
                .get(*task_id)
                .is_none_or(|last| last.elapsed() > STALL)
        })
        .cloned()
        .collect();
    let mut failed_any = false;
    for task_id in stalled {
        let Ok(Some(task)) = state.store.task(&task_id) else {
            continue;
        };
        if task.state != TaskState::Running {
            continue;
        }
        // A task that already downloaded its full size may be checksumming a
        // very large file; that is not a stalled download.
        if task.total.is_some_and(|total| task.downloaded >= total) {
            continue;
        }
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::Cancel {
                task_id: task_id.clone(),
            },
        );
        state.active.remove(&task_id);
        state.live.remove(&task_id);
        state.canceling.remove(&task_id);
        state.last_progress.remove(&task_id);
        apply_speed_budgets(state);
        let message = format!("no progress for {} s", STALL.as_secs());
        let _ = state.store.fail_task(&task_id, "timeout", &message, true);
        let _ = state.store.append_log(&task_id, "error", &message, None);
        broadcast(
            state,
            json!({"event": "failed", "task_id": task_id, "error": message}),
        );
        schedule_retry(state, &task_id, &message);
        failed_any = true;
    }
    if failed_any {
        promote_queued(state);
    }
}

/// Schedule an automatic retry for a retryable failure, honoring the attempt
/// cap. Exhausted tasks stay failed until the user resumes them manually.
pub(crate) fn schedule_retry(state: &mut State, task_id: &str, message: &str) {
    if state.retry_max == 0 {
        return;
    }
    // The attempt counter lives in the database so it survives restarts and so
    // a retry cycle cannot reset itself to "first attempt" every time.
    let previous = state
        .store
        .task(task_id)
        .ok()
        .flatten()
        .map(|task| task.retry_attempts)
        .unwrap_or(0);
    let attempts = previous.saturating_add(1);
    if attempts > state.retry_max {
        state.retries.remove(task_id);
        // Keep the counter: "failed after N retries" is information worth
        // showing; `retry_at = NULL` already means "nothing scheduled".
        let _ = state.store.clear_pending_retry(task_id);
        let _ = state.store.append_log(
            task_id,
            "warn",
            &format!("auto-retry exhausted after {previous} retries: {message}"),
            None,
        );
        return;
    }
    let delay = Duration::from_secs(
        state
            .retry_base_secs
            .saturating_mul(1_u64 << (attempts - 1).min(6)),
    );
    let next = Instant::now() + delay;
    let _ = state
        .store
        .set_retry(task_id, attempts, now_ms() + delay.as_millis() as i64);
    state
        .retries
        .insert(task_id.to_string(), RetrySchedule { next });
    let _ = state.store.append_log(
        task_id,
        "info",
        &format!(
            "auto-retry {attempts}/{} in {} s",
            state.retry_max,
            delay.as_secs()
        ),
        None,
    );
    broadcast(
        state,
        json!({
            "event": "retry_scheduled",
            "task_id": task_id,
            "attempt": attempts,
            "delay_secs": delay.as_secs(),
        }),
    );
}

/// Start every retry whose backoff elapsed; retries re-enter the normal queue
/// so they compete FIFO with other work.
pub(crate) fn check_retries(state: &mut State) {
    if state.retries.is_empty() {
        return;
    }
    let now = Instant::now();
    let due: Vec<String> = state
        .retries
        .iter()
        .filter(|(_, schedule)| schedule.next <= now)
        .map(|(task_id, _)| task_id.clone())
        .collect();
    if due.is_empty() {
        return;
    }
    for task_id in due {
        state.retries.remove(&task_id);
        let _ = state.store.clear_pending_retry(&task_id);
        let result = state.store.set_state(&task_id, TaskState::Queued);
        if result.is_ok() {
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
            );
        }
    }
    promote_queued(state);
}

/// (Re)start a task from its stored record if capacity allows. Ok(true) when
/// the task is running afterwards, Ok(false) when it stayed queued.
pub(crate) fn start_from_record(state: &mut State, task: &StoredTask) -> Result<bool, DaemonError> {
    let final_dir = destination_dir(state, task.target_dir.as_deref());
    fs::create_dir_all(&final_dir)?;
    let staging = prepare_staging(state, &task.id, &final_dir)?;
    let checksum = parse_checksum(&task.settings).ok().flatten();
    let spec = TaskSpec {
        task_id: task.id.clone(),
        url: task.url.clone(),
        filename: task.filename.clone(),
        settings: task.settings.clone(),
        plugin_state: task.plugin_state.clone(),
        expected_size: task.total,
        checksum,
        proxy: None,
    };
    // PAC is resolved again for every (re)start: the script may have changed
    // between attempts. No network I/O here — promotions must not block.
    let spec = TaskSpec {
        proxy: resolve_task_proxy(state, &spec.url, false),
        ..spec
    };
    try_launch(state, &task.plugin_id, spec, staging, final_dir)
}
