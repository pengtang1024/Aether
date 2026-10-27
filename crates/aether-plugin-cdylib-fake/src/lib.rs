//! Fake in-process plugin used to exercise the cdylib ABI.

use aether_plugin_sdk::{
    Capability, Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, HostCommand, Metadata, Plugin,
    PluginContext, PluginEvent, SdkError,
};

const MANIFEST: &str = r#"{
  "id": "abi-fake",
  "name": "ABI fake plugin",
  "version": "0.1.0",
  "api": 1,
  "kind": "cdylib",
  "protocols": ["http"],
  "capabilities": ["network", "filesystem:task"],
  "executable": {"linux-x86_64": "libaether_plugin_cdylib_fake.so"}
}"#;

struct AbiFake;

impl Plugin for AbiFake {
    fn id(&self) -> &str {
        "abi-fake"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::Network, Capability::FilesystemTask]
    }

    fn on_command(
        &mut self,
        ctx: &mut PluginContext,
        command: HostCommand,
    ) -> Result<(), SdkError> {
        match command {
            HostCommand::Probe {
                request_id, input, ..
            } => {
                let supported = input.starts_with("http://") || input.starts_with("https://");
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol: supported.then(|| "http".into()),
                    priority: 5,
                    metadata: supported.then(|| Metadata {
                        final_url: input,
                        size: Some(2048),
                        etag: Some("abi-fake".into()),
                        content_type: Some("application/octet-stream".into()),
                        files: vec![],
                    }),
                    suggested_filename: supported.then(|| "abi-fake.bin".into()),
                    reason: None,
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                std::fs::create_dir_all(&paths.staging)?;
                let payload = vec![0x5A_u8; 2048];
                std::fs::write(paths.staging.join("abi-fake.bin"), &payload)?;
                ctx.send(&PluginEvent::Metadata {
                    task_id: task.task_id.clone(),
                    metadata: Metadata {
                        final_url: task.url.clone(),
                        size: Some(2048),
                        etag: None,
                        content_type: None,
                        files: vec![],
                    },
                })?;
                ctx.send(&PluginEvent::Progress {
                    task_id: task.task_id.clone(),
                    seq: 1,
                    downloaded: 1024,
                    total: Some(2048),
                    speed_bps: 1024,
                    eta_secs: Some(1),
                    connections: 1,
                    chunks_done: Some(1),
                    chunks_total: Some(2),
                })?;
                let checksum = aether_plugin_sdk::checksum::sha256_bytes(&payload);
                ctx.send(&PluginEvent::Completed {
                    task_id: task.task_id,
                    files: vec![FileInfo {
                        relative_path: "abi-fake.bin".into(),
                        size: 2048,
                        checksum: Some(Checksum {
                            algorithm: ChecksumAlgorithm::Sha256,
                            value: checksum,
                        }),
                    }],
                    plugin_state: None,
                })?;
            }
            HostCommand::Cancel { task_id } => {
                ctx.send(&PluginEvent::Failed {
                    task_id,
                    error: aether_plugin_sdk::PluginError::new(ErrorKind::Canceled, "canceled"),
                })?;
            }
            HostCommand::Pause { .. }
            | HostCommand::Resume { .. }
            | HostCommand::SetBudget { .. }
            | HostCommand::Snapshot { .. } => {}
            HostCommand::Init { .. } => {
                return Err(SdkError::Protocol("init handled by ABI runner".into()))
            }
            HostCommand::Ping { .. } | HostCommand::Shutdown => {
                return Err(SdkError::Protocol(
                    "ping/shutdown handled by ABI runner".into(),
                ))
            }
        }
        Ok(())
    }
}

aether_plugin_abi::export_abi_plugin!(AbiFake, MANIFEST);
