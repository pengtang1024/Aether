//! `aether-cli logs` reads the task log table the daemon writes.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_host::{NewTask, TaskState, TaskStore};
use serde_json::json;

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-cli-logs-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn seed(dir: &PathBuf) {
    let mut store = TaskStore::open(dir.join(".aether").join("aether.db")).unwrap();
    store
        .create_task(&NewTask {
            id: "t1".into(),
            url: "https://example.com/a.bin".into(),
            plugin_id: "fake".into(),
            filename: Some("a.bin".into()),
            settings: json!({}),
            state: TaskState::Queued,
        })
        .unwrap();
    store.append_log("t1", "info", "first line", None).unwrap();
    store
        .append_log("t1", "error", "boom", Some(&json!({"attempt": 2})))
        .unwrap();
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aether-cli"))
}

#[test]
fn prints_logs_oldest_first() {
    let root = temp_dir("order");
    seed(&root);

    let output = cli()
        .args(["logs", "t1", "--dir"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "unexpected output: {stdout:?}");
    // `HH:MM:SSZ level message`
    assert!(lines[0].ends_with("first line"), "{}", lines[0]);
    assert!(lines[0].contains(" info "), "{}", lines[0]);
    assert!(lines[1].ends_with("boom"), "{}", lines[1]);
    assert!(
        lines[0].chars().take(2).all(|c| c.is_ascii_digit()) && lines[0].contains('Z'),
        "missing timestamp: {}",
        lines[0]
    );

    // --limit keeps the newest lines only.
    let output = cli()
        .args(["logs", "t1", "--limit", "1", "--dir"])
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(stdout.contains("boom"));

    fs::remove_dir_all(&root).ok();
}

#[test]
fn unknown_task_is_an_error() {
    let root = temp_dir("missing");
    seed(&root);

    let output = cli()
        .args(["logs", "task-nope", "--dir"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "stderr: {stderr}");

    fs::remove_dir_all(&root).ok();
}
