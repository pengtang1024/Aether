//! Optional ffmpeg remux after a finished download.
//!
//! The task-level `transcode_to` setting names a container (`"mp4"`,
//! `"mkv"`, …). When present, the merged file is remuxed with
//! `ffmpeg -c copy` — container change only, no re-encode — and the
//! original is replaced on success. A failed remux keeps the original
//! download and reports the reason through the returned note (carried in
//! `plugin_state`): the bytes the user asked for are never thrown away
//! over a cosmetic post-processing step.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use aether_plugin_sdk::task::SharedProgress;
use aether_plugin_sdk::{
    Checksum, ChecksumAlgorithm, FileInfo, PluginEvent, PluginSender, TaskSpec,
};

/// ffmpeg reports progress faster than the UI needs it.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// ffmpeg binary for a task: the `ffmpeg_path` task setting wins, then the
/// `AETHER_FFMPEG` environment, then plain `ffmpeg` on PATH — the same
/// resolution order the rtmp plugin uses.
pub fn executable(task: &TaskSpec) -> String {
    task.settings
        .get("ffmpeg_path")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .filter(|path| !path.trim().is_empty())
        .or_else(|| std::env::var("AETHER_FFMPEG").ok())
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "ffmpeg".into())
}

/// The requested output container, normalised to a bare extension-like
/// name. `None` (or blank input) means "no transcode".
pub fn target_container(task: &TaskSpec) -> Option<String> {
    let raw = task.settings.get("transcode_to")?.as_str()?;
    let name = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    (!name.is_empty() && name.len() <= 8 && name.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .then_some(name)
}

/// Remux the finished download when the task asked for it.
///
/// Returns the file the task should complete with (new name after a
/// successful remux, the input unchanged otherwise) and an optional note
/// explaining why the transcode did not happen. Never fails the task:
/// a missing/broken ffmpeg leaves the original file in place.
pub fn maybe_remux(
    sender: &PluginSender,
    task: &TaskSpec,
    staging: &Path,
    file: &FileInfo,
    progress: &SharedProgress,
) -> (FileInfo, Option<String>) {
    let Some(container) = target_container(task) else {
        return (file.clone(), None);
    };
    match remux(sender, task, staging, file, &container, progress) {
        Ok(remuxed) => (remuxed, None),
        Err(note) => (file.clone(), Some(note)),
    }
}

fn remux(
    sender: &PluginSender,
    task: &TaskSpec,
    staging: &Path,
    file: &FileInfo,
    container: &str,
    progress: &SharedProgress,
) -> Result<FileInfo, String> {
    let input = staging.join(&file.relative_path);
    let stem = Path::new(&file.relative_path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.relative_path.clone());
    let output_name = format!("{stem}.{container}");
    let output = staging.join(&output_name);
    let executable = executable(task);

    let args = [
        "-nostdin",
        "-y",
        "-i",
        &input.to_string_lossy(),
        "-c",
        "copy",
        // `-f` keeps names ffmpeg cannot infer working; every container
        // this plugin accepts has a matching muxer.
        "-f",
        container,
        "-progress",
        "pipe:1",
        "-loglevel",
        "error",
        &output.to_string_lossy(),
    ];
    let mut child = spawn_ffmpeg(&executable, &args)?;

    // Progress arrives on stdout as `key=value` lines; stderr stays quiet
    // at `-loglevel error` and is only read once the process exits.
    if let Some(stdout) = child.stdout.take() {
        let task_id = task.task_id.clone();
        let sender = sender.clone();
        thread::spawn(move || forward_ffmpeg_progress(sender, task_id, stdout));
    }

    let mut stderr = child.stderr.take().map(BufReader::new);
    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for {executable}: {error}"))?;
    let detail = read_stderr_tail(stderr.as_mut());

    if !status.success() {
        let _ = fs::remove_file(&output);
        let kept = format!("kept the original {}", file.relative_path);
        return Err(match detail {
            Some(detail) => format!("ffmpeg exited with {status}: {detail}; {kept}"),
            None => format!("ffmpeg exited with {status}; {kept}"),
        });
    }
    if !output.is_file() {
        return Err(format!(
            "ffmpeg reported success but produced no {container} output; kept the original file"
        ));
    }

    // The remux replaced the container: drop the original so the task
    // finalizes one file, not two.
    fs::remove_file(&input)
        .map_err(|error| format!("cannot remove {}: {error}", file.relative_path))?;

    let size = fs::metadata(&output)
        .map_err(|error| format!("stat failed: {error}"))?
        .len();
    let checksum =
        aether_plugin_sdk::checksum::sha256_file(&output).map_err(|error| error.to_string())?;
    Ok(FileInfo {
        relative_path: output_name,
        size,
        checksum: Some(Checksum {
            algorithm: ChecksumAlgorithm::Sha256,
            value: checksum,
        }),
    })
}

/// Spawn ffmpeg, retrying once on `ETXTBSY`: on Linux/macOS that errno means
/// the binary is momentarily open for writing (an in-place update, a just
/// written test fixture). The writer always closes, so a single bounded
/// retry turns a spurious "cannot start" into a normal run instead of
/// throwing away the download over a transient exec condition.
fn spawn_ffmpeg(executable: &str, args: &[&str]) -> Result<std::process::Child, String> {
    let spawn = || {
        Command::new(executable)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let mut last = None;
    for attempt in 0..3 {
        match spawn() {
            Ok(child) => return Ok(child),
            // ETXTBSY (26) only: every other errno is a real failure.
            Err(error) if error.raw_os_error() == Some(26) => {
                last = Some(error);
                if attempt < 2 {
                    thread::sleep(Duration::from_millis(250));
                }
            }
            Err(error) => return Err(format!("cannot start {executable}: {error}")),
        }
    }
    Err(format!(
        "cannot start {executable}: {}",
        last.expect("ETXTBSY loop ran at least once")
    ))
}

/// Parse ffmpeg `-progress pipe:1` output into progress events. `out_time`
/// is a timestamp, so `downloaded` carries milliseconds of media processed
/// (the UI shows it as a moving counter; there is no byte total to map).
fn forward_ffmpeg_progress(sender: PluginSender, task_id: String, stdout: impl std::io::Read) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut seq = 0_u64;
    let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        match key {
            // Historic name: this value is microseconds on every ffmpeg
            // release that matters here.
            "out_time_us" | "out_time_ms" => {
                let Ok(micros) = value.parse::<u64>() else {
                    continue;
                };
                if last_emit.elapsed() < PROGRESS_INTERVAL {
                    continue;
                }
                seq += 1;
                let _ = sender.send(&PluginEvent::Progress {
                    task_id: task_id.clone(),
                    seq,
                    downloaded: micros / 1000,
                    total: None,
                    speed_bps: 0,
                    eta_secs: None,
                    connections: 1,
                    chunks_done: None,
                    chunks_total: None,
                });
                last_emit = Instant::now();
            }
            "progress" if value.trim() == "end" => break,
            _ => {}
        }
    }
}

