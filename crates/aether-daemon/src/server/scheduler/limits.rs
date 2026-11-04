use crate::server::*;
use crate::settings::CONNECTIONS_MAX;

/// Connections one task may open. A task-level `connections` setting wins over
/// the daemon-wide default; out-of-range values are rejected by `add_task`,
/// and the http plugin clamps to the same `1..=CONNECTIONS_MAX` range anyway.
pub(crate) fn task_connections(settings: &Value, fallback: u32) -> u32 {
    settings
        .get("connections")
        .and_then(Value::as_u64)
        .map(|value| (value as u32).clamp(1, CONNECTIONS_MAX))
        .unwrap_or(fallback)
}

/// Parse one numeric limit out of a patch. `null` is rejected: the scheduler
/// needs a concrete value, and `0` already means what the caller wants (no
/// limit, no retry).
pub(crate) fn parse_limit<T>(value: &Value, key: &str) -> Result<T, DaemonError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<Option<T>>(value.clone())
        .map_err(|error| DaemonError::Protocol(format!("invalid {key}: {error}")))?
        .ok_or_else(|| DaemonError::Protocol(format!("{key} cannot be cleared; send a number")))
}

/// Copy the persisted limits onto the fields the scheduler reads. `validate`
/// has run by then, and startup resolution keeps the limits present, so every
/// value here is one the engine can honour.
pub(crate) fn apply_limits(state: &mut State) {
    if let Some(value) = state.settings.max_concurrent {
        state.max_concurrent = value as usize;
    }
    // `0` is the stored form of "no limit".
    if let Some(value) = state.settings.max_speed_bps {
        state.max_speed_bps = (value > 0).then_some(value);
    }
    if let Some(value) = state.settings.retry_max {
        state.retry_max = value;
    }
    // A new global limit (or a cleared one) must reach running tasks at once.
    apply_speed_budgets(state);
}

/// Explicit per-task limit (`max_speed_bps`) for the active tasks.
fn active_task_limits(state: &State) -> Vec<(String, Option<u64>)> {
    state
        .active
        .iter()
        .filter_map(|task_id| {
            let task = state.store.task(task_id).ok().flatten()?;
            let limit = task
                .settings
                .get("max_speed_bps")
                .and_then(Value::as_u64)
                .filter(|limit| *limit > 0);
            Some((task_id.clone(), limit))
        })
        .collect()
}

/// Split the global limit across the running tasks. An explicit per-task
/// limit is honoured unless it exceeds its fair share; what is left goes to
/// the tasks without a limit, so the sum stays within `total` (except when
/// the total is smaller than the task count and every task needs at least
/// 1 B/s to avoid being "unlimited").
pub(crate) fn distribute_budget(
    total: Option<u64>,
    tasks: &[(String, Option<u64>)],
) -> HashMap<String, u64> {
    let mut shares = HashMap::with_capacity(tasks.len());
    let Some(total) = total else {
        for (task_id, limit) in tasks {
            shares.insert(task_id.clone(), limit.unwrap_or(0));
        }
        return shares;
    };
    let fair = (total / tasks.len().max(1) as u64).max(1);
    let mut remaining = total;
    for (task_id, limit) in tasks {
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            let share = limit.min(fair);
            shares.insert(task_id.clone(), share);
            remaining = remaining.saturating_sub(share);
        }
    }
    let unlimited = tasks
        .iter()
        .filter(|(_, limit)| limit.is_none_or(|limit| limit == 0))
        .count() as u64;
    // `max(1)` keeps `download_bps == 0` from meaning "unlimited" when the
    // total is already exhausted by explicit limits.
    let share = (remaining / unlimited.max(1)).max(1);
    for (task_id, limit) in tasks {
        if limit.is_none_or(|limit| limit == 0) {
            shares.insert(task_id.clone(), share);
        }
    }
    shares
}

