use crate::server::*;

pub(crate) fn resolve_proxy_request(
    state: &mut State,
    request: Request,
) -> Result<Value, DaemonError> {
    let url = request
        .url
        .ok_or_else(|| DaemonError::Protocol("resolve_proxy needs url".into()))?;
    let pac = state.pac.is_some();
    let proxy = resolve_task_proxy(state, &url, true);
    let error = state.pac.as_ref().and_then(PacResolver::last_error);
    Ok(json!({ "url": url, "pac": pac, "proxy": proxy, "pac_error": error }))
}

/// Change one task's download limit. Queued tasks pick it up on start;
/// running HTTP tasks receive a live `SetBudget`. BitTorrent's session-wide
/// rate limiter has no per-task switch, so BT is rejected instead of silently
/// ignoring the call.
pub(crate) fn set_task_limit(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    let limit = request
        .settings
        .get("max_speed_bps")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let task = state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    let torrent = state.manager.registry().iter().any(|entry| {
        entry.manifest.id == task.plugin_id
            && entry
                .manifest
                .features
                .iter()
                .any(|feature| feature == "torrent")
    });
    if torrent {
        return Err(DaemonError::Protocol(
            "per-task speed limits are not supported for BitTorrent".into(),
        ));
    }
    let mut settings = task.settings.as_object().cloned().unwrap_or_default();
    if limit == 0 {
        settings.remove("max_speed_bps");
    } else {
        settings.insert("max_speed_bps".into(), json!(limit));
    }
    let settings = Value::Object(settings);
    state.store.update_task_settings(&task_id, &settings)?;
    // Recompute every running task: the new limit applies immediately, and
    // `0` falls back to the task's fair share of the global limit instead of
    // running unlimited.
    apply_speed_budgets(state);
    Ok(json!({ "task_id": task_id, "settings": settings }))
}

/// Export the queued/running/paused URLs in aria2's session-line format.
/// SQLite remains the restore source; this is a portable snapshot for users
/// and clients that call `aria2.saveSession`.
pub(crate) fn save_session(state: &State) -> Result<Value, DaemonError> {
    let tasks = state.store.list_tasks_with_files(10_000, false)?;
    let active = tasks
        .into_iter()
        .filter(|task| {
            matches!(
                task.state,
                TaskState::Queued | TaskState::Running | TaskState::Paused
            )
        })
        .collect::<Vec<_>>();
    let mut text = String::new();
    for task in &active {
        text.push_str(&task.url);
        text.push('\n');
    }
    let path = state.data_dir.join("aria2-session.txt");
    let temporary = path.with_extension("txt.tmp");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, &path)?;
    Ok(json!({ "path": path, "count": active.len() }))
}

/// Live peer list for the aria2 compatibility layer. The BT plugin pushes
/// peer snapshots through `Detail`; reading the cache keeps this off the
/// daemon's main loop (no plugin round-trip).
pub(crate) fn get_peers(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let task_id = task_id(&request)?;
    state
        .store
        .task(&task_id)?
        .ok_or_else(|| DaemonError::Protocol(format!("task {task_id} not found")))?;
    let peers = state
        .details
        .get(&task_id)
        .and_then(|detail| detail.get("peers"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    Ok(json!({ "peers": peers }))
}

/// Apply an explicit waiting-queue order. Priorities are spaced by 200 and
/// pre-compensated for the aging bonus (capped at 100), so the requested
/// order survives future aging until the queue drains.
pub(crate) fn reorder_queue(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let order = request
        .settings
        .get("order")
        .and_then(Value::as_array)
        .ok_or_else(|| DaemonError::Protocol("reorder_queue needs settings.order".into()))?;
    let now = now_ms();
    let mut changes = Vec::with_capacity(order.len());
    for (index, id) in order.iter().enumerate() {
        let id = id
            .as_str()
            .ok_or_else(|| DaemonError::Protocol("reorder_queue entries must be strings".into()))?;
        let task = state
            .store
            .task(id)?
            .ok_or_else(|| DaemonError::Protocol(format!("task {id} not found")))?;
        if !matches!(task.state, TaskState::Queued | TaskState::Paused) {
            return Err(DaemonError::Protocol(format!("task {id} is not waiting")));
        }
        let age_minutes = (now - task.created_at).max(0) / 1000 / 60;
        let age = (age_minutes / 5).min(100);
        let priority = 1_000_000 - (index as i64 * 200) - age;
        changes.push((id.to_string(), priority as i32));
    }
    state.store.set_priorities(&changes)?;
    promote_queued(state);
    Ok(json!({ "reordered": changes.len() }))
}
