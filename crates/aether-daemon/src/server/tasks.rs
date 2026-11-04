use crate::server::*;
use crate::settings::CONNECTIONS_MAX;

/// `connections` only makes sense for HTTP(S) range downloads, but a task may
/// still carry it; reject values the engine cannot honour at add time so the
/// CLI reports a protocol error instead of silently ignoring the setting.
pub(crate) fn validate_task_connections(settings: &Value) -> Result<(), DaemonError> {
    let Some(value) = settings.get("connections") else {
        return Ok(());
    };
    let valid = value
        .as_u64()
        .is_some_and(|count| (1..=u64::from(CONNECTIONS_MAX)).contains(&count));
    if valid {
        Ok(())
    } else {
        Err(DaemonError::Protocol(format!(
            "connections must be within 1..={CONNECTIONS_MAX}, got {value}"
        )))
    }
}

pub(crate) fn add_task(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let url = request
        .url
        .ok_or_else(|| DaemonError::Protocol("add needs url".into()))?;
    let task_proxy = resolve_task_proxy(state, &url, true);
    // A forced plugin is probed directly: its declared protocol does not
    // describe the URL (aria2 publishes inline torrents over loopback http).
    let outcomes = match request.plugin.as_deref() {
        Some(id) => state
            .manager
            .probe_plugin(id, &url, task_proxy.clone())
            .into_iter()
            .collect(),
        None => state.manager.probe_with_proxy(&url, task_proxy.clone()),
    };
    let chosen = match request.plugin.as_deref() {
        Some(id) => outcomes
            .iter()
            .find(|outcome| outcome.plugin_id == id)
            .and_then(|outcome| outcome.result.as_ref())
            .map(|result| (id.to_string(), result.clone()))
            .ok_or_else(|| DaemonError::Protocol(format!("plugin {id} did not answer probe")))?,
        None => outcomes
            .iter()
            .filter_map(|outcome| {
                outcome
                    .result
                    .as_ref()
                    .filter(|result| result.supported)
                    .map(|result| (outcome.plugin_id.clone(), result.clone()))
            })
            .max_by_key(|(_, result)| result.priority)
            .ok_or_else(|| {
                // Without the per-plugin reasons this error is undiagnosable
                // ("is my URL malformed, or did every plugin error out?").
                let reasons = outcomes
                    .iter()
                    .map(|outcome| {
                        let reason = outcome
                            .error
                            .clone()
                            .or_else(|| outcome.result.as_ref().and_then(|r| r.reason.clone()))
                            .unwrap_or_else(|| "no reason reported".into());
                        format!("{}: {reason}", outcome.plugin_id)
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                DaemonError::Protocol(format!("no plugin supports this URL ({reasons})"))
            })?,
    };

    let (plugin_id, probe) = chosen;
    if !probe.supported {
        return Err(DaemonError::Protocol(format!(
            "{plugin_id} does not support this URL"
        )));
    }

    let task_id = format!(
        "task-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    // Staging lives under the daemon-owned `.aether` tree; finalized files go
    // straight into the destination the client asked for.
    let target_dir = resolve_target_dir(state, request.dir.as_deref())?;
    let final_dir = target_dir.clone();
    fs::create_dir_all(&final_dir)?;
    let staging = prepare_staging(state, &task_id, &final_dir)?;

    // Only non-default destinations are persisted; `None` keeps following the
    // daemon's `--download-dir` if the user moves it.
    let stored_dir =
        (target_dir != state.download_dir).then(|| target_dir.to_string_lossy().into_owned());
    // Fail fast instead of dying halfway through: the probe usually knows the
    // size, and downloads already running into the same directory will keep
    // consuming space, so count their remaining bytes as reserved.
    if let Some(size) = probe.metadata.as_ref().and_then(|metadata| metadata.size) {
        let reserved = reserved_in(state, &target_dir);
        match ensure_space(&target_dir, size.saturating_add(reserved)) {
            Ok(_) => {}
            Err(error) if error.is_shortage() => {
                return Err(DaemonError::Protocol(format!(
                    "{error} (task needs {}, other downloads reserve {})",
                    human_bytes(size),
                    human_bytes(reserved)
                )))
            }
            // statvfs can fail on exotic mounts; not a reason to block.
            Err(error) => eprintln!("aether-daemon: {error}; continuing"),
        }
        // Staging always lives under the daemon's download root, which may be
        // a different filesystem than a "save as" target. Check both.
        let staging_root = staging
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| state.download_dir.join(".aether"));
        let staging_reserved = reserved_staging(state);
        match ensure_space(&staging_root, size.saturating_add(staging_reserved)) {
            Ok(_) => {}
            Err(error) if error.is_shortage() => {
                return Err(DaemonError::Protocol(format!(
                    "{error} (staging needs {}, other downloads reserve {})",
                    human_bytes(size),
                    human_bytes(staging_reserved)
                )))
            }
            Err(error) => eprintln!("aether-daemon: {error}; continuing"),
        }
    }

    // -100..=100 keeps a single task from starving the queue forever.
    let priority = request.priority.unwrap_or(0).clamp(-100, 100);
    // aria2 `out` (and any native caller that asks) overrides the probe's
    // suggested name; the http plugin already prefers task.filename.
    let filename = request
        .settings
        .get("filename")
        .and_then(Value::as_str)
        .and_then(aether_plugin_sdk::fsutil::sanitize_filename)
        .or_else(|| probe.suggested_filename.clone());
    let mut settings = request.settings;
    // Plugin settings are a default layer: a task that names the same key wins.
    if let Some(defaults) = state.store.plugin_settings(&plugin_id)? {
        aether_host::plugin_settings::apply_defaults(&mut settings, &defaults);
    }
    let checksum = parse_checksum(&settings)?;
    validate_task_connections(&settings)?;
    // The daemon-wide ffmpeg path is a default, not an override: a task that
    // names its own binary (or none at all) keeps/loses to its own choice,
    // and the value is persisted so the record shows what will actually run.
    if let Some(ffmpeg) = state.settings.ffmpeg_path.as_deref() {
        if let Some(object) = settings.as_object_mut() {
            object
                .entry("ffmpeg_path")
                .or_insert_with(|| serde_json::Value::String(ffmpeg.to_string()));
        }
    }
    state.store.create_task(&NewTask {
        id: task_id.clone(),
        url: url.clone(),
        plugin_id: plugin_id.clone(),
        filename: filename.clone(),
        settings: settings.clone(),
        state: TaskState::Queued,
        priority,
        target_dir: stored_dir,
    })?;

    let task = TaskSpec {
        task_id: task_id.clone(),
        url,
        filename,
        settings,
        plugin_state: None,
        expected_size: probe.metadata.as_ref().and_then(|metadata| metadata.size),
        checksum,
        proxy: task_proxy,
    };

    let started = match try_launch(state, &plugin_id, task, staging, final_dir.clone()) {
        Ok(started) => started,
        Err(error) => {
            let message = error.to_string();
            state
                .store
                .fail_task(&task_id, "internal", &message, true)?;
            return Err(error);
        }
    };
    if let Ok(Some(task)) = state.store.task(&task_id) {
        broadcast(state, json!({"event": "task_added", "task": task}));
    }
    if !started {
        let _ = state.store.append_log(
            &task_id,
            "info",
            &format!(
                "queued: {} task(s) already running (max {})",
                state.active.len(),
                state.max_concurrent
            ),
            None,
        );
    }

    Ok(json!({
        "task_id": task_id,
        "plugin_id": plugin_id,
        "queued": !started,
        "dir": final_dir.to_string_lossy(),
        "priority": priority,
    }))
}

/// Parse a task-level SHA-256 from settings. Accepts both aria2's
/// `sha-256=<hex>` and the JSON object form `{algorithm, value}`.
pub(crate) fn parse_checksum(settings: &Value) -> Result<Option<Checksum>, DaemonError> {
    let Some(value) = settings.get("checksum") else {
        return Ok(None);
    };
    let (algorithm, value) = match value {
        Value::String(text) => {
            let (algorithm, value) = text
                .split_once(':')
                .or_else(|| text.split_once('='))
                .ok_or_else(|| {
                    DaemonError::Protocol("checksum must be sha-256:<hex> or sha-256=<hex>".into())
                })?;
            (algorithm.to_string(), value.to_string())
        }
        Value::Object(map) => {
            let algorithm = map
                .get("algorithm")
                .and_then(Value::as_str)
                .ok_or_else(|| DaemonError::Protocol("checksum.algorithm is required".into()))?;
            let value = map
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| DaemonError::Protocol("checksum.value is required".into()))?;
            (algorithm.to_string(), value.to_string())
        }
        _ => {
            return Err(DaemonError::Protocol(
                "checksum must be a string or object".into(),
            ))
        }
    };
    let algorithm = algorithm.trim().replace('_', "-").to_ascii_lowercase();
    if algorithm != "sha256" && algorithm != "sha-256" {
        return Err(DaemonError::Protocol(
            "only SHA-256 checksums are supported".into(),
        ));
    }
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DaemonError::Protocol(
            "SHA-256 checksum must be 64 hex characters".into(),
        ));
    }
    Ok(Some(Checksum {
        algorithm: ChecksumAlgorithm::Sha256,
        value: value.to_ascii_lowercase(),
    }))
}

pub(crate) fn task_id(request: &Request) -> Result<String, DaemonError> {
    request
        .task_id
        .clone()
        .ok_or_else(|| DaemonError::Protocol("task_id is required".into()))
}
