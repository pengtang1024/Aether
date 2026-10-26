//! yt-dlp adapter: probe with `-J`, download with a staging output template.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::{ProgressSnapshot, SharedProgress, TaskControl};
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, ErrorKind, FileInfo, PluginError, PluginEvent, PluginSender,
    TaskPaths, TaskSpec,
};
use serde_json::Value;
use thiserror::Error;

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

pub struct ExtractProbe {
    pub supported: bool,
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub reason: Option<String>,
}

pub fn probe(input: &str, executable: &str) -> ExtractProbe {
    let output = match run_with_timeout(
        Command::new(executable)
            .args(["-J", "--no-warnings", "--no-playlist", "--", input])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        PROBE_TIMEOUT,
    ) {
        Ok(output) => output,
        Err(error) => {
            return ExtractProbe {
                supported: false,
                filename: None,
                size: None,
                reason: Some(error.to_string()),
            }
        }
    };

    if !output.status.success() {
        return ExtractProbe {
            supported: false,
            filename: None,
            size: None,
            reason: Some(
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .last()
                    .unwrap_or("yt-dlp probe failed")
                    .to_string(),
            ),
        };
    }

    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => {
            return ExtractProbe {
                supported: false,
                filename: None,
                size: None,
                reason: Some(format!("invalid yt-dlp JSON: {error}")),
            }
        }
    };

    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("download");
    let ext = value.get("ext").and_then(Value::as_str).unwrap_or("bin");
    let filename = aether_plugin_sdk::fsutil::sanitize_filename(&format!("{title}.{ext}"));
    let size = value
        .get("filesize")
        .and_then(Value::as_u64)
        .or_else(|| value.get("filesize_approx").and_then(Value::as_u64));

    ExtractProbe {
        supported: true,
        filename,
        size,
        reason: None,
    }
}

pub fn run(
    sender: PluginSender,
    task: TaskSpec,
    paths: TaskPaths,
    control: Arc<TaskControl>,
    progress: SharedProgress,
    executable: String,
) {
    match run_inner(&sender, &task, &paths, &control, &progress, &executable) {
        Ok(()) => {}
        Err(ExtractError::Canceled) => {}
        Err(error) => {
            let _ = sender.send(&PluginEvent::Failed {
                task_id: task.task_id,
                error: error.to_plugin_error(),
            });
        }
    }
}

fn run_inner(
    sender: &PluginSender,
    task: &TaskSpec,
    paths: &TaskPaths,
    control: &TaskControl,
    progress: &SharedProgress,
    executable: &str,
) -> Result<(), ExtractError> {
    if control.is_canceled() {
        return Err(ExtractError::Canceled);
    }
    fs::create_dir_all(&paths.staging)?;
    let template = paths.staging.join("%(title)s.%(ext)s");

    let mut child = Command::new(executable)
        .args([
            "--newline",
            "--no-playlist",
            "--no-warnings",
            "--no-part",
            "-o",
        ])
        .arg(&template)
        .arg("--")
        .arg(&task.url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExtractError::Protocol("yt-dlp stdout unavailable".into()))?;
    let stderr = child.stderr.take();
    let stderr_thread = stderr.map(|stderr| {
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                line.clear();
            }
        })
    });

    let mut snapshot = ProgressSnapshot {
        connections: 1,
        ..Default::default()
    };
    let mut seq = 0_u64;
    let mut last_progress = Instant::now();

    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if control.is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(handle) = stderr_thread {
                let _ = handle.join();
            }
            return Err(ExtractError::Canceled);
        }
        if let Some((percent, total, speed)) = parse_progress(&line) {
            let downloaded = total.map(|total| total * percent / 100).unwrap_or(percent);
            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                seq = seq.saturating_add(1);
                snapshot.downloaded = downloaded;
                snapshot.total = total;
                snapshot.speed_bps = speed;
                snapshot.seq = seq;
                *progress.lock().unwrap() = snapshot.clone();
                let _ = sender.send(&PluginEvent::Progress {
                    task_id: task.task_id.clone(),
                    seq,
                    downloaded,
                    total,
                    speed_bps: speed,
                    eta_secs: None,
                    connections: 1,
                    chunks_done: None,
                    chunks_total: None,
                });
                last_progress = Instant::now();
            }
        }
    }

    let status = child.wait()?;
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    if !status.success() {
        return Err(ExtractError::Protocol(format!(
            "yt-dlp exited with {status}"
        )));
    }

    let mut files = Vec::new();
    for path in list_files(&paths.staging)? {
        let metadata = fs::metadata(&path)?;
        if metadata.len() == 0 {
            continue;
        }
        let relative = path
            .strip_prefix(&paths.staging)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let checksum = aether_plugin_sdk::checksum::sha256_file(&path)?;
        files.push(FileInfo {
            relative_path: relative,
            size: metadata.len(),
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: checksum,
            }),
        });
    }

    if files.is_empty() {
        return Err(ExtractError::Protocol("yt-dlp produced no files".into()));
    }

    sender.send(&PluginEvent::Completed {
        task_id: task.task_id.clone(),
        files,
        plugin_state: None,
    })?;

    Ok(())
}

fn list_files(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(list_files(&path)?);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.ends_with(".part") || name.ends_with(".ytdl") {
            continue;
        }
        files.push(path);
    }
    Ok(files)
}

fn parse_progress(line: &str) -> Option<(u64, Option<u64>, u64)> {
    let rest = line.strip_prefix("[download] ")?;
    let percent = rest.split('%').next()?.trim().parse::<f64>().ok()?;
    let percent = percent.clamp(0.0, 100.0) as u64;

    let total = rest
        .split(" of ")
        .nth(1)
        .and_then(|value| value.split_whitespace().next())
        .and_then(parse_size);
    let speed = rest
        .split(" at ")
        .nth(1)
        .and_then(|value| value.split_whitespace().next())
        .and_then(parse_size)
        .unwrap_or(0);

    Some((percent, total, speed))
}

fn parse_size(value: &str) -> Option<u64> {
    let number = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>()
        .parse::<f64>()
        .ok()?;
    let multiplier = if value.contains("GiB") {
        1024.0 * 1024.0 * 1024.0
    } else if value.contains("MiB") {
        1024.0 * 1024.0
    } else if value.contains("KiB") {
        1024.0
    } else {
        1.0
    };
    Some((number * multiplier) as u64)
}

fn run_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, ExtractError> {
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            return Ok(output);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExtractError::Timeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sdk error: {0}")]
    Sdk(#[from] aether_plugin_sdk::SdkError),

    #[error("canceled")]
    Canceled,

    #[error("yt-dlp timed out")]
    Timeout,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl ExtractError {
    pub fn to_plugin_error(&self) -> PluginError {
        match self {
            Self::Canceled => PluginError::new(ErrorKind::Canceled, "canceled"),
            Self::Timeout => {
                PluginError::new(ErrorKind::Timeout, "yt-dlp timed out").retryable(true)
            }
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
    fn parses_ytdlp_progress() {
        let parsed = parse_progress("[download]  12.3% of 1.50MiB at 1.20MiB/s ETA 00:01").unwrap();
        assert_eq!(parsed.0, 12);
        assert_eq!(parsed.1, Some(1_572_864));
        assert_eq!(parsed.2, 1_258_291);
    }
}
