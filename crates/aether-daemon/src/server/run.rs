use crate::server::*;

pub fn run(config: DaemonConfig) -> Result<(), DaemonError> {
    let data_dir = config.download_dir.join(".aether");
    fs::create_dir_all(&data_dir)?;
    // The endpoint is a socket file on Unix (its parent must exist) and a
    // pipe name on Windows (no filesystem presence at all).
    #[cfg(unix)]
    fs::create_dir_all(config.socket.parent().unwrap_or(Path::new(".")))?;
    // Single-instance guard. Unix keys it off the socket path; a Windows pipe
    // name has no directory, so the lock lives in the data dir and only
    // serializes daemons that share a download root (the pipe name itself is
    // per user, so a second daemon on the same user still gets rejected by
    // the connect probe below).
    #[cfg(unix)]
    let lock_path = config.socket.with_extension("lock");
    #[cfg(windows)]
    let lock_path = data_dir.join("daemon.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| DaemonError::AlreadyRunning)?;

    #[cfg(unix)]
    {
        if config.socket.exists() {
            if Conn::connect(&config.socket).is_ok() {
                return Err(DaemonError::AlreadyRunning);
            }
            let _ = fs::remove_file(&config.socket);
        }
    }
    // A live pipe refuses a second FILE_FLAG_FIRST_PIPE_INSTANCE below, so
    // the same probe is inherent on Windows.
    #[cfg(windows)]
    if Conn::connect(&config.socket).is_ok() {
        return Err(DaemonError::AlreadyRunning);
    }

    let listener = Listener::bind(&config.socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;
    }

    let registry = PluginRegistry::discover(std::slice::from_ref(&config.plugins_dir))?;
    let mut manager = PluginManager::new(registry, &data_dir, config.host_version.clone());
    let store = TaskStore::open(data_dir.join("aether.db"))?;
    // Plugins the user switched off last time stay off.
    for plugin_id in store.disabled_plugins()? {
        let _ = manager.registry_mut().set_enabled(&plugin_id, false);
    }

    // Persisted settings, with AETHER_PROXY winning over the file so ops and
    // tests can override a user's choice for one run.
    let settings_path = Settings::path(&config.download_dir);
    let mut settings = Settings::load(&settings_path);
    // RPC is useful out of the box (CLI, second device): seed a loopback
    // address and a token on first boot, then persist so the UI and the CLI
    // read the same values. Explicit empty strings are preserved — except for
    // the listener/token pair, which must never be unauthenticated.
    let seeded = settings.seed_rpc_defaults();
    let repaired = settings.ensure_rpc_auth();
    if seeded || repaired {
        settings.save(&settings_path)?;
    }
    if let Some(pac) = &config.pac_url {
        settings.proxy = Some(ProxySetting::Pac {
            url: Some(pac.clone()),
            script: None,
        });
    } else if let Some(spec) = &config.proxy {
        settings.proxy = Some(ProxySetting::parse_spec(spec).map_err(DaemonError::Protocol)?);
    }
    // Scheduler limits: the file wins, `AETHER_MAX_*` fills what it leaves
    // out, and the resolved values are written back so `get_settings` reports
    // the truth instead of `null`.
    let limits = settings.resolve_limits(Limits {
        max_concurrent: config.max_concurrent,
        max_speed_bps: config.max_speed_bps,
        retry_max: config.retry_max,
    });
    manager.set_proxy(init_proxy(settings.proxy.as_ref()));
    manager.set_network(settings.network());
    manager.set_allow_external_process(settings.plugin_external_process.unwrap_or(true));
    let update_status = match (&config.update_url, &config.update_public_key) {
        (Some(url), Some(key)) => {
            let status = Arc::new(Mutex::new(serde_json::json!({
                "status": "idle",
                "latest": null,
                "artifact": null,
                "error": null,
            })));
            spawn_update_loop(
                url.clone(),
                key.clone(),
                config.update_appimage.clone(),
                data_dir.clone(),
                settings_path.clone(),
                status.clone(),
                config.socket.clone(),
            );
            Some(status)
        }
        _ => None,
    };

    // Captured before `settings` moves into `State`; the accept loop shares
    // this handle so a rotation via set_settings applies live.
    let rpc_secret = Arc::new(Mutex::new(
        settings
            .rpc_secret
            .clone()
            .filter(|secret| !secret.trim().is_empty()),
    ));
    let (finalize_tx, finalize_rx) = mpsc::channel();
    let mut state = State {
        manager,
        store,
        download_dir: config.download_dir.clone(),
        plugins_dir: config.plugins_dir.clone(),
        data_dir: data_dir.clone(),
        services: ServiceSupervisor::new(
            config.socket.clone(),
            data_dir.clone(),
            config.host_version.clone(),
        ),
        rpc_base: None,
        details: HashMap::new(),
        last_progress: HashMap::new(),
        active: HashSet::new(),
        update: update_status,
        plugin_progress: Arc::new(Mutex::new(json!({"running": false}))),
        subscribers: Vec::new(),
        dropped_events: 0,
        max_concurrent: limits.max_concurrent,
        max_speed_bps: limits.max_speed_bps,
        retry_max: limits.retry_max,
        retry_base_secs: config.retry_base_secs,
        live: HashSet::new(),
        canceling: HashSet::new(),
        schedule_paused: HashSet::new(),
        retries: HashMap::new(),
        pac: settings.proxy.as_ref().and_then(ProxySetting::resolver),
        settings,
        settings_path,
        system_proxy: SystemProxyCache::default(),
        system_pac: None,
        rpc_secret,
        host_version: config.host_version.clone(),
        plugin_public_key: config.update_public_key.clone(),
        update_url: config.update_url.clone(),
        update_appimage: config.update_appimage.clone(),
        finalize_tx,
        finalize_rx,
        finalize_pending: 0,
        unpersisted: Vec::new(),
    };

    recover_tasks(&mut state);
    restore_retries(&mut state);
    cleanup_stale_staging(&state);
    let (command_tx, command_rx) = mpsc::channel::<Command>();

    start_rpc(&mut state, &config, &command_tx)?;
    let accept_tx = command_tx.clone();
    let accept_thread = thread::spawn(move || accept_loop(listener, accept_tx));
    eprintln!("aether-daemon: listening on {}", config.socket.display());
    event_loop(&mut state, command_rx, accept_thread)?;

    #[cfg(unix)]
    let _ = fs::remove_file(&config.socket);
    Ok(())
}
