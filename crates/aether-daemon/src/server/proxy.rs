use crate::server::*;

/// The `Init::proxy` for plugin processes: PAC hands plugins the environment
/// default, because the real proxy is resolved per task.
pub(crate) fn init_proxy(proxy: Option<&ProxySetting>) -> Option<ProxyConfig> {
    proxy.and_then(ProxySetting::init_config)
}

/// Network settings ride into plugin processes at spawn, so after a change
/// every idle plugin process is stopped; its next use respawns it with the
/// new configuration. Plugins with running or paused work keep their process
/// (and therefore their old settings) until that work finishes.
/// Stop idle plugins that declare `external-process`; used when the policy
/// flips to "blocked". Busy plugins keep running until their task ends.
pub(crate) fn stop_idle_external_plugins(state: &mut State) {
    let busy: HashSet<String> = state
        .live
        .iter()
        .filter_map(|task_id| {
            state
                .store
                .task(task_id)
                .ok()
                .flatten()
                .map(|task| task.plugin_id)
        })
        .collect();
    let ids = state
        .manager
        .registry()
        .iter()
        .filter(|entry| {
            entry
                .manifest
                .capabilities
                .contains(&aether_plugin_sdk::Capability::ExternalProcess)
        })
        .map(|entry| entry.manifest.id.clone())
        .filter(|id| !busy.contains(id))
        .collect::<Vec<_>>();
    for id in ids {
        state.manager.stop(&id);
    }
}

/// Start the aria2 compatibility service when an enabled `kind: "service"`
/// plugin declares the `aria2` protocol. Failure is logged, not fatal:
/// downloads keep working and HTTP requests answer 503.
pub(crate) fn restart_idle_plugins(state: &mut State) {
    let busy: HashSet<String> = state
        .live
        .iter()
        .filter_map(|task_id| {
            state
                .store
                .task(task_id)
                .ok()
                .flatten()
                .map(|task| task.plugin_id)
        })
        .collect();
    for plugin_id in state
        .manager
        .registry()
        .iter()
        .map(|entry| entry.manifest.id.clone())
        .collect::<Vec<_>>()
    {
        if !busy.contains(&plugin_id) {
            state.manager.stop(&plugin_id);
        }
    }
}

/// Resolve the proxy for one task. Errors are logged and swallowed: a broken
/// PAC (or an unreadable desktop setting) must not stop downloads, it just
/// falls back to the process default.
pub(crate) fn resolve_task_proxy(
    state: &mut State,
    url: &str,
    allow_fetch: bool,
) -> Option<ProxyConfig> {
    if state.pac.is_some() {
        let pac = state.pac.as_mut()?;
        return match pac.resolve(url, allow_fetch) {
            Ok(proxy) => Some(proxy),
            Err(error) => {
                eprintln!("aether-daemon: pac for {url}: {error}");
                None
            }
        };
    }
    // `system` (and the default, unset) means "follow the desktop"; explicit
    // custom/direct modes are handled by the plugins themselves.
    match state.settings.proxy.as_ref() {
        None | Some(ProxySetting::System) => {}
        Some(_) => return None,
    }
    match state.system_proxy.lookup().cloned() {
        Some(SystemProxy::Manual(config)) => Some(config),
        Some(SystemProxy::Pac(pac_url)) => resolve_system_pac(state, &pac_url, url, allow_fetch),
        None => None,
    }
}

/// A desktop-configured PAC URL (GNOME/KDE automatic mode) is evaluated like an
/// explicit PAC setting, but with its own cache slot.
fn resolve_system_pac(
    state: &mut State,
    pac_url: &str,
    url: &str,
    allow_fetch: bool,
) -> Option<ProxyConfig> {
    let needs_resolver = state
        .system_pac
        .as_ref()
        .map(|(current, _)| current != pac_url)
        .unwrap_or(true);
    if needs_resolver {
        state.system_pac = Some((
            pac_url.to_string(),
            PacResolver::new(Some(pac_url.to_string()), None),
        ));
    }
    let (_, resolver) = state.system_pac.as_mut()?;
    match resolver.resolve(url, allow_fetch) {
        Ok(proxy) => Some(proxy),
        Err(error) => {
            eprintln!("aether-daemon: system pac {pac_url} for {url}: {error}");
            None
        }
    }
}
