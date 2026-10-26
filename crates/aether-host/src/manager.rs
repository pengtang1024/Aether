//! Plugin process supervision and probe routing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::{
    Budget, Init, LogLevel, PluginEvent, PluginKind, PluginProcess, ProbeResult, TaskPaths,
    TaskSpec,
};

use crate::abi::AbiPlugin;

use crate::error::HostError;
use crate::registry::PluginRegistry;

pub struct PluginManager {
    registry: PluginRegistry,
    data_dir: PathBuf,
    host_version: String,
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
            processes: HashMap::new(),
        }
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    pub fn probe(&mut self, input: &str) -> Vec<ProbeOutcome> {
        let ids = self
            .registry
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.manifest.id.clone())
            .collect::<Vec<_>>();

        let mut outcomes = Vec::new();
        for id in ids {
            match self
                .ensure_process(&id)
                .and_then(|process| process.probe(input).map_err(HostError::from))
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
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn shutdown(&mut self) {
        for (_, process) in self.processes.drain() {
            let _ = process.shutdown();
        }
    }

    pub fn stop(&mut self, plugin_id: &str) {
        if let Some(process) = self.processes.remove(plugin_id) {
            let _ = process.shutdown();
        }
    }

    fn ensure_process(&mut self, plugin_id: &str) -> Result<&mut RunningPlugin, HostError> {
        if !self.processes.contains_key(plugin_id) {
            let entry = self
                .registry
                .get(plugin_id)
                .ok_or_else(|| HostError::PluginNotFound(plugin_id.to_string()))?;
            if !entry.enabled {
                return Err(HostError::PluginDisabled(plugin_id.to_string()));
            }
            let executable = entry.executable()?;
            let init = Init {
                host_version: self.host_version.clone(),
                data_dir: self.data_dir.to_string_lossy().into_owned(),
                log_level: LogLevel::Info,
                proxy: None,
                limits: Default::default(),
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

    fn probe(&mut self, input: &str) -> Result<ProbeResult, HostError> {
        match self {
            Self::Process(process) => Ok(process.probe(input)?),
            Self::Abi(plugin) => plugin.probe(input),
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
