//! Plugin-side runner: handshake plus a typed command loop.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::SdkError;
use crate::ipc;
use crate::protocol::{Capability, HostCommand, Init, LogLevel, PluginEvent, API_VERSION};

/// Cloneable event sender. Workers can hold one while the plugin keeps the
/// main context, so progress does not need to flow back through the command
/// loop.
#[derive(Clone)]
pub struct PluginSender {
    output: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl PluginSender {
    pub fn new(output: Box<dyn Write + Send>) -> Self {
        Self {
            output: Arc::new(Mutex::new(output)),
        }
    }

    pub fn send(&self, event: &PluginEvent) -> Result<(), SdkError> {
        let mut output = self
            .output
            .lock()
            .map_err(|_| SdkError::Protocol("plugin output lock poisoned".into()))?;
        ipc::write_frame(&mut *output, event).map_err(SdkError::from)
    }
}

/// Mutable context passed to plugin handlers.
pub struct PluginContext {
    sender: PluginSender,
    init: Option<Init>,
}

impl PluginContext {
    pub fn new(output: Box<dyn Write + Send>) -> Self {
        Self {
            sender: PluginSender::new(output),
            init: None,
        }
    }

    /// Host configuration received during the handshake.
    pub fn init(&self) -> Option<&Init> {
        self.init.as_ref()
    }

    /// Cloneable sender for background workers.
    pub fn sender(&self) -> PluginSender {
        self.sender.clone()
    }

    pub fn send(&mut self, event: &PluginEvent) -> Result<(), SdkError> {
        self.sender.send(event)
    }
}

/// Implement this trait for a first-party plugin and call [`run_stdio`].
pub trait Plugin: Send {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn capabilities(&self) -> &[Capability];

    /// Called after `Init`, before `Ready` is sent. Use it to open resources.
    fn on_init(&mut self, _ctx: &mut PluginContext, _init: &Init) -> Result<(), SdkError> {
        Ok(())
    }

    /// Handle a command other than `Init`, `Ping` and `Shutdown`.
    fn on_command(&mut self, ctx: &mut PluginContext, command: HostCommand)
        -> Result<(), SdkError>;

    /// Called on `Shutdown` or when the host closes stdin.
    fn on_shutdown(&mut self, _ctx: &mut PluginContext) -> Result<(), SdkError> {
        Ok(())
    }
}

/// Run a plugin on stdin/stdout.
pub fn run_stdio<P: Plugin>(plugin: P) -> Result<(), SdkError> {
    run(plugin, std::io::stdin(), Box::new(std::io::stdout()))
}

/// Run a plugin with explicit streams (used by tests and embedding).
pub fn run<P: Plugin, R: Read>(
    mut plugin: P,
    mut input: R,
    output: Box<dyn Write + Send>,
) -> Result<(), SdkError> {
    let mut ctx = PluginContext::new(output);

    ctx.send(&PluginEvent::Hello {
        api: API_VERSION,
        plugin_id: plugin.id().to_string(),
        version: plugin.version().to_string(),
        capabilities: plugin.capabilities().to_vec(),
    })?;

    let init = match ipc::read_frame::<_, HostCommand>(&mut input)? {
        Some(HostCommand::Init {
            host_version,
            data_dir,
            log_level,
            proxy,
            limits,
        }) => Init {
            host_version,
            data_dir,
            log_level,
            proxy,
            limits,
        },
        Some(other) => return Err(SdkError::Protocol(format!("expected init, got {other:?}"))),
        None => return Err(SdkError::Protocol("host closed stdin before init".into())),
    };

    plugin.on_init(&mut ctx, &init)?;
    ctx.init = Some(init);
    ctx.send(&PluginEvent::Ready { api: API_VERSION })?;

    loop {
        let command = match ipc::read_frame::<_, HostCommand>(&mut input)? {
            Some(command) => command,
            None => break,
        };

        match command {
            HostCommand::Ping { request_id } => {
                ctx.send(&PluginEvent::Pong {
                    request_id,
                    unix_ms: unix_ms(),
                })?;
            }
            HostCommand::Shutdown => {
                plugin.on_shutdown(&mut ctx)?;
                break;
            }
            HostCommand::Init { .. } => {
                return Err(SdkError::Protocol("duplicate init".into()));
            }
            other => plugin.on_command(&mut ctx, other)?,
        }
    }

    Ok(())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Convenience for plugins that only need to acknowledge `Init`.
pub fn log_event(level: LogLevel, message: impl Into<String>) -> PluginEvent {
    PluginEvent::Log {
        level,
        message: message.into(),
        fields: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct TestPlugin;

    impl Plugin for TestPlugin {
        fn id(&self) -> &str {
            "test"
        }

        fn version(&self) -> &str {
            "0.1.0"
        }

        fn capabilities(&self) -> &[Capability] {
            &[Capability::Network]
        }

        fn on_command(
            &mut self,
            _ctx: &mut PluginContext,
            _command: HostCommand,
        ) -> Result<(), SdkError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn performs_handshake_and_exits_on_shutdown() {
        let mut input = Vec::new();
        ipc::write_frame(
            &mut input,
            &HostCommand::Init {
                host_version: "0.2.0".into(),
                data_dir: "/tmp/aether".into(),
                log_level: LogLevel::Info,
                proxy: None,
                limits: Default::default(),
            },
        )
        .unwrap();
        ipc::write_frame(&mut input, &HostCommand::Shutdown).unwrap();

        let output = Arc::new(Mutex::new(Vec::new()));
        run(
            TestPlugin,
            Cursor::new(input),
            Box::new(SharedWriter(output.clone())),
        )
        .unwrap();

        let bytes = output.lock().unwrap().clone();
        let mut cursor = Cursor::new(bytes);
        let hello: PluginEvent = ipc::read_frame(&mut cursor).unwrap().unwrap();
        let ready: PluginEvent = ipc::read_frame(&mut cursor).unwrap().unwrap();

        assert!(matches!(
            hello,
            PluginEvent::Hello { api, ref plugin_id, .. }
                if api == API_VERSION && plugin_id == "test"
        ));
        assert_eq!(ready, PluginEvent::Ready { api: API_VERSION });
    }
}
