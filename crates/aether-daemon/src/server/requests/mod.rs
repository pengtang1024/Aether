use crate::server::*;

mod plugins;
mod session;
mod settings;
mod stats;
mod tasks;

/// Dispatch one decoded request. Unknown methods are a protocol error so a
/// stale client fails loudly instead of silently doing nothing.
pub(crate) fn handle_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    match request.method.as_str() {
        "ping" => Ok(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")})),
        "probe" => {
            let url = request
                .url
                .ok_or_else(|| DaemonError::Protocol("probe needs url".into()))?;
            let outcomes = state
                .manager
                .probe(&url)
                .into_iter()
                .map(|outcome| {
                    json!({
                        "plugin_id": outcome.plugin_id,
                        "supported": outcome.result.as_ref().map(|result| result.supported),
                        "priority": outcome.result.as_ref().map(|result| result.priority),
                        "filename": outcome.result.as_ref().and_then(|result| result.suggested_filename.clone()),
                        "size": outcome.result.as_ref().and_then(|result| result.metadata.as_ref().and_then(|metadata| metadata.size)),
                        "reason": outcome.result.as_ref().and_then(|result| result.reason.clone()),
                        "error": outcome.error,
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({ "outcomes": outcomes }))
        }
        "add" => add_task(state, request),
        "list" => tasks::list_request(state, request),
        "get" => tasks::get_request(state, request),
        "logs" => tasks::logs_request(state, request),
        "pause" => tasks::pause_request(state, request),
        "resume" => tasks::resume_request(state, request),
        "save_session" => session::save_session(state),
        "task_summary" => tasks::task_summary_request(state, request),
        "set_task_limit" => session::set_task_limit(state, request),
        "queued_order" => Ok(json!({ "tasks": state.store.waiting_order()? })),
        "reorder_queue" => session::reorder_queue(state, request),
        "get_peers" => session::get_peers(state, request),
        "cancel" => tasks::cancel_request(state, request),
        "delete" => tasks::delete_request(state, request),
        "resolve_proxy" => session::resolve_proxy_request(state, request),
        "plugins" => plugins::plugins_request(state, request),
        "set_plugin_enabled" => plugins::set_plugin_enabled_request(state, request),
        "stats" => stats::stats_request(state, request),
        "db_check" => Ok(json!({ "result": state.store.integrity_check()? })),
        "backup" => stats::backup_request(state, request),
        "get_plugin_settings" => plugins::get_plugin_settings_request(state, request),
        "set_plugin_settings" => plugins::set_plugin_settings_request(state, request),
        "get_settings" => Ok(json!({ "settings": state.settings })),
        "set_settings" => settings::set_settings_request(state, request),
        "install_plugin" => install_plugin(state, request),
        "uninstall_plugin" => uninstall_plugin(state, request),
        "rollback_plugin" => rollback_plugin(state, request),
        "rescan_plugins" => plugins::rescan_plugins_request(state, request),
        "plugin_updates" => plugin_updates(state),
        "plugin_update_all" => plugin_update_all(state),
        "update_status" => Ok(state.update.as_ref().map_or_else(
            || json!({"status": "disabled"}),
            |status| status.lock().unwrap().clone(),
        )),
        "update_check" => stats::update_check_request(state, request),
        "shutdown" => Ok(json!({"ok": true})),
        other => Err(DaemonError::Protocol(format!("unknown method {other}"))),
    }
}
