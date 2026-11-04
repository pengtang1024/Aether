use super::needs_repaint;
use crate::backend::Snapshot;
use crate::model::{Status, Task, CHUNK_COUNT};

fn task(speed: f64, status: Status) -> Task {
    Task {
        id: 1,
        name: "test.zip".into(),
        source: "http".into(),
        path: String::new(),
        note: String::new(),
        total: 1024,
        done: 100.0,
        status,
        plugin_id: "http".into(),
        torrent: false,
        speed,
        history: Default::default(),
        order: [0; CHUNK_COUNT],
        time_ms: 0,
    }
}

fn snapshot(speed: f64, status: Status) -> Snapshot {
    Snapshot {
        tasks: vec![task(speed, status)],
        ..Snapshot::default()
    }
}

#[test]
fn first_paint_always_happens() {
    assert!(needs_repaint(None, &Snapshot::default(), 0));
}

#[test]
fn unchanged_data_skips_the_repaint() {
    let snap = snapshot(10.0, Status::Downloading);
    let painted = (snap.clone(), 3);
    assert!(!needs_repaint(Some(&painted), &snap, 3));
}

#[test]
fn a_new_minute_repaints_for_the_uptime_label() {
    let snap = snapshot(10.0, Status::Downloading);
    let painted = (snap.clone(), 3);
    assert!(needs_repaint(Some(&painted), &snap, 4));
}

#[test]
fn a_moving_speed_repaints() {
    let painted = (snapshot(10.0, Status::Downloading), 3);
    assert!(needs_repaint(
        Some(&painted),
        &snapshot(11.0, Status::Downloading),
        3
    ));
}

#[test]
fn a_status_change_repaints() {
    let painted = (snapshot(10.0, Status::Downloading), 3);
    assert!(needs_repaint(
        Some(&painted),
        &snapshot(10.0, Status::Completed),
        3
    ));
}
