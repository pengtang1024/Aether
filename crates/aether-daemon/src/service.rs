//! Shared lifecycle for `kind: "service"` plugins (aria2 compatibility, …).
//!
//! The daemon's download plugins are owned by [`aether_host::PluginManager`],
//! which is main-loop state. Compatibility services are called from HTTP/WS
//! threads instead, so they need a separately shareable supervisor. Each
//! service has its own slot lock: a slow aria2 call cannot block a future
//! second service, and status probes never wait on an in-flight call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use aether_host::PluginRegistry;
use aether_plugin_sdk::{HostCommand, Init, LogLevel, PluginKind, PluginManifest, PluginProcess};
use serde_json::Value;

/// One compatibility transport. HTTP/WS parsing stays in `aria2`; the
/// supervisor only turns a payload into a plugin call.
pub trait Bridge: Send + Sync {
    fn call(&self, payload: Value) -> Result<Value, String>;
}

/// Adapter from the daemon's HTTP handler to one named service.
pub struct ServiceBridge {
    supervisor: Arc<ServiceSupervisor>,
    service: String,
}

impl ServiceBridge {
    pub fn new(supervisor: Arc<ServiceSupervisor>, service: impl Into<String>) -> Self {
        Self {
            supervisor,
            service: service.into(),
        }
    }
}

impl Bridge for ServiceBridge {
    fn call(&self, payload: Value) -> Result<Value, String> {
        self.supervisor.call(&self.service, payload)
    }
}

pub struct ServiceSupervisor {
    socket: PathBuf,
    data_dir: PathBuf,
    host_version: String,
    listen: Mutex<Option<String>>,
    services: Mutex<HashMap<String, Arc<Mutex<ServiceSlot>>>>,
}

struct ServiceSlot {
    service: String,
    manifest: PluginManifest,
    executable: PathBuf,
    process: Option<PluginProcess>,
    last_error: Option<String>,
    starts: u32,
}

/// Snapshot for the plugins RPC / settings page.
#[derive(Clone, Debug)]
pub struct ServiceStatus {
    pub service: String,
    pub running: bool,
    pub last_error: Option<String>,
    pub starts: u32,
}

impl ServiceSupervisor {
    pub fn new(socket: PathBuf, data_dir: PathBuf, host_version: String) -> Arc<Self> {
        Arc::new(Self {
            socket,
            data_dir,
            host_version,
            listen: Mutex::new(None),
            services: Mutex::new(HashMap::new()),
        })
    }

