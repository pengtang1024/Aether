use crate::server::*;

/// An update client for the plugin channel, or `None` when the daemon was
/// not configured with both the manifest URL and the release key (the same
/// trust root the app-update loop uses).
fn plugin_update_client(state: &State) -> Option<aether_update::UpdateClient> {
    let url = state.update_url.as_deref()?;
    let key = state.plugin_public_key.as_deref()?;
    aether_update::UpdateClient::new(
        url,
        key,
        &state.host_version,
        aether_plugin_sdk::current_platform_key(),
        &state.data_dir.join("plugin-updates"),
    )
    .ok()
}

/// Compare installed plugin versions against the update manifest.
pub(crate) fn plugin_updates(state: &State) -> Result<Value, DaemonError> {
    let Some(client) = plugin_update_client(state) else {
        // Honest about the precondition instead of reporting "no updates":
        // nothing was checked.
        return Ok(json!({ "configured": false, "updates": [] }));
    };
    let manifest = client
        .check_manifest()
        .map_err(|error| DaemonError::Protocol(format!("update manifest: {error}")))?;
    let installed: std::collections::HashMap<String, String> = state
        .manager
        .registry()
        .iter()
        .map(|entry| (entry.manifest.id.clone(), entry.manifest.version.clone()))
        .collect();
    let pins = state.settings.plugin_pins.clone().unwrap_or_default();
    let pinned = manifest
        .plugins
        .keys()
        .filter(|id| pins.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    let updates = manifest
        .plugins
        .iter()
        .filter(|(id, entry)| {
            !pins.contains_key(*id)
                && installed.get(*id).map(String::as_str) != Some(entry.version.as_str())
        })
        .map(|(id, entry)| {
            json!({
                "plugin_id": id,
                "installed": installed.get(id),
                "latest": entry.version,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "configured": true, "updates": updates, "pinned": pinned }))
}

/// Download, verify and install every outdated plugin from the update
/// manifest. Each plugin is independent: one failure (busy, bad archive,
/// signature) is reported and the rest continue.
pub(crate) fn plugin_update_all(state: &mut State) -> Result<Value, DaemonError> {
    let Some(client) = plugin_update_client(state) else {
        return Err(DaemonError::Protocol(
            "plugin updates need update_url and the release key configured".into(),
        ));
    };
    let progress = state.plugin_progress.clone();
    *progress.lock().unwrap() = json!({"running": true, "phase": "checking"});
    let manifest = match client.check_manifest() {
        Ok(manifest) => manifest,
        Err(error) => {
            *progress.lock().unwrap() = json!({
                "running": false,
                "phase": "done",
                "done": 0,
                "total": 0,
                "updated": 0,
                "error": error.to_string(),
            });
            return Err(DaemonError::Protocol(format!("update manifest: {error}")));
        }
    };
    let installed: std::collections::HashMap<String, String> = state
        .manager
        .registry()
        .iter()
        .map(|entry| (entry.manifest.id.clone(), entry.manifest.version.clone()))
        .collect();

    let pins = state.settings.plugin_pins.clone().unwrap_or_default();
    let mut updated = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    let mut pending = Vec::new();
    for (id, entry) in &manifest.plugins {
        if pins.contains_key(id) {
            skipped.push(id.clone());
            continue;
        }
        if installed.get(id).map(String::as_str) == Some(entry.version.as_str()) {
            continue;
        }
        pending.push((id, entry));
    }
    let total = pending.len();
    for (done, (id, entry)) in pending.into_iter().enumerate() {
        match update_one_plugin(state, &client, id, entry, &progress, done, total) {
            Ok(()) => updated.push(id.clone()),
            Err(error) => failed.push(json!({ "plugin_id": id, "error": error.to_string() })),
        }
    }
    let mut value = json!({
        "running": false,
        "phase": "done",
        "done": updated.len(),
        "total": total,
        "updated": updated.len(),
    });
    if let Some(error) = failed.first() {
        value["error"] = error["error"].clone();
    }
    *progress.lock().unwrap() = value;
    Ok(json!({
        "updated": updated,
        "failed": failed,
        "skipped_pinned": skipped,
        "up_to_date": updated.is_empty() && failed.is_empty(),
    }))
}

fn update_one_plugin(
    state: &mut State,
    client: &aether_update::UpdateClient,
    id: &str,
    entry: &aether_update::PluginUpdateEntry,
    progress: &Arc<Mutex<Value>>,
    done: usize,
    total: usize,
) -> Result<(), DaemonError> {
    let staging_root = state
        .data_dir
        .join("plugin-updates")
        .join(format!("staging-{id}"));
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root)?;
    }
    // Same 200 ms throttle as the app-update pass: the UI only polls `stats`.
    let mut last_report = Instant::now() - Duration::from_secs(1);
    let archive = client
        .download_artifact_with_progress(
            &entry.url,
            &entry.sha256,
            entry.size,
            &state.data_dir.join("plugin-updates"),
            &format!("{id}.tar.gz"),
            &mut |written, size| {
                if last_report.elapsed() < Duration::from_millis(200) {
                    return;
                }
                last_report = Instant::now();
                *progress.lock().unwrap() = json!({
                    "running": true,
                    "phase": "downloading",
                    "plugin": id,
                    "done": done,
                    "total": total,
                    "downloaded": written,
                    "size": size.or(entry.size),
                });
            },
        )
        .map_err(|error| DaemonError::Protocol(format!("download {id}: {error}")))?;
    let unpacked = staging_root.join("pkg");
    let unpack = aether_update::unpack_plugin_archive(&archive, &unpacked, 256 * 1024 * 1024);
    if let Err(error) = unpack {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(DaemonError::Protocol(error.to_string()));
    }
    let result = install_package(state, &unpacked);
    let _ = fs::remove_dir_all(&staging_root);
    result.map(|_| ())
}
