use crate::server::*;

/// RPC over TCP: every connection must open with an `auth` request carrying
/// the token before it may use any other method. The secret is read live, so
/// a `set_settings` rotation applies to new connections immediately.
/// Keep-alive means a connection can sit idle for a minute; cap the number
/// of compat threads so a stuck client cannot exhaust the process.
const MAX_RPC_CONNECTIONS: usize = 128;

struct RpcConnectionGuard(Arc<AtomicUsize>);

impl Drop for RpcConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn rpc_accept_loop(
    mut listener: Listener,
    commands: Sender<Command>,
    secret: std::sync::Arc<Mutex<Option<String>>>,
    bridge: Arc<ServiceBridge>,
    blob_dir: PathBuf,
) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok(conn) = listener.accept() else { break };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_RPC_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            conn.shutdown();
            continue;
        }
        let guard = RpcConnectionGuard(active.clone());
        let commands = commands.clone();
        let secret = secret.clone();
        let bridge = bridge.clone();
        let blob_dir = blob_dir.clone();
        thread::spawn(move || {
            let _guard = guard;
            rpc_client_loop(conn, commands, secret, bridge, blob_dir)
        });
    }
}

fn rpc_client_loop(
    stream: Conn,
    commands: Sender<Command>,
    secret: std::sync::Arc<Mutex<Option<String>>>,
    bridge: Arc<ServiceBridge>,
    blob_dir: PathBuf,
) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        writer.shutdown();
        return;
    }
    // One port, two protocols: HTTP goes to the aria2 compatibility bridge,
    // the native JSON-lines protocol keeps its auth handshake below.
    if crate::aria2::is_http_request(&line) {
        crate::aria2::serve_http(
            reader,
            &mut writer,
            &line,
            Some(bridge.as_ref()),
            &secret,
            &commands,
            &blob_dir,
        );
        writer.shutdown();
        return;
    }
    let request = serde_json::from_str::<Request>(&line);
    let authed = match request {
        Ok(request) if request.method == "auth" => {
            let token = request
                .settings
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or("");
            let expected = secret.lock().unwrap();
            let ok = constant_time_eq(
                token.as_bytes(),
                expected.as_deref().unwrap_or("").as_bytes(),
            );
            if write_response(
                &mut writer,
                &if ok {
                    Response::ok(request.id, json!({"ok": true}))
                } else {
                    Response::err(request.id, "invalid rpc token")
                },
            )
            .is_err()
            {
                writer.shutdown();
                return;
            }
            ok
        }
        other => {
            let id = other.as_ref().map_or(0, |request| request.id);
            let _ = write_response(
                &mut writer,
                &Response::err(id, "rpc needs an auth request first"),
            );
            false
        }
    };
    if authed {
        serve(&mut reader, &mut writer, commands);
    }
    writer.shutdown();
}

/// Constant-time byte comparison for secrets (no early exit on the first
/// differing byte).
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

/// Bind the optional TCP RPC listener and seed the loopback defaults.
pub(crate) fn start_rpc(
    state: &mut State,
    config: &DaemonConfig,
    command_tx: &Sender<Command>,
) -> Result<(), DaemonError> {
    // RPC over TCP: the CLI/env override wins over the persisted setting.
    // Anything wider than loopback must carry a token — checked here as well
    // because the boot path bypasses `set_settings` and its validation.
    let rpc_listen = config
        .rpc_listen
        .clone()
        .filter(|addr| !addr.trim().is_empty())
        .or_else(|| {
            state
                .settings
                .rpc_listen
                .clone()
                .filter(|addr| !addr.trim().is_empty())
        });
    if let Some(addr) = rpc_listen.as_deref() {
        // The effective pair is checked here too: the CLI/env override is
        // not part of `set_settings`, so its validation would miss it.
        let parsed: std::net::SocketAddr = addr.trim().parse().map_err(|error| {
            DaemonError::Protocol(format!("rpc_listen must be ip:port: {error}"))
        })?;
        let has_secret = state
            .rpc_secret
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|secret| !secret.is_empty());
        if !parsed.ip().is_loopback() && !has_secret {
            return Err(DaemonError::Protocol(
                "rpc_listen on a non-loopback address requires rpc_secret".into(),
            ));
        }
        // The default listener is a convenience, not a requirement: a busy
        // port must not keep downloads from starting. Explicit addresses
        // still fail loudly so a misconfiguration is visible.
        let listener = match Listener::bind_tcp(addr.trim()) {
            Ok(listener) => Some(listener),
            Err(error) if addr.trim() == RPC_LISTEN_DEFAULT => {
                eprintln!(
                    "aether-daemon: cannot listen on default RPC address {addr}: {error}; \
                     continuing without TCP RPC"
                );
                None
            }
            Err(error) => {
                return Err(DaemonError::Protocol(format!(
                    "cannot listen on {addr}: {error}"
                )))
            }
        };
        if let Some(listener) = listener {
            let blob_listen = if parsed.ip().is_unspecified() {
                format!("127.0.0.1:{}", parsed.port())
            } else {
                addr.trim().to_string()
            };
            state.rpc_base = Some(blob_listen.clone());
            state.services.set_listen(Some(blob_listen));
            state.services.sync(state.manager.registry());
            if has_secret {
                eprintln!("aether-daemon: rpc listening on {addr} (token required)");
            } else {
                eprintln!("aether-daemon: rpc listening on {addr} (no token; loopback only)");
            }
            let rpc_tx = command_tx.clone();
            let rpc_secret = state.rpc_secret.clone();
            let bridge = Arc::new(ServiceBridge::new(state.services.clone(), "aria2"));
            let blob_dir = state.data_dir.join("aria2-blobs");
            thread::spawn(move || rpc_accept_loop(listener, rpc_tx, rpc_secret, bridge, blob_dir));
        }
    }
    Ok(())
}
