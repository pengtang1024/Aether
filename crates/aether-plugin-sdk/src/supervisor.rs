//! Host-side supervisor for one plugin process.

use std::collections::VecDeque;
use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::SdkError;
use crate::ipc::{self, IpcError};
use crate::manifest::PluginManifest;
use crate::protocol::{
    Budget, Capability, HostCommand, Init, PluginEvent, ProbeResult, ProxyConfig, TaskPaths,
    TaskSpec, API_VERSION,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// One running plugin process.
///
/// The process is killed on drop if it is still running. Use [`shutdown`] for a
/// graceful stop.
///
/// [`shutdown`]: PluginProcess::shutdown
pub struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Result<PluginEvent, SdkError>>,
    pending: VecDeque<PluginEvent>,
    reader: Option<JoinHandle<()>>,
    manifest: PluginManifest,
    next_request_id: u64,
}

impl PluginProcess {
    /// Spawn `executable`, perform the Hello/Init/Ready handshake and verify
    /// that the plugin identity matches its manifest.
    pub fn spawn(
        manifest: PluginManifest,
        executable: &Path,
        init: Init,
    ) -> Result<Self, SdkError> {
        manifest.validate_identity()?;

        if !executable.is_file() {
            return Err(SdkError::Manifest(
                crate::manifest::ManifestError::MissingExecutable(executable.display().to_string()),
            ));
        }

        let mut child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SdkError::Protocol("plugin stdin is not available".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SdkError::Protocol("plugin stdout is not available".into()))?;

        let (sender, events) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                match ipc::read_frame::<_, PluginEvent>(&mut stdout) {
                    Ok(Some(event)) => {
                        if sender.send(Ok(event)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err(SdkError::Protocol("plugin closed stdout".into())));
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(SdkError::Ipc(error)));
                        break;
                    }
                }
            }
        });

        let mut process = Self {
            child,
            stdin,
            events,
            pending: VecDeque::new(),
            reader: Some(reader),
            manifest,
            next_request_id: 1,
        };

        if let Err(error) = process.handshake(init) {
            process.kill();
            return Err(error);
        }

        Ok(process)
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// True when the child has already exited (and has been reaped). The
    /// Host uses this to replace a crashed plugin instead of reusing a dead
    /// stdin pipe forever.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Send a command to the plugin.
    pub fn send(&mut self, command: &HostCommand) -> Result<(), SdkError> {
        ipc::write_frame(&mut self.stdin, command).map_err(|error| match error {
            IpcError::Io(io) if io.kind() == std::io::ErrorKind::BrokenPipe => {
                SdkError::Exited("stdin closed".into())
            }
            other => SdkError::Ipc(other),
        })
    }

    /// Wait for the next event (including logs/heartbeats).
    pub fn recv_event(&mut self, timeout: Duration) -> Result<PluginEvent, SdkError> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(event);
        }
        self.recv_next(timeout)
    }

    fn recv_next(&mut self, timeout: Duration) -> Result<PluginEvent, SdkError> {
        match self.events.recv_timeout(timeout) {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Err(SdkError::Timeout("plugin event")),
            Err(RecvTimeoutError::Disconnected) => {
                let status = self
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|status| status.to_string())
                    .unwrap_or_else(|| "unknown status".into());
                Err(SdkError::Exited(status))
            }
        }
    }

    /// Ask the plugin whether it supports `input`.
    pub fn probe(&mut self, input: &str) -> Result<ProbeResult, SdkError> {
        self.probe_with_proxy(input, None)
    }

    /// Probe with the Host-resolved proxy for this input (PAC).
    pub fn probe_with_proxy(
        &mut self,
        input: &str,
        proxy: Option<ProxyConfig>,
    ) -> Result<ProbeResult, SdkError> {
        let request_id = self.next_request_id();
        self.send(&HostCommand::Probe {
            request_id,
            input: input.to_string(),
            proxy,
        })?;

        let deadline = Instant::now() + PROBE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SdkError::Timeout("probe result"));
            }

            match self.recv_next(remaining)? {
                PluginEvent::ProbeResult {
                    request_id: id,
                    supported,
                    protocol,
                    priority,
                    metadata,
                    suggested_filename,
                    reason,
                } if id == request_id => {
                    return Ok(ProbeResult {
                        request_id,
                        supported,
                        protocol,
                        priority,
                        metadata,
                        suggested_filename,
                        reason,
                    });
                }
                other => self.pending.push_back(other),
            }
        }
    }

    /// Send `Start`; progress/completion arrive as events.
    pub fn start_task(
        &mut self,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    ) -> Result<u64, SdkError> {
        let request_id = self.next_request_id();
        self.send(&HostCommand::Start {
            request_id,
            task,
            paths,
            budget,
        })?;
        Ok(request_id)
    }

    pub fn pause(&mut self, task_id: &str) -> Result<(), SdkError> {
        self.send(&HostCommand::Pause {
            task_id: task_id.to_string(),
        })
    }

    pub fn resume(&mut self, task_id: &str) -> Result<(), SdkError> {
        self.send(&HostCommand::Resume {
            task_id: task_id.to_string(),
        })
    }

    pub fn cancel(&mut self, task_id: &str) -> Result<(), SdkError> {
        self.send(&HostCommand::Cancel {
            task_id: task_id.to_string(),
        })
    }

    pub fn set_budget(&mut self, task_id: &str, budget: Budget) -> Result<(), SdkError> {
        self.send(&HostCommand::SetBudget {
            task_id: task_id.to_string(),
            budget,
        })
    }

    /// One request/response round-trip with a service plugin. The timeout is
    /// generous because the bridge may itself call back into the daemon.
    pub fn bridge_call(
        &mut self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, SdkError> {
        let request_id = self.next_request_id();
        self.send(&HostCommand::BridgeCall {
            request_id,
            payload,
        })?;

        let deadline = Instant::now() + BRIDGE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SdkError::Timeout("bridge result"));
            }
            match self.recv_next(remaining)? {
                PluginEvent::BridgeResult {
                    request_id: id,
                    payload,
                } if id == request_id => return Ok(payload),
                other => self.pending.push_back(other),
            }
        }
    }

    /// Graceful shutdown: send `Shutdown`, wait up to two seconds, then kill.
    pub fn shutdown(mut self) -> Result<(), SdkError> {
        self.stop()
    }

    /// In-place variant of [`shutdown`](Self::shutdown), for handles kept
    /// behind a mutex (the aria2 bridge outlives individual calls).
    pub fn stop(&mut self) -> Result<(), SdkError> {
        let _ = self.send(&HostCommand::Shutdown);

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match self.child.try_wait()? {
                Some(_) => break,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                None => thread::sleep(Duration::from_millis(20)),
            }
        }

        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }

        Ok(())
    }

    fn handshake(&mut self, init: Init) -> Result<(), SdkError> {
        match self.recv_event(HANDSHAKE_TIMEOUT)? {
            PluginEvent::Hello {
                api,
                plugin_id,
                version,
                capabilities,
            } => {
                if api != API_VERSION || api != self.manifest.api {
                    return Err(SdkError::Protocol(format!(
                        "api mismatch: plugin {api}, host {API_VERSION}, manifest {}",
                        self.manifest.api
                    )));
                }
                if plugin_id != self.manifest.id {
                    return Err(SdkError::Protocol(format!(
                        "plugin id mismatch: expected {}, got {plugin_id}",
                        self.manifest.id
                    )));
                }
                if version != self.manifest.version {
                    return Err(SdkError::Protocol(format!(
                        "plugin version mismatch: expected {}, got {version}",
                        self.manifest.version
                    )));
                }
                verify_capabilities(&self.manifest, &capabilities)?;
            }
            other => return Err(SdkError::Protocol(format!("expected hello, got {other:?}"))),
        }

        self.send(&HostCommand::Init {
            host_version: init.host_version,
            data_dir: init.data_dir,
            log_level: init.log_level,
            proxy: init.proxy,
            limits: init.limits,
            network: init.network,
        })?;

        match self.recv_event(HANDSHAKE_TIMEOUT)? {
            PluginEvent::Ready { api } if api == API_VERSION => Ok(()),
            other => Err(SdkError::Protocol(format!("expected ready, got {other:?}"))),
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        id
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The manifest is the trust root: a plugin may not claim capabilities at
/// runtime that it did not declare, and a manifest capability that the binary
/// silently drops is just as wrong (the manifest would over-promise to users).
pub fn verify_capabilities(
    manifest: &PluginManifest,
    declared: &[Capability],
) -> Result<(), SdkError> {
    if let Some(extra) = declared
        .iter()
        .find(|capability| !manifest.capabilities.contains(capability))
    {
        return Err(SdkError::Protocol(format!(
            "plugin claims capability {extra} that its manifest does not declare"
        )));
    }
    if let Some(missing) = manifest
        .capabilities
        .iter()
        .find(|capability| !declared.contains(capability))
    {
        return Err(SdkError::Protocol(format!(
            "plugin does not report manifest capability {missing}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(capabilities: &[Capability]) -> PluginManifest {
        serde_json::from_value(serde_json::json!({
            "id": "fake",
            "name": "Fake",
            "version": "0.1.0",
            "api": API_VERSION,
            "protocols": ["http"],
            "capabilities": capabilities,
            "executable": {}
        }))
        .unwrap()
    }

    #[test]
    fn accepts_identical_capability_sets_in_any_order() {
        let manifest = manifest_with(&[Capability::Network, Capability::FilesystemTask]);
        verify_capabilities(
            &manifest,
            &[Capability::FilesystemTask, Capability::Network],
        )
        .unwrap();
    }

    #[test]
    fn rejects_undeclared_runtime_capability() {
        let manifest = manifest_with(&[Capability::Network]);
        let error = verify_capabilities(&manifest, &[Capability::Network, Capability::Clipboard])
            .unwrap_err();
        assert!(error.to_string().contains("does not declare"));
    }

    #[test]
    fn rejects_manifest_capability_missing_at_runtime() {
        let manifest = manifest_with(&[Capability::Network, Capability::ExternalProcess]);
        let error = verify_capabilities(&manifest, &[Capability::Network]).unwrap_err();
        assert!(error.to_string().contains("does not report"));
    }
}