fn read_stderr_tail(stderr: Option<&mut BufReader<std::process::ChildStderr>>) -> Option<String> {
    let stderr = stderr?;
    let mut tail = String::new();
    let mut line = String::new();
    while let Ok(n) = stderr.read_line(&mut line) {
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            tail = trimmed.to_owned();
        }
        line.clear();
    }
    (!tail.is_empty()).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task_with(settings: serde_json::Value) -> TaskSpec {
        TaskSpec {
            task_id: "task-1".into(),
            url: "https://example.test/list.m3u8".into(),
            filename: None,
            settings,
            plugin_state: None,
            expected_size: None,
            checksum: None,
            proxy: None,
        }
    }

    #[test]
    fn container_names_are_normalised() {
        assert_eq!(
            target_container(&task_with(json!({"transcode_to": "MP4"}))),
            Some("mp4".into())
        );
        assert_eq!(
            target_container(&task_with(json!({"transcode_to": ".mkv"}))),
            Some("mkv".into())
        );
        // Anything that is not a plain container name is ignored rather than
        // passed into a shell-like context.
        assert_eq!(
            target_container(&task_with(json!({"transcode_to": "../evil;rm"}))),
            None
        );
        assert_eq!(
            target_container(&task_with(json!({"transcode_to": ""}))),
            None
        );
        assert_eq!(target_container(&task_with(json!({}))), None);
    }

    #[test]
    fn ffmpeg_path_setting_wins_over_environment() {
        std::env::set_var("AETHER_FFMPEG", "/env/ffmpeg");
        assert_eq!(
            executable(&task_with(json!({"ffmpeg_path": "/cfg/ffmpeg"}))),
            "/cfg/ffmpeg"
        );
        assert_eq!(executable(&task_with(json!({}))), "/env/ffmpeg");
        std::env::remove_var("AETHER_FFMPEG");
        assert_eq!(executable(&task_with(json!({}))), "ffmpeg");
    }
}
