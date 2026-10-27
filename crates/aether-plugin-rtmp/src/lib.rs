//! RTMP/RTSP capture through ffmpeg.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::{shared_progress, ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Capability, Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, Plugin, PluginContext,
    PluginError, PluginEvent, PluginSender, SdkError,
};
use thiserror::Error;

const PLUGIN_ID: &str = "rtmp";
const PLUGIN_VERSION: &str = "0.1.0";
const CAPABILITIES: &[Capability] = &[
    Capability::Network,
    Capability::FilesystemTask,
    Capability::ExternalProcess,
];

pub struct RtmpPlugin {
    tasks: HashMap<String, TaskRecord>,
}

struct TaskRecord {
    control: Arc<TaskControl>,
    progress: SharedProgress,
}

impl RtmpPlugin {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    fn ffmpeg(task: &aether_plugin_sdk::TaskSpec) -> String {
        task.settings
            .get("ffmpeg_path")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("AETHER_FFMPEG").ok())
            .unwrap_or_else(|| "ffmpeg".into())
    }
}

impl Default for RtmpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for RtmpPlugin {
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
        command: aether_plugin_sdk::HostCommand,
    ) -> Result<(), SdkError> {
        use aether_plugin_sdk::HostCommand;
        match command {
            HostCommand::Probe {
                request_id, input, ..
            } => {
                let supported = input.starts_with("rtmp://")
                    || input.starts_with("rtmps://")
                    || input.starts_with("rtsp://");
                let executable = std::env::var("AETHER_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
                let available = Command::new(&executable)
                    .arg("-version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                ctx.send(&PluginEvent::ProbeResult {
                    request_id,
                    supported: supported && available,
                    protocol: supported.then(|| "rtmp".into()),
                    priority: 35,
                    metadata: None,
                    suggested_filename: Some("stream.flv".into()),
                    reason: (!available).then(|| format!("ffmpeg not found at {executable}")),
                })?;
            }
            HostCommand::Start { task, paths, .. } => {
                let executable = Self::ffmpeg(&task);
                let task_id = task.task_id.clone();
                let sender = ctx.sender();
                let control = Arc::new(TaskControl::new());
                let progress = shared_progress();
                let thread_control = control.clone();
                let thread_progress = progress.clone();
                thread::spawn(move || {
                    let _ = run(
                        sender,
                        task,
                        paths,
                        thread_control,
                        thread_progress,
                        executable,
                    );
                });
                self.tasks.insert(task_id, TaskRecord { control, progress });
            }
            // Live captures cannot be paused reliably; cancel and restart.
            HostCommand::Pause { .. } | HostCommand::Resume { .. } => {}
            HostCommand::Cancel { task_id } => {
                if let Some(record) = self.tasks.get(&task_id) {
                    record.control.cancel();
                }
            }
            HostCommand::SetBudget { .. } => {}
            HostCommand::Snapshot { task_id, .. } => {
                let Some(record) = self.tasks.get(&task_id) else {
                    return Ok(());
                };
                let snapshot = record.progress.lock().unwrap().clone();
                ctx.send(&PluginEvent::Progress {
                    task_id,
                    seq: snapshot.seq,
                    downloaded: snapshot.downloaded,
                    total: snapshot.total,
                    speed_bps: snapshot.speed_bps,
                    eta_secs: None,
                    connections: 1,
                    chunks_done: None,
                    chunks_total: None,
                })?;
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

    fn on_shutdown(&mut self, _ctx: &mut PluginContext) -> Result<(), SdkError> {
        for record in self.tasks.values() {
            record.control.cancel();
        }
        Ok(())
    }
}

fn run(
    sender: PluginSender,
    task: aether_plugin_sdk::TaskSpec,
    paths: aether_plugin_sdk::TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
    executable: String,
) -> Result<(), RtmpError> {
    fs::create_dir_all(&paths.staging)?;
    let output: PathBuf = paths.staging.join("stream.flv");
    let mut child = Command::new(executable)
        .args(["-y", "-nostdin", "-loglevel", "warning", "-i"])
        .arg(&task.url)
        .args(["-c", "copy", "-f", "flv"])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RtmpError::Protocol("ffmpeg stderr unavailable".into()))?;
    let mut snapshot = ProgressSnapshot {
        connections: 1,
        ..Default::default()
    };
    let mut last = Instant::now();

    for line in BufReader::new(stderr).lines() {
        let line = line?;
        if control.is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RtmpError::Canceled);
        }
        if let Some(seconds) = parse_time(&line) {
            if last.elapsed() >= Duration::from_millis(250) {
                snapshot.downloaded = seconds;
                snapshot.seq = snapshot.seq.saturating_add(1);
                *progress.lock().unwrap() = snapshot.clone();
                let _ = sender.send(&PluginEvent::Progress {
                    task_id: task.task_id.clone(),
                    seq: snapshot.seq,
                    downloaded: seconds,
                    total: None,
                    speed_bps: 0,
                    eta_secs: None,
                    connections: 1,
                    chunks_done: None,
                    chunks_total: None,
                });
                last = Instant::now();
            }
        }
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(RtmpError::Protocol(format!("ffmpeg exited with {status}")));
    }
    if !output.is_file() {
        return Err(RtmpError::Protocol("ffmpeg produced no output".into()));
    }

    let checksum = aether_plugin_sdk::checksum::sha256_file(&output)?;
    sender.send(&PluginEvent::Completed {
        task_id: task.task_id,
        files: vec![FileInfo {
            relative_path: "stream.flv".into(),
            size: fs::metadata(&output)?.len(),
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        }],
        plugin_state: None,
    })?;
    Ok(())
}

fn parse_time(line: &str) -> Option<u64> {
    let value = line.split("time=").nth(1)?.split_whitespace().next()?;
    let mut parts = value.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600 + minutes * 60 + seconds as u64)
}

#[derive(Debug, Error)]
pub enum RtmpError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("canceled")]
    Canceled,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl RtmpError {
    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::Io(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Sdk(error) => PluginError::new(ErrorKind::Internal, error.to_string()),
            Self::Protocol(message) => PluginError::new(ErrorKind::Protocol, message.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_time() {
        assert_eq!(
            parse_time("frame= 10 time=00:01:23.45 bitrate=1.2"),
            Some(83)
        );
    }
}
