use crate::server::*;

/// Centralized staging lives under the download root; a target on another
/// filesystem gets its own hidden staging directory so finalization is a
/// rename instead of a cross-device copy.
fn task_staging_root(state: &State, task_id: &str, final_dir: &Path) -> PathBuf {
    if same_filesystem(&state.download_dir, final_dir) {
        state.download_dir.join(".aether").join(task_id)
    } else {
        final_dir.join(".aether-staging").join(task_id)
    }
}

/// Drop staging directories left behind by a crash for tasks that are no
/// longer runnable. The DB is the source of truth.
pub(crate) fn cleanup_stale_staging(state: &State) {
    let Ok(entries) = fs::read_dir(&state.data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("task-") {
            continue;
        }
        let keep = state.store.task(name).ok().flatten().is_some_and(|task| {
            matches!(
                task.state,
                TaskState::Running | TaskState::Queued | TaskState::Paused
            )
        });
        if !keep {
            let _ = fs::remove_dir_all(&path);
        }
    }
    if let Ok(tasks) = state.store.list_tasks_with_files(10_000, false) {
        for task in tasks {
            if task.state != TaskState::Completed {
                continue;
            }
            let Some(target) = task.target_dir.as_deref() else {
                continue;
            };
            let root = Path::new(target).join(".aether-staging").join(&task.id);
            if root.is_dir() {
                let _ = fs::remove_dir_all(&root);
                cleanup_staging_parent(&root);
            }
        }
    }
}

/// Remove the cross-device `.aether-staging` parent when the last task under
/// it is gone. The centralized `.aether` parent is shared with the DB/settings
/// and is intentionally left alone.
pub(crate) fn cleanup_staging_parent(root: &Path) {
    let Some(parent) = root.parent() else {
        return;
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some(".aether-staging") {
        let _ = fs::remove_dir(parent);
    }
}

/// Prefer an existing staging directory over the computed one: a mount can
/// change between runs, and a partial download must not be abandoned.
pub(crate) fn existing_staging_root(state: &State, task_id: &str, final_dir: &Path) -> PathBuf {
    let centralized = state.download_dir.join(".aether").join(task_id);
    let target = final_dir.join(".aether-staging").join(task_id);
    if centralized.join("staging").is_dir() {
        centralized
    } else if target.join("staging").is_dir() {
        target
    } else {
        task_staging_root(state, task_id, final_dir)
    }
}

/// Prepare the staging directory, falling back to the centralized root when
/// volume detection was wrong or the target mount refuses the hidden folder.
pub(crate) fn prepare_staging(
    state: &State,
    task_id: &str,
    final_dir: &Path,
) -> std::io::Result<PathBuf> {
    let preferred = existing_staging_root(state, task_id, final_dir).join("staging");
    let fallback = state
        .download_dir
        .join(".aether")
        .join(task_id)
        .join("staging");
    create_dir_with_fallback(&preferred, &fallback)
}

/// Create `preferred`, falling back to `fallback` when the target mount is
/// read-only, full enough to refuse a directory, or not a directory at all.
pub(crate) fn create_dir_with_fallback(
    preferred: &Path,
    fallback: &Path,
) -> std::io::Result<PathBuf> {
    match fs::create_dir_all(preferred) {
        Ok(()) => Ok(preferred.to_path_buf()),
        Err(error) => {
            eprintln!(
                "aether-daemon: cannot create staging under {}: {error}; using the download root",
                preferred.display()
            );
            fs::create_dir_all(fallback)?;
            Ok(fallback.to_path_buf())
        }
    }
}

/// Bytes still to be written into `dir` by running plus queued/paused tasks.
/// The queue is included because a burst of adds must not each pass the check
/// against the same free space.
pub(crate) fn reserved_in(state: &State, dir: &Path) -> u64 {
    let mut reserved: u64 = state
        .active
        .iter()
        .filter_map(|task_id| state.store.task(task_id).ok().flatten())
        .filter(|task| destination_dir(state, task.target_dir.as_deref()) == dir)
        .map(|task| task.total.unwrap_or(0).saturating_sub(task.downloaded))
        .sum();
    if let Ok(groups) = state.store.reserved_by_target() {
        reserved = reserved.saturating_add(
            groups
                .into_iter()
                .filter(|(target, _)| destination_dir(state, target.as_deref()) == dir)
                .map(|(_, bytes)| bytes)
                .sum::<u64>(),
        );
    }
    reserved
}

/// Bytes that will eventually pass through the daemon's staging root.
pub(crate) fn reserved_staging(state: &State) -> u64 {
    let mut reserved: u64 = state
        .active
        .iter()
        .filter_map(|task_id| state.store.task(task_id).ok().flatten())
        .map(|task| task.total.unwrap_or(0).saturating_sub(task.downloaded))
        .sum();
    if let Ok(groups) = state.store.reserved_by_target() {
        reserved = reserved.saturating_add(groups.into_iter().map(|(_, bytes)| bytes).sum::<u64>());
    }
    reserved
}

/// Where a task's files land: the recorded directory or the daemon default.
pub(crate) fn destination_dir(state: &State, target_dir: Option<&str>) -> PathBuf {
    target_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| state.download_dir.clone())
}

/// Resolve the `dir` field of `add`. Absolute paths are honored (same-user
/// daemon: a client may legitimately "save as" elsewhere); relative paths stay
/// inside the download root and may not climb out of it.
pub(crate) fn resolve_target_dir(
    state: &State,
    requested: Option<&str>,
) -> Result<PathBuf, DaemonError> {
    let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(state.download_dir.clone());
    };
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(DaemonError::Protocol(format!(
            "invalid destination {raw:?}: relative paths may not contain '..'"
        )));
    }
    Ok(state.download_dir.join(path))
}
