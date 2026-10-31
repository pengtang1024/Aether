//! aria2 JSON-RPC compatibility service.
//!
//! The plugin owns no listening socket. The daemon's TCP listener identifies
//! HTTP requests, validates the aria2 token, and forwards the call over the
//! plugin IPC channel. This process translates the call into native daemon
//! requests over the trusted local endpoint it receives via `ServiceInit`.

mod bridge;
mod client;
mod host;
mod mapping;
mod query;

use std::time::{SystemTime, UNIX_EPOCH};

use aether_plugin_sdk::{
    run_stdio, Capability, HostCommand, Plugin, PluginContext, PluginEvent, SdkError,
};
use serde_json::{json, Value};

use client::DaemonClient;
use host::TorrentHost;

const PLUGIN_ID: &str = "aria2";
const PLUGIN_VERSION: &str = "0.2.2";

struct Aria2Plugin {
    socket: Option<String>,
    client: Option<DaemonClient>,
    /// Lazily started on the first addTorrent/addMetalink call.
    host: Option<TorrentHost>,
    session: String,
}

impl Aria2Plugin {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            socket: None,
            client: None,
            host: None,
            session: format!("aether-{}-{nanos}", std::process::id()),
        }
    }

    fn torrent_host(&mut self) -> Result<TorrentHost, String> {
        if self.host.is_none() {
            self.host = Some(TorrentHost::start()?);
        }
        self.host
            .clone()
            .ok_or_else(|| "torrent source unavailable".to_string())
    }

    fn bridge(&mut self, request: &Value) -> Value {
        let Some(socket) = self.socket.clone() else {
            return bridge_error("the daemon has not provided its local endpoint yet");
        };
        // Only start the loopback source when a call actually carries inline
        // torrent/metalink bytes; the common polling path opens no socket.
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let needs_host = matches!(
            method,
            "aria2.addTorrent" | "aria2.addMetalink" | "system.multicall"
        );
        let host = if needs_host {
            match self.torrent_host() {
                Ok(host) => Some(host),
                Err(message) => return bridge_error(&message),
            }
        } else {
            None
        };
        if self.client.is_none() {
            match DaemonClient::connect(&socket) {
                Ok(client) => self.client = Some(client),
                Err(message) => return bridge_error(&message),
            }
        }
        let session = self.session.clone();
        let reply = match self.client.as_mut() {
            Some(client) => bridge::handle(client, host.as_ref(), &session, request),
            None => bridge_error("daemon client unavailable"),
        };
        // A transport failure should not poison the next request: drop the
        // connection so the following call reconnects.
        if reply.get("error").is_some() {
            self.client = None;
        }
        reply
    }
}

fn bridge_error(message: &str) -> Value {
    json!({ "error": { "code": 1, "message": message } })
}

impl Plugin for Aria2Plugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    fn on_command(
        &mut self,
        ctx: &mut PluginContext,
        command: HostCommand,
    ) -> Result<(), SdkError> {
        match command {
            HostCommand::ServiceInit { socket } => {
                self.socket = Some(socket);
                self.client = None;
            }
            HostCommand::BridgeCall {
                request_id,
                payload,
            } => {
                let reply = self.bridge(&payload);
                ctx.send(&PluginEvent::BridgeResult {
                    request_id,
                    payload: reply,
                })?;
            }
            // The daemon filters service plugins out of probe/start routing,
            // but answer honestly if an older host still sends one.
            HostCommand::Probe {
                request_id, input, ..
            } => {
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: false,
                    protocol: None,
                    priority: 0,
                    metadata: None,
                    suggested_filename: None,
                    reason: Some(format!(
                        "aria2 compatibility service, not a downloader: {input}"
                    )),
                })?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("aether-plugin-aria2 {PLUGIN_VERSION}");
        return;
    }
    if let Err(error) = run_stdio(Aria2Plugin::new()) {
        eprintln!("aether-plugin-aria2: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin_sdk::ipc::write_frame;
    use aether_plugin_sdk::plugin::run;
    use aether_plugin_sdk::{Limits, LogLevel};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn answers_bridge_calls_without_a_daemon_as_errors() {
        let mut input = Vec::new();
        write_frame(
            &mut input,
            &HostCommand::Init {
                host_version: "0.2.2".into(),
                data_dir: "/tmp/aether".into(),
                log_level: LogLevel::Info,
                proxy: None,
                limits: Limits::default(),
                network: None,
            },
        )
        .unwrap();
        write_frame(
            &mut input,
            &HostCommand::ServiceInit {
                socket: "/tmp/aether-no-such.sock".into(),
            },
        )
        .unwrap();
        write_frame(
            &mut input,
            &HostCommand::BridgeCall {
                request_id: 7,
                payload: json!({"method": "aria2.getVersion", "params": []}),
            },
        )
        .unwrap();
        write_frame(&mut input, &HostCommand::Shutdown).unwrap();

        let output = Arc::new(Mutex::new(Vec::new()));
        run(
            Aria2Plugin::new(),
            Cursor::new(input),
            Box::new(SharedWriter(output.clone())),
        )
        .unwrap();

        let bytes = output.lock().unwrap().clone();
        let mut cursor = Cursor::new(bytes);
        let hello: PluginEvent = aether_plugin_sdk::ipc::read_frame(&mut cursor)
            .unwrap()
            .unwrap();
        assert!(matches!(hello, PluginEvent::Hello { ref plugin_id, .. } if plugin_id == "aria2"));
        let _ready: PluginEvent = aether_plugin_sdk::ipc::read_frame(&mut cursor)
            .unwrap()
            .unwrap();
        let reply: PluginEvent = aether_plugin_sdk::ipc::read_frame(&mut cursor)
            .unwrap()
            .unwrap();
        match reply {
            PluginEvent::BridgeResult {
                request_id,
                payload,
            } => {
                assert_eq!(request_id, 7);
                assert!(payload.get("error").is_some(), "{payload}");
            }
            other => panic!("expected bridge result, got {other:?}"),
        }
    }
}