    /// The daemon's bound RPC address, used by plugins for blob URLs. Existing
    /// processes receive the new value on a best-effort basis.
    pub fn set_listen(&self, listen: Option<String>) {
        if let Ok(mut current) = self.listen.lock() {
            *current = listen.clone();
        }
        let Some(listen) = listen else {
            return;
        };
        let slots = self
            .services
            .lock()
            .map(|services| services.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for slot in slots {
            // Never wait on a slot that is serving a request; the address is
            // only needed for blob URLs and the next spawn sends it again.
            if let Ok(mut slot) = slot.try_lock() {
                if let Some(process) = slot.process.as_mut() {
                    let _ = process.send(&HostCommand::ServiceAddress {
                        listen: listen.clone(),
                    });
                }
            }
        }
    }

    /// Reconcile the running set with the registry: start enabled services,
    /// restart changed versions, stop disabled/removed ones. Safe to call from
    /// the main loop: a busy slot is skipped instead of blocking.
    pub fn sync(&self, registry: &PluginRegistry) {
        let desired: HashMap<String, (PluginManifest, PathBuf)> = registry
            .iter()
            .filter(|entry| entry.enabled && entry.manifest.kind == PluginKind::Service)
            .filter_map(|entry| {
                let service = entry.manifest.service.clone()?;
                let executable = entry.executable().ok()?;
                Some((service, (entry.manifest.clone(), executable)))
            })
            .collect();

        let Ok(mut services) = self.services.lock() else {
            return;
        };
        let existing = services.keys().cloned().collect::<Vec<_>>();
        for service in existing {
            if desired.contains_key(&service) {
                continue;
            }
            if let Some(slot) = services.remove(&service) {
                stop_async(slot);
            }
        }

        for (service, (manifest, executable)) in desired {
            if let Some(existing) = services.get(&service).cloned() {
                // A busy slot is left alone until the next rescan; stopping it
                // here could deadlock with a plugin callback into this loop.
                let Ok(mut slot) = existing.try_lock() else {
                    continue;
                };
                let changed =
                    slot.manifest.version != manifest.version || slot.executable != executable;
                let dead = slot.process.as_mut().is_none_or(PluginProcess::has_exited);
                if !changed && !dead {
                    continue;
                }
                if let Some(mut process) = slot.process.take() {
                    let _ = process.stop();
                }
                slot.manifest = manifest;
                slot.executable = executable;
                let _ = self.spawn(&mut slot);
                continue;
            }

            let mut slot = ServiceSlot {
                service: service.clone(),
                manifest,
                executable,
                process: None,
                last_error: None,
                starts: 0,
            };
            let _ = self.spawn(&mut slot);
            services.insert(service, Arc::new(Mutex::new(slot)));
        }
    }

    /// One `BridgeCall` round-trip. A missing/dead process is respawned first;
    /// the slot is replaced in place so the HTTP thread does not need to know.
    pub fn call(&self, service: &str, payload: Value) -> Result<Value, String> {
        let slot = self
            .services
            .lock()
            .ok()
            .and_then(|services| services.get(service).cloned())
            .ok_or_else(|| format!("compatibility service {service} is not installed"))?;
        let mut slot = slot
            .lock()
            .map_err(|_| format!("compatibility service {service} lock poisoned"))?;
        let dead = slot.process.as_mut().is_none_or(PluginProcess::has_exited);
        if dead {
            if let Some(mut process) = slot.process.take() {
                let _ = process.stop();
            }
            self.spawn(&mut slot)?;
        }
        let result = slot
            .process
            .as_mut()
            .expect("spawned above")
            .bridge_call(payload);
        slot.last_error = result.as_ref().err().map(ToString::to_string);
        result.map_err(|error| error.to_string())
    }

    /// Stop every service. Stops run on a worker thread: an in-flight bridge
    /// call may be waiting on this same daemon loop.
    pub fn stop_all(&self) {
        let slots = self
            .services
            .lock()
            .map(|mut services| services.drain().map(|(_, slot)| slot).collect::<Vec<_>>())
            .unwrap_or_default();
        if slots.is_empty() {
            return;
        }
        thread::spawn(move || {
            for slot in slots {
                stop_slot(slot);
            }
        });
    }

    pub fn statuses(&self) -> Vec<ServiceStatus> {
        let Ok(services) = self.services.lock() else {
            return Vec::new();
        };
        services
            .iter()
            .map(|(service, slot)| match slot.try_lock() {
                Ok(mut slot) => ServiceStatus {
                    service: service.clone(),
                    running: slot
                        .process
                        .as_mut()
                        .is_some_and(|process| !process.has_exited()),
                    last_error: slot.last_error.clone(),
                    starts: slot.starts,
                },
                // A call is in flight; it holds the process open.
                Err(_) => ServiceStatus {
                    service: service.clone(),
                    running: true,
                    last_error: None,
                    starts: 0,
                },
            })
            .collect()
    }

    fn spawn(&self, slot: &mut ServiceSlot) -> Result<(), String> {
        let init = Init {
            host_version: self.host_version.clone(),
            data_dir: self.data_dir.to_string_lossy().into_owned(),
            log_level: LogLevel::Info,
            proxy: None,
            limits: Default::default(),
            network: None,
        };
        let result = (|| {
            let mut process = PluginProcess::spawn(slot.manifest.clone(), &slot.executable, init)
                .map_err(|error| format!("cannot start {}: {error}", slot.service))?;
            process
                .send(&HostCommand::ServiceInit {
                    socket: self.socket.to_string_lossy().into_owned(),
                })
                .map_err(|error| format!("cannot configure {}: {error}", slot.service))?;
            let listen = self.listen.lock().ok().and_then(|listen| listen.clone());
            if let Some(listen) = listen {
                process
                    .send(&HostCommand::ServiceAddress { listen })
                    .map_err(|error| format!("cannot configure {} blobs: {error}", slot.service))?;
            }
            Ok::<PluginProcess, String>(process)
        })();
        match result {
            Ok(process) => {
                slot.process = Some(process);
                slot.last_error = None;
                slot.starts = slot.starts.saturating_add(1);
                eprintln!(
                    "aether-daemon: compatibility service {} ready",
                    slot.service
                );
                Ok(())
            }
            Err(error) => {
                slot.last_error = Some(error.clone());
                eprintln!("aether-daemon: {error}");
                Err(error)
            }
        }
    }
}

fn stop_async(slot: Arc<Mutex<ServiceSlot>>) {
    thread::spawn(move || stop_slot(slot));
}

fn stop_slot(slot: Arc<Mutex<ServiceSlot>>) {
    let Ok(mut slot) = slot.lock() else {
        return;
    };
    if let Some(mut process) = slot.process.take() {
        let _ = process.stop();
    }
}
