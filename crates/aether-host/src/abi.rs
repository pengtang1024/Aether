//! Host-side loader for optional in-process `cdylib` plugins.

use std::ffi::{c_void, CStr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aether_plugin_abi::{AetherPluginApi, ABI_VERSION};
use aether_plugin_sdk::{
    ipc, HostCommand, Init, PluginEvent, PluginKind, PluginManifest, ProbeResult, TaskPaths,
    TaskSpec, API_VERSION,
};
use libloading::Library;

use crate::error::HostError;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct AbiPlugin {
    api: AetherPluginApi,
    handle: *mut c_void,
    manifest: PluginManifest,
    path: PathBuf,
    _library: Library,
}

impl AbiPlugin {
    pub fn load(path: &Path) -> Result<Self, HostError> {
        let library = unsafe { Library::new(path) }
            .map_err(|error| HostError::Abi(format!("load {}: {error}", path.display())))?;
        let symbol: libloading::Symbol<extern "C" fn() -> *const AetherPluginApi> =
            unsafe { library.get(b"aether_plugin_api\0") }
                .map_err(|error| HostError::Abi(format!("aether_plugin_api: {error}")))?;
        let api_ptr = symbol();
        if api_ptr.is_null() {
            return Err(HostError::Abi("plugin returned a null ABI table".into()));
        }
        let api = unsafe { *api_ptr };
        if api.abi_version != ABI_VERSION {
            return Err(HostError::Abi(format!(
                "ABI mismatch: plugin {}, host {ABI_VERSION}",
                api.abi_version
            )));
        }

        let manifest_ptr = unsafe { (api.manifest_json)() };
        if manifest_ptr.is_null() {
            return Err(HostError::Abi("plugin returned a null manifest".into()));
        }
        let manifest_json = unsafe { CStr::from_ptr(manifest_ptr) }
            .to_str()
            .map_err(|error| HostError::Abi(format!("manifest utf-8: {error}")))?;
        let manifest = PluginManifest::from_json(manifest_json)?;
        manifest.validate_identity()?;
        if manifest.kind != PluginKind::Cdylib {
            return Err(HostError::Abi(format!(
                "{} is not marked kind=cdylib",
                manifest.id
            )));
        }

        let handle = unsafe { (api.create)(std::ptr::null()) };
        if handle.is_null() {
            return Err(HostError::Abi("plugin create returned null".into()));
        }

        Ok(Self {
            api,
            handle,
            manifest,
            path: path.to_path_buf(),
            _library: library,
        })
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn handshake(&mut self, init: Init) -> Result<(), HostError> {
        match self.recv_event(HANDSHAKE_TIMEOUT)? {
            PluginEvent::Hello {
                api,
                plugin_id,
                version,
                ..
            } => {
                if api != API_VERSION || api != self.manifest.api {
                    return Err(HostError::Abi("plugin API mismatch".into()));
                }
                if plugin_id != self.manifest.id || version != self.manifest.version {
                    return Err(HostError::Abi("plugin identity mismatch".into()));
                }
            }
            other => return Err(HostError::Abi(format!("expected Hello, got {other:?}"))),
        }
        self.send(&HostCommand::Init {
            host_version: init.host_version,
            data_dir: init.data_dir,
            log_level: init.log_level,
            proxy: init.proxy,
            limits: init.limits,
        })?;
        match self.recv_event(HANDSHAKE_TIMEOUT)? {
            PluginEvent::Ready { api } if api == API_VERSION => Ok(()),
            other => Err(HostError::Abi(format!("expected Ready, got {other:?}"))),
        }
    }

    pub fn send(&mut self, command: &HostCommand) -> Result<(), HostError> {
        let frame = ipc::encode_frame(command)
            .map_err(|error| HostError::Abi(format!("encode command: {error}")))?;
        let status = unsafe { (self.api.send)(self.handle, frame.as_ptr(), frame.len()) };
        if status == 0 {
            Ok(())
        } else {
            Err(HostError::Abi("plugin rejected the command".into()))
        }
    }

    pub fn recv_event(&mut self, timeout: Duration) -> Result<PluginEvent, HostError> {
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut len = 0_usize;
        let status = unsafe {
            (self.api.poll)(
                self.handle,
                &mut data,
                &mut len,
                timeout.as_millis().min(u32::MAX as u128) as u32,
            )
        };
        if status == 0 {
            return Err(aether_plugin_sdk::SdkError::Timeout("ABI plugin event").into());
        }
        if status < 0 || data.is_null() {
            return Err(HostError::Abi("ABI poll failed".into()));
        }
        let frame = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        unsafe { (self.api.free_buffer)(data, len) };
        ipc::decode_frame(&frame).map_err(|error| HostError::Abi(format!("decode event: {error}")))
    }

    pub fn probe(&mut self, input: &str) -> Result<ProbeResult, HostError> {
        self.send(&HostCommand::Probe {
            request_id: 1,
            input: input.to_string(),
        })?;
        loop {
            match self.recv_event(PROBE_TIMEOUT)? {
                PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol,
                    priority,
                    metadata,
                    suggested_filename,
                    reason,
                } if request_id == 1 => {
                    return Ok(ProbeResult {
                        request_id,
                        supported,
                        protocol,
                        priority,
                        metadata,
                        suggested_filename,
                        reason,
                    })
                }
                PluginEvent::Log { .. } | PluginEvent::Heartbeat { .. } => continue,
                other => {
                    return Err(HostError::Abi(format!(
                        "unexpected event while probing: {other:?}"
                    )))
                }
            }
        }
    }

    pub fn start_task(
        &mut self,
        task: TaskSpec,
        paths: TaskPaths,
        budget: aether_plugin_sdk::Budget,
    ) -> Result<u64, HostError> {
        self.send(&HostCommand::Start {
            request_id: 2,
            task,
            paths,
            budget,
        })?;
        Ok(2)
    }

    pub fn shutdown(mut self) -> Result<(), HostError> {
        let _ = self.send(&HostCommand::Shutdown);
        Ok(())
    }
}

impl Drop for AbiPlugin {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.api.destroy)(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}
