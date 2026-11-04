use crate::server::*;

/// Install one verified package directory into `plugins_dir` (shared by the
/// `install_plugin` RPC and the update channel's unpacked downloads).
pub(crate) fn install_package(state: &mut State, package: &Path) -> Result<Value, DaemonError> {
    let manifest_path = package.join("plugin.json");
    let raw = fs::read_to_string(&manifest_path).map_err(|error| {
        DaemonError::Protocol(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let manifest_value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;

    // Structural validation: identity, executable map, host bounds, and the
    // current platform's executable existing inside the package with no
    // path traversal (resolve_executable canonicalizes and containment-checks).
    let manifest = aether_plugin_sdk::manifest::PluginManifest::from_json(&raw)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;
    manifest
        .validate()
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?;
    if manifest.api != aether_plugin_sdk::API_VERSION {
        return Err(DaemonError::Protocol(format!(
            "plugin {} targets API {} but the host speaks API {}",
            manifest.id,
            manifest.api,
            aether_plugin_sdk::API_VERSION
        )));
    }
    if !manifest
        .is_compatible_with_host(&state.host_version)
        .map_err(|error| DaemonError::Protocol(format!("plugin.json: {error}")))?
    {
        return Err(DaemonError::Protocol(format!(
            "plugin {} is not compatible with host {}",
            manifest.id, state.host_version
        )));
    }
    manifest
        .resolve_executable(package)
        .map_err(|error| DaemonError::Protocol(format!("executable: {error}")))?;

    // Signature policy: a configured release key makes a valid signature
    // mandatory; without a key, unsigned installs need the explicit dev
    // escape hatch (PLUGIN_ARCHITECTURE: dev mode allows local unsigned
    // plugins, production refuses them).
    match state.plugin_public_key.as_deref() {
        Some(key) => {
            aether_update::verify_manifest_signature(&manifest_value, key)
                .map_err(|error| DaemonError::Protocol(format!("signature: {error}")))?;
            aether_update::verify_package_files(package, &manifest_value)
                .map_err(|error| DaemonError::Protocol(format!("integrity: {error}")))?;
        }
        None => {
            let allowed = std::env::var("AETHER_ALLOW_UNSIGNED_PLUGINS")
                .map(|value| value.trim() == "1")
                .unwrap_or(false);
            if !allowed {
                return Err(DaemonError::Protocol(
                    "this daemon has no plugin signing key configured; unsigned plugins need                      AETHER_ALLOW_UNSIGNED_PLUGINS=1 (development only)"
                        .into(),
                ));
            }
        }
    }

    // Refuse to replace a plugin whose tasks are still running (same rule
    // as disabling), so the old binary is never pulled from under work.
    if plugin_is_busy(state, &manifest.id) {
        return Err(DaemonError::Protocol(format!(
            "plugin {} still has running tasks",
            manifest.id
        )));
    }

    let existed = state.manager.registry().get(&manifest.id).is_some();
    if existed {
        // Idle by the check above: retire the old process so the next spawn
        // picks up the freshly installed binary instead of the old one.
        state.manager.stop(&manifest.id);
    }

    // Stage, then swap: copy failure leaves the installed copy untouched.
    let target = state.plugins_dir.join(&manifest.id);
    let staging =
        state
            .plugins_dir
            .join(format!(".staging-{}-{}", manifest.id, std::process::id()));
    let retired = state.plugins_dir.join(format!(".retired-{}", manifest.id));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    copy_dir(package, &staging)?;
    // Keep exactly one previous version outside `plugins_dir` (a backup
    // directory inside the scan root would be discovered as a duplicate
    // plugin id). `rollback_plugin` installs it through this same verified
    // path, so a rollback cannot bypass signature/integrity checks.
    if target.is_dir() {
        let backup_root = state.data_dir.join("plugin-backups");
        fs::create_dir_all(&backup_root)?;
        let backup = backup_root.join(&manifest.id);
        let backup_staging =
            backup_root.join(format!(".tmp-{}-{}", manifest.id, std::process::id()));
        if backup_staging.exists() {
            fs::remove_dir_all(&backup_staging)?;
        }
        copy_dir(&target, &backup_staging)?;
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&backup_staging, &backup)?;
    }
    if let Err(error) = fs::rename(&target, &retired) {
        if target.exists() {
            fs::remove_dir_all(&staging)?;
            return Err(DaemonError::Io(error));
        }
    }
    if let Err(error) = fs::rename(&staging, &target) {
        // Roll the previous version back so a failed swap is not a delete.
        let _ = fs::rename(&retired, &target);
        let _ = fs::remove_dir_all(&staging);
        return Err(DaemonError::Io(error));
    }
    let _ = fs::remove_dir_all(&retired);

    rescan_registry(state)?;
    state.services.sync(state.manager.registry());
    Ok(json!({
        "plugin_id": manifest.id,
        "version": manifest.version,
        "replaced": existed,
    }))
}

/// Recursive copy for plugin packages; follows no symlinks (they would not
/// survive a signature check anyway) and preserves nothing but bytes.
fn copy_dir(from: &Path, to: &Path) -> Result<(), DaemonError> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &destination)?;
        }
        // Symlinks/sockets/fifos are skipped: the package must be plain files.
    }
    Ok(())
}