/// Recompute every running task's share and push it to its plugin. Called
/// whenever the active set or the limits change, so a new task, a pause, or
/// `set_task_limit` takes effect without restarting anything.
pub(crate) fn apply_speed_budgets(state: &mut State) {
    if state.active.is_empty() {
        return;
    }
    let fallback = state.settings.connections.unwrap_or(CONNECTIONS_DEFAULT);
    // One store read per active task: the split needs its limit and the
    // dispatch needs its plugin and connection count, so reading twice cost
    // two full row-plus-files queries per task on every budget change.
    let mut tasks = Vec::with_capacity(state.active.len());
    let mut targets = HashMap::with_capacity(state.active.len());
    for task_id in &state.active {
        let Some(task) = state.store.task(task_id).ok().flatten() else {
            continue;
        };
        let limit = task
            .settings
            .get("max_speed_bps")
            .and_then(Value::as_u64)
            .filter(|limit| *limit > 0);
        targets.insert(
            task_id.clone(),
            (
                task.plugin_id.clone(),
                task_connections(&task.settings, fallback),
            ),
        );
        tasks.push((task_id.clone(), limit));
    }
    if tasks.is_empty() {
        return;
    }
    let shares = distribute_budget(state.max_speed_bps, &tasks);
    for (task_id, download_bps) in shares {
        if !state.live.contains(&task_id) {
            continue;
        }
        let Some((plugin_id, connections)) = targets.get(&task_id) else {
            continue;
        };
        let _ = state.manager.send(
            plugin_id,
            &HostCommand::SetBudget {
                task_id,
                budget: Budget {
                    connections: *connections,
                    download_bps,
                    ..Budget::default()
                },
            },
        );
    }
}

/// Start a task unless the concurrency limit is reached. Returns Ok(true) when
/// the task is now running, Ok(false) when it stayed queued.
pub(crate) fn try_launch(
    state: &mut State,
    plugin_id: &str,
    task: TaskSpec,
    staging: PathBuf,
    final_dir: PathBuf,
) -> Result<bool, DaemonError> {
    // Outside the scheduled window nothing starts; the periodic tick pauses
    // whatever is already running.
    if !schedule_in_window(state) {
        return Ok(false);
    }
    if state.active.len() >= state.max_concurrent {
        return Ok(false);
    }

    let task_id = task.task_id.clone();
    let settings_limit = task
        .settings
        .get("max_speed_bps")
        .and_then(serde_json::Value::as_u64)
        .filter(|limit| *limit > 0);
    // Include this task in the split so its starting budget already reflects
    // the global limit; the other running tasks are rebalanced below.
    let mut planned = active_task_limits(state);
    planned.push((task_id.clone(), settings_limit));
    let download_bps = distribute_budget(state.max_speed_bps, &planned)
        .get(&task_id)
        .copied()
        .unwrap_or(0);
    let budget = Budget {
        // A task-level `connections` setting wins over the settings-page
        // default; the http plugin clamps this to 16 when it starts ranges.
        connections: task_connections(
            &task.settings,
            state.settings.connections.unwrap_or(CONNECTIONS_DEFAULT),
        ),
        download_bps,
        ..Budget::default()
    };

    state
        .manager
        .start(plugin_id, task, TaskPaths { staging, final_dir }, budget)
        .map_err(DaemonError::Host)?;
    state.store.set_state(&task_id, TaskState::Running)?;
    let _ = state.store.append_log(
        &task_id,
        "info",
        &format!("running with plugin {plugin_id}"),
        None,
    );
    state.active.insert(task_id.clone());
    state.live.insert(task_id.clone());
    state.last_progress.insert(task_id.clone(), Instant::now());
    // A new task changes every running task's fair share.
    apply_speed_budgets(state);
    broadcast(
        state,
        json!({"event": "task_state", "task_id": task_id, "state": "running"}),
    );
    Ok(true)
}

/// Promote the oldest queued tasks while there is capacity.
pub(crate) fn promote_queued(state: &mut State) {
    while state.active.len() < state.max_concurrent {
        // Highest priority first, then FIFO; the store does the ordering so
        // the daemon never re-implements it.
        let next = state.store.next_queued().ok().flatten();
        let Some(task) = next else { return };
        match start_from_record(state, &task) {
            Ok(true) => {}
            Ok(false) => {
                break;
            }
            Err(error) => {
                let _ = state
                    .store
                    .fail_task(&task.id, "internal", &error.to_string(), true);
                continue;
            }
        }
    }
}

/// Whether the schedule lets tasks run at `now_minutes` (minutes since
/// midnight). A window whose end is before its start wraps midnight. A
/// disabled, incomplete or unparseable schedule always allows running: a
/// hand-edited file must not stall the queue silently.
pub(crate) fn schedule_active(
    on: Option<bool>,
    start: Option<&str>,
    end: Option<&str>,
    now_minutes: u32,
) -> bool {
    if on != Some(true) {
        return true;
    }
    let parse = |text: Option<&str>| {
        text.map(str::trim)
            .filter(|text| !text.is_empty())
            .and_then(parse_hhmm)
    };
    let (Some(start), Some(end)) = (parse(start), parse(end)) else {
        return true;
    };
    if start <= end {
        (start..end).contains(&now_minutes)
    } else {
        now_minutes >= start || now_minutes < end
    }
}
