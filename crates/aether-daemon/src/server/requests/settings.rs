use crate::server::*;

pub(crate) fn set_settings_request(
    state: &mut State,
    request: Request,
) -> Result<Value, DaemonError> {
    // Partial update: only keys that are present change. `proxy: null`
    // clears the proxy and falls back to the plugin default; blank
    // user_agent/dns input clears those knobs the same way. The limits
    // need a concrete number instead — `null` is rejected below.
    let patch = request
        .settings
        .as_object()
        .ok_or_else(|| DaemonError::Protocol("set_settings needs an object".into()))?;
    // The file is parsed with `deny_unknown_fields`, so the RPC
    // rejects the same typos instead of storing nothing quietly.
    for key in patch.keys() {
        if !KEYS.contains(&key.as_str()) {
            return Err(DaemonError::Protocol(format!("unknown setting {key}")));
        }
    }

    // Parse and validate the whole patch before committing a single
    // field: a rejected patch leaves the running settings untouched.
    let mut next = state.settings.clone();
    if let Some(proxy) = patch.get("proxy") {
        let proxy: Option<ProxySetting> = serde_json::from_value(proxy.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid proxy setting: {error}")))?;
        if let Some(proxy) = &proxy {
            proxy.validate().map_err(DaemonError::Protocol)?;
        }
        next.proxy = proxy;
    }
    if let Some(user_agent) = patch.get("user_agent") {
        let user_agent: Option<String> = serde_json::from_value(user_agent.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid user_agent: {error}")))?;
        next.user_agent = user_agent.filter(|agent| !agent.trim().is_empty());
    }
    if let Some(ip_mode) = patch.get("ip_mode") {
        next.ip_mode = serde_json::from_value(ip_mode.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid ip_mode: {error}")))?;
    }
    if let Some(dns) = patch.get("dns") {
        let dns: Option<String> = serde_json::from_value(dns.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid dns: {error}")))?;
        let dns = dns.filter(|servers| !servers.trim().is_empty());
        if let Some(servers) = &dns {
            aether_plugin_sdk::NetworkConfig::parse_dns_servers(servers)
                .map_err(DaemonError::Protocol)?;
        }
        next.dns = dns;
    }
    if let Some(value) = patch.get("max_concurrent") {
        next.max_concurrent = Some(parse_limit(value, "max_concurrent")?);
    }
    if let Some(value) = patch.get("max_speed_bps") {
        next.max_speed_bps = Some(parse_limit(value, "max_speed_bps")?);
    }
    if let Some(value) = patch.get("connections") {
        next.connections = Some(parse_limit(value, "connections")?);
    }
    if let Some(value) = patch.get("retry_max") {
        next.retry_max = Some(parse_limit(value, "retry_max")?);
    }
    if let Some(value) = patch.get("default_headers") {
        let headers: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid default_headers: {error}")))?;
        next.default_headers = headers.filter(|list| !list.trim().is_empty());
    }
    if let Some(value) = patch.get("ftp_passive") {
        next.ftp_passive = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid ftp_passive: {error}")))?;
    }
    if let Some(value) = patch.get("bt_listen_port") {
        next.bt_listen_port = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid bt_listen_port: {error}")))?;
    }
    if let Some(value) = patch.get("bt_dht") {
        next.bt_dht = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid bt_dht: {error}")))?;
    }
    if let Some(value) = patch.get("bt_upload_bps") {
        next.bt_upload_bps = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid bt_upload_bps: {error}")))?;
    }
    if let Some(value) = patch.get("bt_blocklist") {
        let blocklist: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid bt_blocklist: {error}")))?;
        next.bt_blocklist = blocklist.filter(|path| !path.trim().is_empty());
    }
    if let Some(value) = patch.get("bt_trackers") {
        let trackers: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid bt_trackers: {error}")))?;
        next.bt_trackers = trackers.filter(|list| !list.trim().is_empty());
    }
    if let Some(value) = patch.get("schedule_on") {
        next.schedule_on = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid schedule_on: {error}")))?;
    }
    if let Some(value) = patch.get("schedule_start") {
        let text: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid schedule_start: {error}")))?;
        next.schedule_start = text.filter(|text| !text.trim().is_empty());
    }
    if let Some(value) = patch.get("schedule_end") {
        let text: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid schedule_end: {error}")))?;
        next.schedule_end = text.filter(|text| !text.trim().is_empty());
    }
    if let Some(value) = patch.get("rpc_listen") {
        let text: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid rpc_listen: {error}")))?;
        // `Some("")` is an explicit "off"; `None` (null) returns to
        // the built-in default on the next boot.
        next.rpc_listen = text.map(|text| text.trim().to_string());
    }
    if let Some(value) = patch.get("rpc_secret") {
        let text: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid rpc_secret: {error}")))?;
        // `Some("")` clears the token; the patch is rejected by `validate`
        // unless it also turns the listener off. `None` (null) is the same:
        // boot re-seeds it, but only while the listener is off.
        next.rpc_secret = text.map(|text| text.trim().to_string());
    }
    if let Some(value) = patch.get("ffmpeg_path") {
        let text: Option<String> = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid ffmpeg_path: {error}")))?;
        next.ffmpeg_path = text.filter(|text| !text.trim().is_empty());
    }
    if let Some(value) = patch.get("plugin_auto_update") {
        next.plugin_auto_update = serde_json::from_value(value.clone()).map_err(|error| {
            DaemonError::Protocol(format!("invalid plugin_auto_update: {error}"))
        })?;
    }
    if let Some(value) = patch.get("plugin_pins") {
        let pins: Option<std::collections::BTreeMap<String, String>> =
            serde_json::from_value(value.clone())
                .map_err(|error| DaemonError::Protocol(format!("invalid plugin_pins: {error}")))?;
        next.plugin_pins = pins.filter(|pins| !pins.is_empty());
    }
    if let Some(value) = patch.get("plugin_external_process") {
        next.plugin_external_process = serde_json::from_value(value.clone()).map_err(|error| {
            DaemonError::Protocol(format!("invalid plugin_external_process: {error}"))
        })?;
    }
    if let Some(value) = patch.get("app_auto_update") {
        next.app_auto_update = serde_json::from_value(value.clone())
            .map_err(|error| DaemonError::Protocol(format!("invalid app_auto_update: {error}")))?;
    }
    next.validate()?;

    let network_before = state.settings.network();
    let proxy_changed = patch.contains_key("proxy");
    let external_process_before = state.settings.plugin_external_process.unwrap_or(true);
    state.settings = next;
    let external_process_after = state.settings.plugin_external_process.unwrap_or(true);
    state
        .manager
        .set_allow_external_process(external_process_after);
    if external_process_before && !external_process_after {
        // The policy only gates the next spawn; retire idle plugins
        // that need an external process so the setting takes effect
        // without a daemon restart. Busy plugins finish their task.
        stop_idle_external_plugins(state);
    }
    if proxy_changed {
        // Applies to plugins spawned from now on; running downloads
        // keep the proxy they started with. PAC is evaluated per task,
        // so a fresh resolver is enough.
        state.pac = state
            .settings
            .proxy
            .as_ref()
            .and_then(ProxySetting::resolver);
        state
            .manager
            .set_proxy(init_proxy(state.settings.proxy.as_ref()));
        // Re-detect the desktop proxy for the new mode on next use.
        state.system_proxy = SystemProxyCache::default();
        state.system_pac = None;
    }
    apply_limits(state);
    // A raised cap starts waiting tasks immediately; a lowered one
    // lets the running ones finish — nothing is killed.
    promote_queued(state);
    // The RPC token is read live by the accept loop, so a rotation is
    // effective for the next connection (no listener restart). Clearing it is
    // only legal together with `rpc_listen: ""`, and even then the bound
    // listener keeps the old token until the restart that actually unbinds it.
    if patch.contains_key("rpc_secret") {
        let next_secret = state
            .settings
            .rpc_secret
            .clone()
            .filter(|secret| !secret.trim().is_empty());
        if state.rpc_base.is_some() && next_secret.is_none() {
            eprintln!(
                "aether-daemon: rpc_secret cleared; keeping the live token until the listener \
                 restarts (rpc_listen is off)"
            );
        } else {
            *state.rpc_secret.lock().unwrap() = next_secret;
        }
        let live = state.rpc_secret.lock().unwrap().clone();
        state.services.set_endpoint(state.rpc_base.clone(), live);
    }
    // A schedule change takes effect now, not at the next periodic
    // tick: closing the window pauses, opening resumes.
    if patch.keys().any(|key| key.starts_with("schedule_")) {
        tick_schedule(state);
    }
    // New network values reach plugins on their next spawn; running
    // downloads keep the clients they were started with. Idle plugin
    // processes are stopped right away so the very next task uses the
    // new configuration instead of waiting for a daemon restart.
    let network_after = state.settings.network();
    if network_before != network_after {
        restart_idle_plugins(state);
    }
    state.manager.set_network(network_after);
    state.settings.save(&state.settings_path)?;
    broadcast(
        state,
        json!({"event": "settings_changed", "settings": state.settings}),
    );
    Ok(json!({ "settings": state.settings }))
}
