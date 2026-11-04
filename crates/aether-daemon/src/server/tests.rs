use crate::server::*;

use serde_json::json;
use std::fs;

#[test]
fn staging_falls_back_when_the_preferred_path_is_blocked() {
    let root = std::env::temp_dir().join(format!(
        "aether-staging-fallback-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, b"x").unwrap();
    let preferred = blocker.join("staging");
    let fallback = root.join("fallback").join("staging");
    let chosen = super::create_dir_with_fallback(&preferred, &fallback).unwrap();
    assert_eq!(chosen, fallback);
    assert!(fallback.is_dir());
    fs::remove_dir_all(root).ok();
}

#[test]
fn splits_the_global_limit_without_overshooting() {
    let tasks = vec![("a".to_string(), None), ("b".to_string(), None)];
    let shares = super::distribute_budget(Some(1000), &tasks);
    assert_eq!(shares["a"] + shares["b"], 1000);

    // An explicit limit below its fair share keeps it and frees the rest.
    let tasks = vec![("a".to_string(), Some(100)), ("b".to_string(), None)];
    let shares = super::distribute_budget(Some(1000), &tasks);
    assert_eq!(shares["a"], 100);
    assert_eq!(shares["b"], 900);

    // An explicit limit above its fair share is capped, so the sum holds.
    let tasks = vec![("a".to_string(), Some(600)), ("b".to_string(), Some(600))];
    let shares = super::distribute_budget(Some(1000), &tasks);
    assert_eq!(shares["a"] + shares["b"], 1000);

    // Without a global limit only explicit per-task limits apply.
    let shares = super::distribute_budget(None, &[("a".to_string(), Some(50))]);
    assert_eq!(shares["a"], 50);
    let shares = super::distribute_budget(None, &[("a".to_string(), None)]);
    assert_eq!(shares["a"], 0);
}

#[test]
fn parses_task_checksums() {
    let hex = "a".repeat(64);
    let checksum = parse_checksum(&json!({ "checksum": format!("sha-256={hex}") }))
        .unwrap()
        .unwrap();
    assert_eq!(checksum.value, hex);

    let object = json!({ "checksum": { "algorithm": "SHA256", "value": hex } });
    assert!(parse_checksum(&object).unwrap().is_some());
    assert!(parse_checksum(&json!({ "checksum": "md5=abc" })).is_err());
    assert!(parse_checksum(&json!({ "checksum": "sha256=short" })).is_err());
    assert!(parse_checksum(&json!({})).unwrap().is_none());
}

#[test]
fn schedule_window_decides_by_clock_time() {
    let window = (Some(true), Some("01:00"), Some("07:00"));

    // Inside the window.
    assert!(schedule_active(window.0, window.1, window.2, 60));
    assert!(schedule_active(window.0, window.1, window.2, 6 * 60 + 59));
    // Outside the window.
    assert!(!schedule_active(window.0, window.1, window.2, 0));
    assert!(!schedule_active(window.0, window.1, window.2, 7 * 60));
    assert!(!schedule_active(window.0, window.1, window.2, 12 * 60));

    // Overnight windows (end < start) wrap midnight.
    assert!(schedule_active(
        Some(true),
        Some("22:00"),
        Some("07:00"),
        23 * 60
    ));
    assert!(schedule_active(
        Some(true),
        Some("22:00"),
        Some("07:00"),
        2 * 60
    ));
    assert!(!schedule_active(
        Some(true),
        Some("22:00"),
        Some("07:00"),
        12 * 60
    ));

    // Disabled or incomplete schedules never stall the queue.
    assert!(schedule_active(None, Some("01:00"), Some("07:00"), 12 * 60));
    assert!(schedule_active(
        Some(false),
        Some("01:00"),
        Some("07:00"),
        12 * 60
    ));
    assert!(schedule_active(Some(true), Some("01:00"), None, 12 * 60));
    assert!(schedule_active(
        Some(true),
        Some("bad"),
        Some("07:00"),
        12 * 60
    ));
}

#[test]
fn task_connections_override_the_daemon_default() {
    // A task-level setting wins over the settings-page default.
    assert_eq!(super::task_connections(&json!({"connections": 8}), 4), 8);
    // Without it, the daemon-wide value applies.
    assert_eq!(super::task_connections(&json!({}), 4), 4);
    // Values the engine clamps are clamped here too, never widened.
    assert_eq!(super::task_connections(&json!({"connections": 99}), 4), 16);
    assert_eq!(super::task_connections(&json!({"connections": 0}), 4), 1);
}

#[test]
fn out_of_range_task_connections_are_rejected_at_add_time() {
    assert!(super::validate_task_connections(&json!({"connections": 8})).is_ok());
    assert!(super::validate_task_connections(&json!({})).is_ok());
    assert!(super::validate_task_connections(&json!({"connections": 0})).is_err());
    assert!(super::validate_task_connections(&json!({"connections": 17})).is_err());
    assert!(super::validate_task_connections(&json!({"connections": "8"})).is_err());
}
