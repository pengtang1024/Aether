use crate::server::*;

pub(crate) fn list_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let limit = request
        .settings
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 10_000) as usize;
    let include_files = request
        .settings
        .get("include_files")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let tasks = state.store.list_tasks_with_files(limit, include_files)?;
    // The UI needs the default root to show a path for tasks that use
    // it (records only carry non-default destinations).
    Ok(json!({
        "tasks": tasks,
        "download_dir": state.download_dir.to_string_lossy(),
    }))
}

pub(crate) fn get_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    Ok(json!({ "task": task }))
}

pub(crate) fn logs_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    // 404-style error keeps the UI honest when a task was deleted.
    state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    let limit = request
        .settings
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 1000) as usize;
    Ok(json!({ "task_id": task_id, "logs": state.store.logs(&task_id, limit)? }))
}

pub(crate) fn pause_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    match task.state {
        TaskState::Running => {
            let _ = state.manager.send(
                &task.plugin_id,
                &HostCommand::Pause {
                    task_id: task_id.clone(),
                },
            );
            state.store.set_state(&task_id, TaskState::Paused)?;
            state.active.remove(&task_id);
            apply_speed_budgets(state);
            let _ = state
                .store
                .append_log(&task_id, "info", "paused by user", None);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
            );
            Ok(json!({"task_id": task_id, "state": "paused"}))
        }
        // A queued task is not executing anything; holding it just
        // keeps it out of the promotion path.
        TaskState::Queued => {
            state.store.set_state(&task_id, TaskState::Paused)?;
            let _ = state
                .store
                .append_log(&task_id, "info", "paused by user", None);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
            );
            Ok(json!({"task_id": task_id, "state": "paused"}))
        }
        other => Err(DaemonError::Protocol(format!(
            "cannot pause a {} task",
            other.as_str()
        ))),
    }
}

pub(crate) fn resume_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    match task.state {
        TaskState::Running => Ok(json!({"task_id": task_id, "state": "running"})),
        TaskState::Completed => Err(DaemonError::Protocol("task already completed".into())),
        _ if state.canceling.contains(&task_id) => Err(DaemonError::Protocol(
            "cancel is still in progress; retry shortly".into(),
        )),
        _ => {
            // A manual resume is an explicit "try again": reset the
            // automatic retry bookkeeping (in memory and in the store).
            state.retries.remove(&task_id);
            let _ = state.store.reset_retry(&task_id);
            if state.live.contains(&task_id) {
                // The plugin still owns a live (paused or stopping)
                // execution; resume it in place.
                let _ = state.manager.send(
                    &task.plugin_id,
                    &HostCommand::Resume {
                        task_id: task_id.clone(),
                    },
                );
                state.store.set_state(&task_id, TaskState::Running)?;
                state.active.insert(task_id.clone());
                apply_speed_budgets(state);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "resumed by user", None);
                broadcast(
                    state,
                    json!({"event": "task_state", "task_id": task_id, "state": "running"}),
                );
                return Ok(json!({"task_id": task_id, "state": "running"}));
            }
            match start_from_record(state, &task) {
                Ok(started) => {
                    if !started {
                        let _ = state.store.set_state(&task_id, TaskState::Queued);
                        let _ = state
                            .store
                            .append_log(&task_id, "info", "resumed by user", None);
                        broadcast(
                            state,
                            json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
                        );
                    }
                    Ok(json!({
                        "task_id": task_id,
                        "state": if started { "running" } else { "queued" },
                    }))
                }
                Err(error) => Err(error),
            }
        }
    }
}

pub(crate) fn task_summary_request(
    state: &mut State,
    _request: Request,
) -> Result<Value, DaemonError> {
    let summary = state.store.summary()?;
    let mut counts = serde_json::Map::new();
    let mut download_speed_bps = 0_u64;
    for (task_state, (count, speed)) in summary {
        if task_state == "running" {
            download_speed_bps = download_speed_bps.saturating_add(speed);
        }
        counts.insert(task_state, json!(count));
    }
    Ok(json!({
        "counts": counts,
        "download_speed_bps": download_speed_bps,
    }))
}

pub(crate) fn cancel_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    if task.state == TaskState::Completed {
        return Err(DaemonError::Protocol(format!(
            "task {task_id} is already completed"
        )));
    }
    // Canceled is idempotent; failed tasks can be canceled to stop the
    // automatic retry cycle.
    if state.canceling.contains(&task_id) {
        return Ok(json!({"task_id": task_id, "state": "canceled"}));
    }
    if state.live.contains(&task_id) {
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::Cancel {
                task_id: task_id.clone(),
            },
        );
    }
    state.canceling.insert(task_id.clone());
    state.store.set_state(&task_id, TaskState::Canceled)?;
    let _ = state.store.reset_retry(&task_id);
    let _ = state
        .store
        .append_log(&task_id, "info", "canceled by user", None);
    state.active.remove(&task_id);
    state.live.remove(&task_id);
    state.retries.remove(&task_id);
    state.details.remove(&task_id);
    state.last_progress.remove(&task_id);
    apply_speed_budgets(state);
    broadcast(
        state,
        json!({"event": "task_state", "task_id": task_id, "state": "canceled"}),
    );
    promote_queued(state);
    Ok(json!({"task_id": task_id, "state": "canceled"}))
}

pub(crate) fn delete_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    if !task.state.is_terminal() {
        return Err(DaemonError::Protocol(
            "cancel the task before deleting it".into(),
        ));
    }
    // Staging/resume state is daemon-owned; finalized files live in the
    // download directory and survive the deletion.
    let centralized = state.download_dir.join(".aether").join(&task_id);
    let _ = fs::remove_dir_all(&centralized);
    cleanup_staging_parent(&centralized);
    let final_dir = destination_dir(state, task.target_dir.as_deref());
    let target_staging = final_dir.join(".aether-staging").join(&task_id);
    let _ = fs::remove_dir_all(&target_staging);
    cleanup_staging_parent(&target_staging);
    state.details.remove(&task_id);
    state.last_progress.remove(&task_id);
    state.store.delete_task(&task_id)?;
    broadcast(state, json!({"event": "task_deleted", "task_id": task_id}));
    Ok(json!({"task_id": task_id, "deleted": true}))
}
