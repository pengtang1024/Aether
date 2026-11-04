use crate::server::*;

/// Category bucket for a finalized file, by extension. Order matches the
/// `stats` JSON: video, archive, app, doc, other.
pub(crate) fn file_category(relative_path: &str) -> usize {
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase());
    match ext.as_deref() {
        Some(
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "ts" | "m4v" | "mp3" | "flac"
            | "wav" | "ogg",
        ) => 0,
        Some("zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "lz4") => 1,
        Some("exe" | "msi" | "dmg" | "deb" | "rpm" | "appimage" | "pkg" | "iso" | "apk") => 2,
        Some(
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "epub"
            | "mobi",
        ) => 3,
        _ => 4,
    }
}

pub(crate) fn handle_event(state: &mut State, envelope: PluginEventEnvelope) {
    match envelope.event {
        PluginEvent::Progress {
            task_id,
            downloaded,
            total,
            speed_bps,
            ..
        } => {
            state.last_progress.insert(task_id.clone(), Instant::now());
            let _ = state
                .store
                .update_progress(&task_id, downloaded, total, speed_bps);
            broadcast(
                state,
                json!({
                    "event": "progress",
                    "task_id": task_id,
                    "downloaded": downloaded,
                    "total": total,
                    "speed_bps": speed_bps,
                }),
            );
        }
        PluginEvent::Metadata { task_id, metadata } => {
            if let Some(total) = metadata.size {
                let _ = state.store.update_total(&task_id, total);
            }
            broadcast(
                state,
                json!({"event": "task_metadata", "task_id": task_id, "metadata": metadata}),
            );
        }
        PluginEvent::Log {
            level,
            message,
            fields,
        } => {
            if let Some(task_id) = fields
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let fields = Value::Object(fields.into_iter().collect());
                let _ = state.store.append_log(
                    &task_id,
                    &format!("{level:?}").to_ascii_lowercase(),
                    &message,
                    Some(&fields),
                );
            } else {
                eprintln!("aether-daemon: plugin log: {message}");
            }
        }
        PluginEvent::Detail { task_id, data } => {
            state.details.insert(task_id.clone(), data.clone());
            broadcast(
                state,
                json!({"event": "task_detail", "task_id": task_id, "detail": data}),
            );
        }
        PluginEvent::Completed { task_id, files, .. } => {
            // A late completion after a watchdog failure/cancel must not
            // resurrect the task.
            if !state.active.contains(&task_id) && !state.live.contains(&task_id) {
                return;
            }
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.last_progress.remove(&task_id);
            state.details.remove(&task_id);
            // The record is authoritative: a "save as" task must finalize into
            // the directory it was created with, even after a restart.
            let record = state.store.task(&task_id).ok().flatten();
            let target_dir = record.as_ref().and_then(|task| task.target_dir.clone());
            let expected = record
                .as_ref()
                .and_then(|task| parse_checksum(&task.settings).ok().flatten());
            let final_dir = destination_dir(state, target_dir.as_deref());
            let staging_root = existing_staging_root(state, &task_id, &final_dir);
            let staging = staging_root.join("staging");
            // Hashing and moving a large file can take seconds; keep the
            // event loop (and every RPC client) responsive by handing the
            // work to a worker thread and applying the result on the loop.
            let tx = state.finalize_tx.clone();
            let job_task_id = task_id.clone();
            let job_staging_root = staging_root.clone();
            state.finalize_pending += 1;
            thread::spawn(move || {
                let result =
                    finalize_task_with_checksum(&staging, &final_dir, &files, expected.as_ref())
                        .map_err(|error| error.to_string());
                let _ = tx.send(FinalizeOutcome {
                    task_id: job_task_id,
                    files,
                    staging_root: job_staging_root,
                    result,
                });
            });
            apply_speed_budgets(state);
            promote_queued(state);
        }
        PluginEvent::Failed { task_id, error } => {
            if !state.active.contains(&task_id) && !state.live.contains(&task_id) {
                return;
            }
            state.active.remove(&task_id);
            state.live.remove(&task_id);
            state.canceling.remove(&task_id);
            state.last_progress.remove(&task_id);
            state.details.remove(&task_id);
            apply_speed_budgets(state);
            if error.kind == aether_plugin_sdk::ErrorKind::Canceled {
                let _ = state.store.set_state(&task_id, TaskState::Canceled);
                let _ = state.store.reset_retry(&task_id);
                let _ = state
                    .store
                    .append_log(&task_id, "info", "canceled by plugin", None);
            } else {
                record_failure(
                    state,
                    &task_id,
                    &format!("{:?}", error.kind).to_ascii_lowercase(),
                    &error.message,
                    error.retryable,
                );
                schedule_retry(state, &task_id, &error.message, error.retryable);
            }
            broadcast(
                state,
                json!({"event": "failed", "task_id": task_id, "error": error}),
            );
            promote_queued(state);
        }
        _ => {}
    }
}

/// Apply a background finalize result on the main loop: record completion,
/// clean the staging tree, broadcast, and promote the next queued task.
pub(crate) fn apply_finalize(state: &mut State, outcome: FinalizeOutcome) {
    let FinalizeOutcome {
        task_id,
        files,
        staging_root,
        result,
    } = outcome;
    match result {
        Ok(moved) => {
            let _ = fs::remove_dir_all(&staging_root);
            cleanup_staging_parent(&staging_root);
            let _ = state.store.complete_task(&task_id, &files);
            let names = moved
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let _ = state.store.append_log(
                &task_id,
                "info",
                &format!("completed: {}", names.join(", ")),
                Some(&json!({"files": names})),
            );
            broadcast(
                state,
                json!({"event": "completed", "task_id": task_id, "files": files}),
            );
        }
        Err(message) => {
            let _ = state.store.fail_task(&task_id, "finalize", &message, false);
            let _ = state.store.append_log(
                &task_id,
                "error",
                &format!("finalize failed: {message}"),
                None,
            );
            broadcast(
                state,
                json!({"event": "failed", "task_id": task_id, "error": message}),
            );
        }
    }
    promote_queued(state);
}

pub(crate) fn recover_tasks(state: &mut State) {
    let tasks = match state.store.recoverable_tasks() {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!("aether-daemon: cannot load recoverable tasks: {error}");
            return;
        }
    };
    for task in tasks {
        // Interrupted running/queued tasks restart while capacity lasts and
        // the rest are demoted to queued. Paused tasks stay paused until the
        // user resumes them explicitly.
        if !matches!(task.state, TaskState::Running | TaskState::Queued) {
            continue;
        }
        match start_from_record(state, &task) {
            Ok(true) => {}
            Ok(false) => {
                let _ = state.store.set_state(&task.id, TaskState::Queued);
            }
            Err(error) => eprintln!("aether-daemon: cannot resume task {}: {error}", task.id),
        }
    }
}
