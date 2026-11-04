use crate::server::*;

pub(crate) fn stats_request(state: &mut State, _request: Request) -> Result<Value, DaemonError> {
    let now = chrono::Local::now();
    let today_start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|naive| {
            naive
                .and_local_timezone(chrono::Local)
                .single()
                .map(|local| local.timestamp_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let week_start = today_start - 7 * 24 * 3600 * 1000;
    let (today_bytes, today_files) = state.store.completed_since(today_start)?;
    let (week_bytes, week_files) = state.store.completed_since(week_start)?;
    // Per-day bars and per-type split come from one file-level query.
    let files = state.store.completed_files_since(week_start)?;
    let tz_offset = chrono::Local::now().offset().local_minus_utc() as i64 * 1000;
    let day_ms = 24 * 3600 * 1000;
    let today_index = ((today_start + tz_offset) / day_ms) as usize;
    let first_index = today_index - 6; // slots: 6 days ago … today
    let mut daily = [0_u64; 7];
    let mut categories = [0_u64; 5];
    for (completed_at, size, relative_path) in &files {
        let day = ((*completed_at + tz_offset) / day_ms) as usize;
        if day >= first_index {
            let slot = (day - first_index).min(6);
            daily[slot] += size;
        }
        categories[file_category(relative_path)] += size;
    }
    // Weekday glyphs for the daily slots, today last.
    let day_labels: Vec<String> = {
        let glyphs = ["一", "二", "三", "四", "五", "六", "日"];
        let today = chrono::Datelike::weekday(&chrono::Local::now()).num_days_from_monday() as i64;
        (0..7)
            .map(|back| {
                let weekday = (today - (6 - back)).rem_euclid(7) as usize;
                glyphs[weekday].to_string()
            })
            .collect()
    };
    let (completed, failed) = state.store.outcome_counts()?;
    let (disk_total, disk_free) = match (
        fs2::total_space(&state.download_dir),
        fs2::available_space(&state.download_dir),
    ) {
        (Ok(total), Ok(free)) => (total, free),
        _ => (0, 0),
    };
    let queued = state
        .store
        .list_tasks(1000)?
        .into_iter()
        .filter(|task| task.state == TaskState::Queued)
        .count() as u64;
    let registry = state.manager.registry();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "download_dir": state.download_dir.to_string_lossy(),
        "plugins_dir": state.plugins_dir.to_string_lossy(),
        "data_dir": state.data_dir.join("aether.db").to_string_lossy(),
        "disk": { "total": disk_total, "free": disk_free },
        "today": { "bytes": today_bytes, "files": today_files },
        "week": { "bytes": week_bytes, "files": week_files },
        "daily": daily,
        "daily_labels": day_labels,
        "categories": {
            "video": categories[0],
            "archive": categories[1],
            "app": categories[2],
            "doc": categories[3],
            "other": categories[4],
        },
        "tasks": {
            "completed": completed,
            "failed": failed,
            "active": state.active.len(),
            "queued": queued,
        },
        "plugins": {
            "total": registry.len(),
            "enabled": registry.iter().filter(|entry| entry.enabled).count(),
        },
        // App-update status, so the About panel shows the same state
        // the background loop is in. `null` = no update source.
        "update": state.update.as_ref().map(|status| {
            status.lock().unwrap().clone()
        }),
        // Plugin update/install progress (`{"running": false}` when
        // idle), so the Plugins panel can show a bar.
        "plugin_progress": state.plugin_progress.lock().unwrap().clone(),
    }))
}

pub(crate) fn backup_request(state: &mut State, request: Request) -> Result<Value, DaemonError> {
    let path = request
        .settings
        .get("path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| DaemonError::Protocol("backup needs a path".into()))?;
    state.store.backup_to(path)?;
    Ok(json!({ "path": path }))
}

pub(crate) fn update_check_request(
    state: &mut State,
    _request: Request,
) -> Result<Value, DaemonError> {
    let (Some(status), Some(url), Some(key)) = (
        state.update.clone(),
        state.update_url.clone(),
        state.plugin_public_key.clone(),
    ) else {
        return Ok(json!({"status": "disabled"}));
    };
    *status.lock().unwrap() = json!({"status": "checking"});
    let appimage = state.update_appimage.clone();
    let data_dir = state.data_dir.clone();
    let settings_path = state.settings_path.clone();
    thread::spawn(move || {
        update_pass(
            &url,
            &key,
            appimage.as_deref(),
            &data_dir,
            &settings_path,
            &status,
        );
    });
    Ok(json!({"status": "checking"}))
}
