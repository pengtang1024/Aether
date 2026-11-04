use crate::server::*;

pub(crate) fn plugins_request(state: &mut State, _request: Request) -> Result<Value, DaemonError> {
    let allow_external = state.manager.allow_external_process();
    let backup_root = state.data_dir.join("plugin-backups");
    let service_status: HashMap<String, ServiceStatus> = state
        .services
        .statuses()
        .into_iter()
        .map(|status| (status.service.clone(), status))
        .collect();
    let stored_settings = state.store.all_plugin_settings()?;
    let plugins = state
        .manager
        .registry()
        .iter()
        .map(|entry| {
            let service = entry
                .manifest
                .service
                .as_deref()
                .and_then(|service| service_status.get(service));
            let running = service.map_or_else(
                || state.manager.running_pid(&entry.manifest.id).is_some(),
                |status| status.running,
            );
            // A policy-blocked plugin is still listed (and can still
            // be disabled/uninstalled) but the UI must say why it
            // cannot start instead of failing at task time.
            let blocked = !allow_external
                && entry
                    .manifest
                    .capabilities
                    .contains(&aether_plugin_sdk::Capability::ExternalProcess);
            json!({
                "id": entry.manifest.id,
                "name": entry.manifest.name,
                "version": entry.manifest.version,
                "protocols": entry.manifest.protocols,
                "capabilities": entry.manifest.capabilities,
                "features": entry.manifest.features,
                "enabled": entry.enabled,
                "blocked": blocked,
                "service": entry.manifest.kind == PluginKind::Service,
                "running": running,
                "last_error": service.and_then(|status| status.last_error.clone()),
                "has_backup": backup_root
                    .join(&entry.manifest.id)
                    .join("plugin.json")
                    .is_file(),
                "settings_schema": entry.manifest.settings_schema.clone(),
                "detail_schema": entry.manifest.detail_schema.clone(),
                "settings": stored_settings
                    .get(&entry.manifest.id)
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "plugins": plugins,
        "external_process_allowed": allow_external,
    }))
}

pub(crate) fn set_plugin_enabled_request(
    state: &mut State,
    request: Request,
) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("set_plugin_enabled needs plugin".into()))?;
    let enabled = request
        .settings
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| DaemonError::Protocol("set_plugin_enabled needs settings.enabled".into()))?;
    if !enabled {
        // Refuse to pull the rug from under running work.
        if plugin_is_busy(state, &plugin_id) {
            return Err(DaemonError::Protocol(format!(
                "plugin {plugin_id} still has running tasks"
            )));
        }
    }
    state
        .manager
        .registry_mut()
        .set_enabled(&plugin_id, enabled)?;
    let is_service = state
        .manager
        .registry()
        .get(&plugin_id)
        .is_some_and(|entry| entry.manifest.kind == PluginKind::Service);
    if !enabled {
        state.manager.stop(&plugin_id);
        forget_plugin_tasks(state, &plugin_id);
    }
    if is_service {
        state.services.sync(state.manager.registry());
    }
    state.store.set_plugin_disabled(&plugin_id, !enabled)?;
    let entry = state
        .manager
        .registry()
        .get(&plugin_id)
        .map(|entry| {
            json!({
                "id": entry.manifest.id,
                "enabled": entry.enabled,
            })
        })
        .unwrap_or(serde_json::Value::Null);
    Ok(json!({ "plugin": entry }))
}

pub(crate) fn get_plugin_settings_request(
    state: &mut State,
    _request: Request,
) -> Result<Value, DaemonError> {
    Ok(json!({ "plugin_settings": state.store.all_plugin_settings()? }))
}

pub(crate) fn set_plugin_settings_request(
    state: &mut State,
    request: Request,
) -> Result<Value, DaemonError> {
    let plugin_id = request
        .plugin
        .clone()
        .ok_or_else(|| DaemonError::Protocol("set_plugin_settings needs plugin".into()))?;
    let entry = state
        .manager
        .registry()
        .iter()
        .find(|entry| entry.manifest.id == plugin_id)
        .ok_or_else(|| DaemonError::Protocol(format!("unknown plugin {plugin_id}")))?;
    aether_host::plugin_settings::validate(
        entry.manifest.settings_schema.as_ref(),
        &request.settings,
    )
    .map_err(|error| DaemonError::Protocol(format!("{plugin_id}: {error}")))?;
    state
        .store
        .save_plugin_settings(&plugin_id, &request.settings)?;
    Ok(json!({ "plugin_id": plugin_id, "settings": request.settings }))
}

pub(crate) fn rescan_plugins_request(
    state: &mut State,
    _request: Request,
) -> Result<Value, DaemonError> {
    let total = rescan_registry(state)?;
    state.services.sync(state.manager.registry());
    Ok(json!({ "plugins": total }))
}
