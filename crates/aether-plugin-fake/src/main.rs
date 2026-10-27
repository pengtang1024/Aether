//! Fake first-party plugin.
//!
//! It implements the real SDK handshake and command loop, but "downloads" a
//! small synthetic file into the Host-granted staging directory. It is used by
//! contract tests and for wiring the Host UI before the HTTP plugin exists.

use std::thread;
use std::time::Duration;

use aether_plugin_sdk::{
    run_stdio, Capability, FileInfo, HostCommand, LogLevel, Metadata, Plugin, PluginContext,
    PluginEvent, PluginSender, SdkError, TaskPaths,
};

const PLUGIN_ID: &str = "fake";
const PLUGIN_VERSION: &str = "0.1.0";
const FILE_NAME: &str = "fake.bin";
const FILE_SIZE: u64 = 1024;

const CAPABILITIES: &[Capability] = &[Capability::Network, Capability::FilesystemTask];

#[derive(Default)]
struct FakePlugin;

impl Plugin for FakePlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    fn on_command(
        &mut self,
        ctx: &mut PluginContext,
        command: HostCommand,
    ) -> Result<(), SdkError> {
        match command {
            HostCommand::Probe { request_id, input } => {
                let supported = input.starts_with("http://") || input.starts_with("https://");
                log(ctx, format!("probe {input}"))?;
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported,
                    protocol: supported.then(|| "http".to_string()),
                    priority: 10,
                    metadata: None,
                    suggested_filename: supported.then(|| FILE_NAME.to_string()),
                    reason: (!supported).then(|| "fake plugin only handles http(s)".to_string()),
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                let hold = task
                    .settings
                    .get("duration_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let fail = task
                    .settings
                    .get("fail")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let fail_always = task
                    .settings
                    .get("fail_always")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let retryable = task
                    .settings
                    .get("retryable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let sender = ctx.sender();
                thread::spawn(move || {
                    if let Err(error) = run_fake_task(
                        sender,
                        task.task_id,
                        task.url,
                        paths,
                        hold,
                        fail,
                        fail_always,
                        retryable,
                    ) {
                        eprintln!("aether-plugin-fake: task failed: {error}");
                    }
                });
            }
            HostCommand::Pause { task_id } | HostCommand::Resume { task_id } => {
                log(ctx, format!("fake task {task_id} pause/resume ignored"))?;
            }
            HostCommand::Cancel { task_id } => {
                ctx.send(&PluginEvent::Failed {
                    task_id,
                    error: aether_plugin_sdk::PluginError::new(
                        aether_plugin_sdk::ErrorKind::Canceled,
                        "canceled by host",
                    ),
                })?;
            }
            HostCommand::SetBudget { task_id, .. } => {
                log(ctx, format!("fake task {task_id} budget updated"))?;
            }
            HostCommand::Snapshot {
                request_id,
                task_id,
            } => {
                log(
                    ctx,
                    format!("fake snapshot request {request_id} for {task_id}"),
                )?;
            }
            HostCommand::Init { .. } => {
                return Err(SdkError::Protocol(
                    "init must be handled by the SDK runner".into(),
                ));
            }
            HostCommand::Ping { .. } | HostCommand::Shutdown => {
                return Err(SdkError::Protocol(
                    "ping/shutdown must be handled by the SDK runner".into(),
                ));
            }
        }

        Ok(())
    }
}

fn trace(message: String) {
    if std::env::var_os("AETHER_FAKE_TRACE").is_some() {
        eprintln!("aether-plugin-fake: {message}");
    }
}

fn run_fake_task(
    sender: PluginSender,
    task_id: String,
    url: String,
    paths: TaskPaths,
    hold_ms: u64,
    fail: bool,
    fail_always: bool,
    retryable: bool,
) -> Result<(), SdkError> {
    trace(format!(
        "task {task_id}: hold={hold_ms} fail={fail} fail_always={fail_always} retryable={retryable}"
    ));
    std::fs::create_dir_all(&paths.staging)?;
    // A sentinel in staging survives plugin restarts, so `fail` can model a
    // "fails once, then succeeds" task for resume testing while `fail_always`
    // models a persistent failure for retry-cap tests.
    let sentinel = paths.staging.join(".fake-attempted");
    let attempted = sentinel.exists();
    std::fs::write(&sentinel, b"1")?;
    if fail_always || (fail && !attempted) {
        trace(format!(
            "task {task_id}: sending Failed (attempted={attempted})"
        ));
        sender.send(&PluginEvent::Failed {
            task_id: task_id.clone(),
            error: aether_plugin_sdk::PluginError::new(
                aether_plugin_sdk::ErrorKind::Protocol,
                "fake intentional failure",
            )
            .retryable(retryable),
        })?;
        trace(format!("task {task_id}: Failed sent"));
        return Ok(());
    }
    trace(format!(
        "task {task_id}: proceeding to completion (attempted={attempted})"
    ));
    let payload = vec![0xA5_u8; FILE_SIZE as usize];
    std::fs::write(paths.staging.join(FILE_NAME), &payload)?;

    sender.send(&PluginEvent::Metadata {
        task_id: task_id.clone(),
        metadata: Metadata {
            final_url: url,
            size: Some(FILE_SIZE),
            etag: Some("fake-etag".into()),
            content_type: Some("application/octet-stream".into()),
            files: vec![],
        },
    })?;

    let steps = 5_u64;
    // Hold the task alive for `duration_ms` (queue/scheduling tests) or the
    // default 20 ms per step when unset.
    let step_ms = hold_ms.max(steps) / steps;
    for step in 1..=steps {
        thread::sleep(Duration::from_millis(step_ms));
        sender.send(&PluginEvent::Progress {
            task_id: task_id.clone(),
            seq: step,
            downloaded: FILE_SIZE * step / steps,
            total: Some(FILE_SIZE),
            speed_bps: 50 * 1024,
            eta_secs: Some(steps - step),
            connections: 1,
            chunks_done: Some(step as u32),
            chunks_total: Some(steps as u32),
        })?;
    }

    sender.send(&PluginEvent::Completed {
        task_id,
        files: vec![FileInfo {
            relative_path: FILE_NAME.into(),
            size: FILE_SIZE,
            checksum: None,
        }],
        plugin_state: None,
    })?;

    Ok(())
}

fn log(ctx: &mut PluginContext, message: String) -> Result<(), SdkError> {
    ctx.send(&PluginEvent::Log {
        level: LogLevel::Debug,
        message,
        fields: Default::default(),
    })
}

fn main() {
    if let Err(error) = run_stdio(FakePlugin) {
        eprintln!("aether-plugin-fake: {error}");
        std::process::exit(1);
    }
}
