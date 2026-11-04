use crate::server::*;

pub(crate) fn schedule_in_window(state: &State) -> bool {
    use chrono::Timelike;

    let now = chrono::Local::now();
    let now_minutes = now.hour() * 60 + now.minute();
    schedule_active(
        state.settings.schedule_on,
        state.settings.schedule_start.as_deref(),
        state.settings.schedule_end.as_deref(),
        now_minutes,
    )
}

/// Enforce the scheduled window: everything pauses when the window closes
/// (and stays out of the launcher while it is closed); the tasks the
/// scheduler paused resume when it opens again. User-paused tasks are never
/// touched because only `schedule_paused` ids are resumed.
pub(crate) fn tick_schedule(state: &mut State) {
    if schedule_in_window(state) {
        let paused: Vec<String> = state.schedule_paused.drain().collect();
        for task_id in paused {
            resume_scheduled(state, &task_id);
        }
        return;
    }
    // Snapshot first: pausing mutates `active` and the store.
    let mut candidates = state.active.clone();
    if let Ok(tasks) = state.store.list_tasks(500) {
        candidates.extend(
            tasks
                .into_iter()
                .filter(|task| task.state == TaskState::Queued)
                .map(|task| task.id),
        );
    }
    for task_id in candidates {
        if !state.schedule_paused.insert(task_id.clone()) {
            continue;
        }
        let Ok(Some(task)) = state.store.task(&task_id) else {
            state.schedule_paused.remove(&task_id);
            continue;
        };
        match task.state {
            TaskState::Running => {
                let _ = state.manager.send(
                    &task.plugin_id,
                    &HostCommand::Pause {
                        task_id: task_id.clone(),
                    },
                );
                let _ = state.store.set_state(&task_id, TaskState::Paused);
                state.active.remove(&task_id);
                apply_speed_budgets(state);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "paused by schedule", None);
                broadcast(
                    state,
                    json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                );
            }
            TaskState::Queued => {
                let _ = state.store.set_state(&task_id, TaskState::Paused);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "paused by schedule", None);
                broadcast(
                    state,
                    json!({"event": "task_state", "task_id": task_id, "state": "paused"}),
                );
            }
            // Anything else raced us (finished / failed / user pause).
            _ => {
                state.schedule_paused.remove(&task_id);
            }
        }
    }
}

/// Resume one task the schedule paused: live plugin executions resume in
/// place, cold ones go through the launcher (which the open window allows).
fn resume_scheduled(state: &mut State, task_id: &str) {
    let Ok(Some(task)) = state.store.task(task_id) else {
        return;
    };
    if task.state != TaskState::Paused {
        return;
    }
    if state.live.contains(task_id) {
        let _ = state.manager.send(
            &task.plugin_id,
            &HostCommand::Resume {
                task_id: task_id.to_string(),
            },
        );
        let _ = state.store.set_state(task_id, TaskState::Running);
        state.active.insert(task_id.to_string());
        state
            .last_progress
            .insert(task_id.to_string(), Instant::now());
        apply_speed_budgets(state);
        let _ = state
            .store
            .append_log(task_id, "info", "resumed by schedule", None);
        broadcast(
            state,
            json!({"event": "task_state", "task_id": task_id, "state": "running"}),
        );
        return;
    }
    match start_from_record(state, &task) {
        Ok(true) => {
            let _ = state
                .store
                .append_log(task_id, "info", "resumed by schedule", None);
        }
        // At capacity: back to the queue, the next promotion picks it up.
        Ok(false) => {
            let _ = state.store.set_state(task_id, TaskState::Queued);
            broadcast(
                state,
                json!({"event": "task_state", "task_id": task_id, "state": "queued"}),
            );
        }
        Err(error) => {
            let _ = state
                .store
                .fail_task(task_id, "internal", &error.to_string(), true);
        }
    }
}
