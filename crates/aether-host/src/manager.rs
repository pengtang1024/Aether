//! Plugin process supervision and probe routing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::{
    Budget, Capability, Init, LogLevel, NetworkConfig, PluginEvent, PluginKind, PluginProcess,
    ProbeResult, ProxyConfig, TaskPaths, TaskSpec,
};

use crate::abi::AbiPlugin;

use crate::error::HostError;
use crate::registry::PluginRegistry;

pub struct PluginManager {
    registry: PluginRegistry,
    data_dir: PathBuf,
    host_version: String,
    /// Passed to every plugin process on spawn. `None` keeps the plugin's own
    /// default (usually "honor the environment").
    proxy: Option<ProxyConfig>,
    /// Host-wide network behaviour (user agent, IP mode, DNS servers) handed
    /// to plugin processes on spawn.
    network: Option<NetworkConfig>,
    /// When false, plugins that declare the `external-process` capability are
    /// refused at spawn time. Process-level sandboxing is out of scope; this
    /// is the policy gate the daemon exposes as a setting.
    allow_external_process: bool,
    processes: HashMap<String, RunningPlugin>,
}

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub plugin_id: String,
    pub result: Option<ProbeResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PluginEventEnvelope {
    pub plugin_id: String,
    pub event: PluginEvent,
}

