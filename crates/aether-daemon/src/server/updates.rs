use crate::server::*;

/// The periodic plugin-update pass, run as a *client* of the daemon (the
/// update loop owns no `State`). Skips silently unless the daemon setting
/// `plugin_auto_update` is on; returns what the daemon reported.
pub fn plugin_auto_update_pass(
    socket: &std::path::Path,
) -> Result<Option<serde_json::Value>, DaemonError> {
    let mut client = crate::client::Client::connect(socket)?;
    let settings = client.settings()?;
    if settings["plugin_auto_update"].as_bool() != Some(true) {
        return Ok(None);
    }
    let result = client.call("plugin_update_all", None, None, None, None, json!({}))?;
    Ok(Some(result))
}

/// One app-update pass: fetch and verify the manifest, then — unless the
/// user turned background downloads off — download and, for AppImage,
/// apply the release. Blocking; the loop and the on-demand RPC run it on a
/// worker thread. Returns the status it wrote.
pub(crate) fn update_pass(
    manifest_url: &str,
    public_key: &str,
    appimage: Option<&Path>,
    data_dir: &Path,
    settings_path: &Path,
    status: &Arc<Mutex<serde_json::Value>>,
) -> serde_json::Value {
    // Re-read each pass so the settings switch applies without a restart.
    let auto = Settings::load(settings_path)
        .app_auto_update
        .unwrap_or(true);
    let result = (|| -> Result<serde_json::Value, aether_update::UpdateError> {
        let client = UpdateClient::new(
            manifest_url,
            public_key,
            env!("CARGO_PKG_VERSION"),
            aether_plugin_sdk::current_platform_key(),
            data_dir,
        )?;
        let Some(manifest) = client.check()? else {
            return Ok(json!({"status": "up_to_date"}));
        };
        if !auto {
            // Background downloads are off: report the version and let the
            // user decide from the About panel.
            return Ok(json!({"status": "available", "latest": manifest.version}));
        }
        // AppImage replaces itself in place; every other install downloads
        // the installer the platform ships and reports it for a manual step.
        let asset = client.asset_to_install(&manifest, appimage.is_some())?;
        // Report download progress into the same status cell the RPC reads, so
        // the About panel can draw a bar instead of a frozen "checking".
        let mut last_report = std::time::Instant::now() - Duration::from_secs(1);
        let artifact = client.download_with_progress(&manifest, asset, |written, total| {
            if last_report.elapsed() < Duration::from_millis(200) {
                return;
            }
            last_report = std::time::Instant::now();
            *status.lock().unwrap() = json!({
                "status": "downloading",
                "latest": manifest.version,
                "downloaded": written,
                "total": total,
            });
        })?;
        if asset.kind == "appimage" {
            if let Some(target) = appimage {
                client.apply_appimage(&artifact, target)?;
                return Ok(json!({
                    "status": "ready",
                    "latest": manifest.version,
                    "artifact": artifact.to_string_lossy(),
                }));
            }
        }
        Ok(json!({
            "status": "package",
            "latest": manifest.version,
            "kind": asset.kind,
            "artifact": artifact.to_string_lossy(),
        }))
    })();
    let value = result.unwrap_or_else(|error| {
        json!({
            "status": "error",
            "error": error.to_string(),
        })
    });
    *status.lock().unwrap() = value.clone();
    value
}

pub(crate) fn spawn_update_loop(
    manifest_url: String,
    public_key: String,
    appimage: Option<PathBuf>,
    data_dir: PathBuf,
    settings_path: PathBuf,
    status: Arc<Mutex<serde_json::Value>>,
    socket: std::path::PathBuf,
) {
    thread::spawn(move || {
        // Do not delay daemon startup for a network check.
        thread::sleep(Duration::from_secs(30));
        loop {
            // Plugin updates ride the same cadence; failures are logged, not
            // surfaced through the app-update status.
            match plugin_auto_update_pass(&socket) {
                Ok(Some(result)) => {
                    let updated = result["updated"].as_array().map_or(0, Vec::len);
                    if updated > 0 {
                        eprintln!("aether-daemon: auto-updated {updated} plugin(s)");
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("aether-daemon: plugin auto-update: {error}"),
            }
            update_pass(
                &manifest_url,
                &public_key,
                appimage.as_deref(),
                &data_dir,
                &settings_path,
                &status,
            );
            thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}
