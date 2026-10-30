//! Client-visible data types that do not depend on the Unix socket.
//!
//! Shared by the daemon client, the CLI and the Slint UI bridge — including
//! on platforms where the socket layer itself does not exist yet.

use serde::Deserialize;
use serde_json::Value;

/// One installed plugin as the daemon registry reports it.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// Dashboard counters behind the daemon's `stats` method.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    /// Completed bytes per day, oldest (7 days ago) first, today last.
    pub daily: [u64; 7],
    /// Weekday glyphs for the daily slots ("一"…"日"), oldest first.
    pub daily_labels: Vec<String>,
    /// Finalized bytes by type: video, archive, app, doc, other.
    pub categories: [u64; 5],
    pub version: String,
    pub download_dir: String,
    pub plugins_dir: String,
    pub data_dir: String,
    pub disk_total: u64,
    pub disk_free: u64,
    pub today_bytes: u64,
    pub today_files: u64,
    pub week_bytes: u64,
    pub week_files: u64,
    pub completed: u64,
    pub failed: u64,
    pub active: u64,
    pub queued: u64,
    pub plugins_total: u64,
    pub plugins_enabled: u64,
    /// App-update status from the daemon's periodic check (`null` when no
    /// update source is configured).
    pub update: Option<Value>,
}

impl Stats {
    /// Flatten the daemon's nested `stats` JSON.
    pub fn from_json(value: &Value) -> Self {
        let num =
            |value: &Value, key: &str| value.get(key).and_then(Value::as_u64).unwrap_or_default();
        let group = |key: &str| value.get(key).cloned().unwrap_or(Value::Null);
        let disk = group("disk");
        let today = group("today");
        let week = group("week");
        let tasks = group("tasks");
        let plugins = group("plugins");
        let daily = {
            let mut slots = [0_u64; 7];
            if let Some(values) = value.get("daily").and_then(Value::as_array) {
                for (slot, item) in values.iter().take(7).enumerate() {
                    slots[slot] = item.as_u64().unwrap_or_default();
                }
            }
            slots
        };
        let cats = value.get("categories").cloned().unwrap_or(Value::Null);
        let pick = |key: &str| cats.get(key).and_then(Value::as_u64).unwrap_or_default();
        let categories = [
            pick("video"),
            pick("archive"),
            pick("app"),
            pick("doc"),
            pick("other"),
        ];
        let daily_labels = value
            .get("daily_labels")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            daily,
            categories,
            daily_labels,
            version: value
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            download_dir: value
                .get("download_dir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            plugins_dir: value
                .get("plugins_dir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            data_dir: value
                .get("data_dir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            disk_total: num(&disk, "total"),
            disk_free: num(&disk, "free"),
            today_bytes: num(&today, "bytes"),
            today_files: num(&today, "files"),
            week_bytes: num(&week, "bytes"),
            week_files: num(&week, "files"),
            completed: num(&tasks, "completed"),
            failed: num(&tasks, "failed"),
            active: num(&tasks, "active"),
            queued: num(&tasks, "queued"),
            plugins_total: num(&plugins, "total"),
            plugins_enabled: num(&plugins, "enabled"),
            update: value
                .get("update")
                .filter(|update| !update.is_null())
                .cloned(),
        }
    }
}