impl PluginManager {
    pub fn new(
        registry: PluginRegistry,
        data_dir: impl Into<PathBuf>,
        host_version: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            data_dir: data_dir.into(),
            host_version: host_version.into(),
            proxy: None,
            network: None,
            allow_external_process: true,
            processes: HashMap::new(),
        }
    }

    /// Set the proxy handed to plugin processes. Takes effect the next time a
    /// plugin is spawned; already running plugins keep their old setting.
    pub fn set_proxy(&mut self, proxy: Option<ProxyConfig>) {
        self.proxy = proxy;
    }

    pub fn proxy(&self) -> Option<&ProxyConfig> {
        self.proxy.as_ref()
    }

    /// Set the network configuration handed to plugin processes. Like the
    /// proxy, it takes effect on the next spawn.
    pub fn set_network(&mut self, network: Option<NetworkConfig>) {
        self.network = network;
    }

    /// Allow or refuse plugins that declare `external-process`. Takes effect
    /// on the next spawn; already running processes keep going.
    pub fn set_allow_external_process(&mut self, allow: bool) {
        self.allow_external_process = allow;
    }

    pub fn allow_external_process(&self) -> bool {
        self.allow_external_process
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    pub fn probe(&mut self, input: &str) -> Vec<ProbeOutcome> {
        self.probe_with_proxy(input, None)
    }

    /// Probe with the Host-resolved proxy for this input; plugins must use it
    /// instead of the process-level proxy so PAC routes probes like downloads.
    pub fn probe_with_proxy(
        &mut self,
        input: &str,
        proxy: Option<ProxyConfig>,
    ) -> Vec<ProbeOutcome> {
        let ids = self
            .registry
            .iter()
            .filter(|entry| entry.enabled && entry.manifest.kind != PluginKind::Service)
            .map(|entry| entry.manifest.id.clone())
            .collect::<Vec<_>>();

        let mut outcomes = Vec::new();
        for id in ids {
            match self
                .ensure_process(&id)
                .and_then(|process| process.probe(input, proxy.clone()).map_err(HostError::from))
            {
                Ok(result) => outcomes.push(ProbeOutcome {
                    plugin_id: id,
                    result: Some(result),
                    error: None,
                }),
                Err(error) => outcomes.push(ProbeOutcome {
                    plugin_id: id,
                    result: None,
                    error: Some(error.to_string()),
                }),
            }
        }
        outcomes.sort_by_key(|outcome| {
            std::cmp::Reverse(
                outcome
                    .result
                    .as_ref()
                    .map(|result| result.priority)
                    .unwrap_or(i32::MIN),
            )
        });
        outcomes
    }

    pub fn start(
        &mut self,
        plugin_id: &str,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    ) -> Result<u64, HostError> {
        let process = self.ensure_process(plugin_id)?;
        Ok(process.start_task(task, paths, budget)?)
    }

    pub fn send(
        &mut self,
        plugin_id: &str,
        command: &aether_plugin_sdk::HostCommand,
    ) -> Result<(), HostError> {
        let process = self.ensure_process(plugin_id)?;
        Ok(process.send(command)?)
    }

    pub fn recv_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PluginEventEnvelope>, HostError> {
        let deadline = Instant::now() + timeout;
        loop {
            for (id, process) in self.processes.iter_mut() {
                match process.recv_event(Duration::from_millis(0)) {
                    Ok(event) => {
                        return Ok(Some(PluginEventEnvelope {
                            plugin_id: id.clone(),
                            event,
                        }))
                    }
                    Err(HostError::Sdk(aether_plugin_sdk::SdkError::Timeout(_))) => {}
                    Err(error) => return Err(error),
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_all();
    }

    /// Stop all running plugin processes but keep the registry intact.
    pub fn stop_all(&mut self) {
        for (_, process) in self.processes.drain() {
            let _ = process.shutdown();
        }
    }

    pub fn stop(&mut self, plugin_id: &str) {
        if let Some(process) = self.processes.remove(plugin_id) {
            let _ = process.shutdown();
        }
    }

    /// PID of a running process plugin (test/diagnostics helper); `None`
    /// for ABI plugins or plugins that are not running.
    pub fn running_pid(&self, plugin_id: &str) -> Option<u32> {
        self.processes.get(plugin_id).and_then(RunningPlugin::pid)
    }

    /// Drop processes that already exited so the next command respawns them.
    /// Returns the removed plugin ids (the daemon logs them).
    pub fn prune_dead(&mut self) -> Vec<String> {
        let dead = self
            .processes
            .iter_mut()
            .filter_map(|(id, process)| process.has_exited().then(|| id.clone()))
            .collect::<Vec<_>>();
        for id in &dead {
            self.processes.remove(id);
        }
        dead
    }

    fn ensure_process(&mut self, plugin_id: &str) -> Result<&mut RunningPlugin, HostError> {
        // A crashed plugin must not poison every later command: drop the
        // dead handle and spawn a fresh process below.
        let dead = self
            .processes
            .get_mut(plugin_id)
            .is_some_and(RunningPlugin::has_exited);
        if dead {
            self.processes.remove(plugin_id);
        }

        if !self.processes.contains_key(plugin_id) {
            let entry = self
                .registry
                .get(plugin_id)
                .ok_or_else(|| HostError::PluginNotFound(plugin_id.to_string()))?;
            if !entry.enabled {
                return Err(HostError::PluginDisabled(plugin_id.to_string()));
            }
            if !self.allow_external_process
                && entry
                    .manifest
                    .capabilities
                    .contains(&Capability::ExternalProcess)
            {
                return Err(HostError::CapabilityDenied {
                    plugin: plugin_id.to_string(),
                    capability: Capability::ExternalProcess.as_str().to_string(),
                });
            }
            let executable = entry.executable()?;
            let init = Init {
                host_version: self.host_version.clone(),
                data_dir: self.data_dir.to_string_lossy().into_owned(),
                log_level: LogLevel::Info,
                proxy: self.proxy.clone(),
                limits: Default::default(),
                network: self.network.clone(),
            };
            let running = match entry.manifest.kind {
                PluginKind::Process => RunningPlugin::Process(PluginProcess::spawn(
                    entry.manifest.clone(),
                    &executable,
                    init,
                )?),
                PluginKind::Cdylib => {
                    let mut plugin = AbiPlugin::load(&executable)?;
                    plugin.handshake(init)?;
                    RunningPlugin::Abi(plugin)
                }
                PluginKind::Service => return Err(HostError::ServicePlugin(plugin_id.to_string())),
            };
            self.processes.insert(plugin_id.to_string(), running);
        }
        Ok(self.processes.get_mut(plugin_id).expect("inserted above"))
    }
}

enum RunningPlugin {
    Process(PluginProcess),
    Abi(AbiPlugin),
}

impl RunningPlugin {
    /// Process plugins can be observed directly; in-process ABI plugins have
    /// no separate lifetime to check.
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Process(process) => Some(process.pid()),
            Self::Abi(_) => None,
        }
    }

    fn has_exited(&mut self) -> bool {
        match self {
            Self::Process(process) => process.has_exited(),
            Self::Abi(_) => false,
        }
    }

    fn send(&mut self, command: &aether_plugin_sdk::HostCommand) -> Result<(), HostError> {
        match self {
            Self::Process(process) => Ok(process.send(command)?),
            Self::Abi(plugin) => plugin.send(command),
        }
    }

    fn recv_event(&mut self, timeout: Duration) -> Result<PluginEvent, HostError> {
        match self {
            Self::Process(process) => Ok(process.recv_event(timeout)?),
            Self::Abi(plugin) => plugin.recv_event(timeout),
        }
    }

    fn probe(&mut self, input: &str, proxy: Option<ProxyConfig>) -> Result<ProbeResult, HostError> {
        match self {
            Self::Process(process) => Ok(process.probe_with_proxy(input, proxy)?),
            Self::Abi(plugin) => plugin.probe_with_proxy(input, proxy),
        }
    }

    fn start_task(
        &mut self,
        task: TaskSpec,
        paths: TaskPaths,
        budget: Budget,
    ) -> Result<u64, HostError> {
        match self {
            Self::Process(process) => Ok(process.start_task(task, paths, budget)?),
            Self::Abi(plugin) => plugin.start_task(task, paths, budget),
        }
    }

    fn shutdown(self) -> Result<(), HostError> {
        match self {
            Self::Process(process) => Ok(process.shutdown()?),
            Self::Abi(plugin) => plugin.shutdown(),
        }
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
