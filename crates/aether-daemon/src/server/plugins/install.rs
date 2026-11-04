use crate::server::*;

/// Re-scan `plugins_dir` and replace the registry, re-applying the
/// persisted disable list. Running plugin processes are left alone: they
/// keep serving their running tasks, and the next spawn picks up whatever
/// the rescan found.
pub(crate) fn rescan_registry(state: &mut State) -> Result<usize, DaemonError> {
    let registry = PluginRegistry::discover(std::slice::from_ref(&state.plugins_dir))?;
    *state.manager.registry_mut() = registry;
    for plugin_id in state.store.disabled_plugins()? {
        let _ = state.manager.registry_mut().set_enabled(&plugin_id, false);
    }
    Ok(state.manager.registry().iter().count())
}

/// Install a plugin package from a local directory into `plugins_dir`.
///
/// Order matters: validate everything (identity, API, host compatibility,
/// executable path safety, signature, file hashes) *before* touching the
/// installed copy, then swap in via a staging directory so a failed copy
/// cannot leave a half-installed package behind.
pub(crate) fn install_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let path = request
        .settings
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| DaemonError::Protocol("install_plugin needs settings.path".into()))?;
    let progress = state.plugin_progress.clone();
    *progress.lock().unwrap() = json!({"running": true, "phase": "checking"});
    let result = install_package(state, Path::new(path));
    *progress.lock().unwrap() = match &result {
        Ok(_) => {
            json!({"running": false, "phase": "done", "done": 1, "total": 1, "updated": 1})
        }
        Err(error) => json!({
            "running": false,
            "phase": "done",
            "done": 0,
            "total": 1,
            "updated": 0,
            "error": error.to_string(),
        }),
    };
    result
}

/// True while a task owned by `plugin_id` is still running. Disabling,
/// replacing, rolling back or uninstalling a plugin must never pull the
/// binary out from under a live download.
pub(crate) fn plugin_is_busy(state: &State, plugin_id: &str) -> bool {
    state.active.iter().any(|task_id| {
        state
            .store
            .task(task_id)
            .ok()
            .flatten()
            .is_some_and(|task| task.plugin_id == plugin_id)
    })
}

/// Forget the in-session plugin processes of `plugin_id`: its tasks must be
/// respawned from their stored record instead of receiving an in-process
/// Resume. Called after the plugin is disabled or uninstalled.
pub(crate) fn forget_plugin_tasks(state: &mut State, plugin_id: &str) {
    state.live.retain(|task_id| {
        state
            .store
            .task(task_id)
            .ok()
            .flatten()
            .is_none_or(|task| task.plugin_id != plugin_id)
    });
}

/// Remove an installed plugin package. Refuses while its tasks are running
/// (the same rule as disable/install), deletes the package and the retained
/// rollback copy, and forgets the persisted disabled flag so a later reinstall
/// starts enabled.
pub(crate) fn uninstall_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("uninstall_plugin needs plugin".into()))?;
    let entry = state
        .manager
        .registry()
        .get(&plugin_id)
        .ok_or_else(|| DaemonError::Protocol(format!("plugin {plugin_id} is not installed")))?;
    // Canonicalize both sides: a symlinked package must not let the delete
    // escape the plugins directory. The registry only accepts direct child
    // packages, and `plugin_id` is validated by the manifest parser.
    let canonical_root = fs::canonicalize(&state.plugins_dir)?;
    let target = fs::canonicalize(&entry.root)?;
    if target.parent() != Some(canonical_root.as_path()) {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} does not live directly under the plugins directory"
        )));
    }
    if plugin_is_busy(state, &plugin_id) {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} still has running tasks"
        )));
    }
    state.manager.stop(&plugin_id);
    forget_plugin_tasks(state, &plugin_id);
    fs::remove_dir_all(&target)?;
    let _ = fs::remove_dir_all(state.data_dir.join("plugin-backups").join(&plugin_id));
    state.store.set_plugin_disabled(&plugin_id, false)?;
    rescan_registry(state)?;
    state.services.sync(state.manager.registry());
    Ok(json!({ "plugin_id": plugin_id, "removed": true }))
}

/// Reinstall the retained previous version. The backup goes through the same
/// verified install path as a fresh package (manifest, host range, signature
/// and file hashes), and the currently installed copy becomes the new
/// backup — rollback is repeatable, not destructive.
pub(crate) fn rollback_plugin(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("rollback_plugin needs plugin".into()))?;
    let backup = state.data_dir.join("plugin-backups").join(&plugin_id);
    if !backup.join("plugin.json").is_file() {
        return Err(DaemonError::Protocol(format!(
            "plugin {plugin_id} has no retained previous version"
        )));
    }
    let mut value = install_package(state, &backup)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("rolled_back".into(), Value::Bool(true));
    }
    Ok(value)
}
