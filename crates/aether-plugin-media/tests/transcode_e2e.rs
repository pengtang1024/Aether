//! Fake-ffmpeg remux end-to-end (ROADMAP 4.4 acceptance): the argv is
//! recorded, exit 0 completes the task with the transcoded file, exit 1
//! keeps the original download and reports why.
//!
//! Unix-only because the fake ffmpeg is a shell script.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aether_plugin_media::transcode;
use aether_plugin_sdk::{FileInfo, PluginSender, TaskSpec};
use serde_json::json;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-transcode-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake ffmpeg: records its argv, then either copies input→output and
/// exits 0, or prints a progress stream and exits 1.
fn fake_ffmpeg(dir: &Path, behavior: &str) -> PathBuf {
    let argv_log = dir.join("argv.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{argv}\"\n\
         input=\"\"\nprev=\"\"\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-i\" ]; then input=\"$arg\"; fi\n  prev=\"$arg\"\ndone\n\
         output=\"$arg\"\n\
         {behavior}\n",
        argv = argv_log.display(),
    );
    let path = dir.join("fake-ffmpeg");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn task(settings: serde_json::Value) -> TaskSpec {
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

fn downloaded_file(dir: &Path, name: &str) -> FileInfo {
    fs::write(dir.join(name), b"fake media bytes").unwrap();
    FileInfo {
        relative_path: name.into(),
        size: 16,
        checksum: None,
    }
}

fn sender() -> PluginSender {
    PluginSender::new(Box::new(Vec::<u8>::new()))
}

#[test]
fn exit_zero_completes_with_the_transcoded_file() {
    let dir = temp_dir("ok");
    let ffmpeg = fake_ffmpeg(
        &dir,
        "printf 'out_time_us=1500000\\nprogress=end\\n'\ncp \"$input\" \"$output\"\nexit 0",
    );
    let file = downloaded_file(&dir, "media.ts");
    let task = task(json!({"transcode_to": "mp4", "ffmpeg_path": ffmpeg.display().to_string()}));

    let (finished, note) = transcode::maybe_remux(
        &sender(),
        &task,
        &dir,
        &file,
        &aether_plugin_sdk::task::shared_progress(),
    );

    assert_eq!(note, None, "successful remux must not carry a note");
    assert_eq!(finished.relative_path, "media.mp4");
    assert_eq!(
        fs::read(dir.join("media.mp4")).unwrap(),
        b"fake media bytes"
    );
    // The original container file is replaced, not duplicated.
    assert!(!dir.join("media.ts").exists());

    // The argv proves remux semantics: -c copy, the real paths, progress on.
    let argv = fs::read_to_string(dir.join("argv.log")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert!(
        lines.windows(2).any(|pair| pair == ["-c", "copy"]),
        "{argv}"
    );
    assert!(
        lines.windows(2).any(|pair| pair == ["-progress", "pipe:1"]),
        "{argv}"
    );
    assert_eq!(
        lines[lines.len() - 1],
        dir.join("media.mp4").to_string_lossy()
    );
    assert!(
        lines.contains(&dir.join("media.ts").to_string_lossy().as_ref()),
        "{argv}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn exit_one_keeps_the_original_and_explains() {
    let dir = temp_dir("fail");
    let ffmpeg = fake_ffmpeg(
        &dir,
        "printf 'out_time_us=2000000\\nprogress=end\\n'\necho 'Invalid data found when processing input' >&2\nrm -f \"$output\"\nexit 1",
    );
    let file = downloaded_file(&dir, "media.ts");
    let task = task(json!({"transcode_to": "mp4", "ffmpeg_path": ffmpeg.display().to_string()}));

    let (finished, note) = transcode::maybe_remux(
        &sender(),
        &task,
        &dir,
        &file,
        &aether_plugin_sdk::task::shared_progress(),
    );

    // The task still completes with the original download; the note names
    // the reason and lands in plugin_state for the task detail page.
    assert_eq!(finished.relative_path, "media.ts");
    assert!(
        dir.join("media.ts").is_file(),
        "original must survive a failed remux"
    );
    assert!(!dir.join("media.mp4").exists(), "no half-written output");
    let note = note.expect("a failed remux must explain itself");
    assert!(note.contains("ffmpeg exited"), "{note}");
    assert!(
        note.contains("Invalid data"),
        "stderr must be surfaced: {note}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_binary_keeps_the_original() {
    let dir = temp_dir("missing");
    let file = downloaded_file(&dir, "media.ts");
    let task = task(json!({"transcode_to": "mkv", "ffmpeg_path": "/nonexistent/ffmpeg"}));

    let (finished, note) = transcode::maybe_remux(
        &sender(),
        &task,
        &dir,
        &file,
        &aether_plugin_sdk::task::shared_progress(),
    );

    assert_eq!(finished.relative_path, "media.ts");
    assert!(dir.join("media.ts").is_file());
    let note = note.expect("missing ffmpeg must be reported");
    assert!(note.contains("cannot start"), "{note}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn without_transcode_to_the_download_is_untouched() {
    let dir = temp_dir("plain");
    let file = downloaded_file(&dir, "media.ts");

    let (finished, note) = transcode::maybe_remux(
        &sender(),
        &task(json!({})),
        &dir,
        &file,
        &aether_plugin_sdk::task::shared_progress(),
    );

    assert_eq!(finished.relative_path, "media.ts");
    assert_eq!(note, None);
    assert!(dir.join("media.ts").is_file());
    // No ffmpeg process was ever needed, so no argv log exists.
    assert!(!dir.join("argv.log").exists());
    fs::remove_dir_all(&dir).ok();
}
