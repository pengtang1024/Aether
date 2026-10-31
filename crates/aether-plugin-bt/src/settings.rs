//! Parsing for the task settings this plugin owns.
//!
//! `bt_files` selects which torrent files to download (0-based indices,
//! relative paths, or a mix), `bt_seed_ratio` / `bt_seed_minutes` control the
//! optional seeding phase after the download finished.

use std::net::SocketAddr;
use std::path::{Component, Path};
use std::time::Duration;

use serde_json::Value;

/// One entry of `bt_files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSelector {
    /// 0-based index into the torrent's file list.
    Index(usize),
    /// Path relative to the torrent root, always slash-normalised.
    Path(String),
}

/// A torrent file as reported by torrent metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    pub path: String,
    pub len: u64,
}

/// Settings that influence a single BT task.
#[derive(Debug, Clone, PartialEq)]
pub struct BtSettings {
    /// `None` keeps the historical behaviour: download every file.
    pub files: Option<Vec<FileSelector>>,
    /// Seed until the upload/download ratio reaches this value. 0 disables.
    pub seed_ratio: f64,
    /// Seed for at most this many minutes. 0 disables the duration limit.
    pub seed_minutes: u64,
    pub initial_peers: Option<Vec<SocketAddr>>,
}

impl BtSettings {
    pub fn from_value(settings: &Value) -> Result<Self, String> {
        Ok(Self {
            files: parse_files(settings.get("bt_files"))?,
            seed_ratio: parse_ratio(settings.get("bt_seed_ratio"))?,
            seed_minutes: parse_minutes(settings.get("bt_seed_minutes"))?,
            initial_peers: initial_peers(settings),
        })
    }

    /// True when the task must keep seeding after the download completed.
    pub fn seeds(&self) -> bool {
        self.seed_ratio > 0.0 || self.seed_minutes > 0
    }

    /// Duration cap for the seeding phase, if one is configured.
    pub fn seed_duration(&self) -> Option<Duration> {
        (self.seed_minutes > 0).then(|| Duration::from_secs(self.seed_minutes.saturating_mul(60)))
    }
}

/// Map selectors onto torrent file indices.
///
/// Duplicates are collapsed and the result is sorted. Missing paths and
/// out-of-range indices are reported together so the user sees every mistake
/// at once.
pub fn resolve_files(
    files: &[TorrentFile],
    selectors: &[FileSelector],
) -> Result<Vec<usize>, String> {
    let mut indices = Vec::new();
    let mut missing = Vec::new();

    for selector in selectors {
        match selector {
            FileSelector::Index(index) => {
                if *index >= files.len() {
                    missing.push(format!("index {index}"));
                } else if !indices.contains(index) {
                    indices.push(*index);
                }
            }
            FileSelector::Path(path) => match files.iter().position(|file| file.path == *path) {
                Some(index) => {
                    if !indices.contains(&index) {
                        indices.push(index);
                    }
                }
                None => missing.push(path.clone()),
            },
        }
    }

    if !missing.is_empty() {
        return Err(format!(
            "bt_files: file(s) not found in torrent: {}",
            missing.join(", ")
        ));
    }

    indices.sort_unstable();
    Ok(indices)
}

fn parse_files(value: Option<&Value>) -> Result<Option<Vec<FileSelector>>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let mut selectors = Vec::new();
            for entry in text.split([',', ';']) {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                selectors.push(parse_text_entry(entry)?);
            }
            Ok((!selectors.is_empty()).then_some(selectors))
        }
        Some(Value::Array(items)) => {
            let mut selectors = Vec::new();
            for item in items {
                selectors.push(parse_array_entry(item)?);
            }
            Ok((!selectors.is_empty()).then_some(selectors))
        }
        Some(other) => Err(format!(
            "bt_files must be an array or a string, got {other}"
        )),
    }
}

fn parse_array_entry(value: &Value) -> Result<FileSelector, String> {
    match value {
        Value::Number(number) => {
            let index = number
                .as_u64()
                .ok_or_else(|| format!("bt_files index {number} is not a non-negative integer"))?;
            let index = usize::try_from(index)
                .map_err(|_| format!("bt_files index {index} does not fit in usize"))?;
            Ok(FileSelector::Index(index))
        }
        Value::String(text) => parse_path(text),
        other => Err(format!(
            "bt_files entries must be indices or paths, got {other}"
        )),
    }
}

fn parse_text_entry(entry: &str) -> Result<FileSelector, String> {
    if let Ok(index) = entry.parse::<usize>() {
        return Ok(FileSelector::Index(index));
    }
    if entry.parse::<i64>().is_ok() {
        return Err(format!(
            "bt_files index {entry} is not a non-negative integer"
        ));
    }
    parse_path(entry)
}

fn parse_path(text: &str) -> Result<FileSelector, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("bt_files path entries must not be empty".into());
    }

    let normalised = text.replace('\\', "/");
    let normalised = normalised
        .strip_prefix("./")
        .unwrap_or(&normalised)
        .to_string();
    let path = Path::new(&normalised);
    if path.is_absolute() {
        return Err(format!(
            "bt_files path {normalised:?} must be relative to the torrent"
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("bt_files path {normalised:?} must not contain .."));
    }

    Ok(FileSelector::Path(normalised))
}

fn parse_ratio(value: Option<&Value>) -> Result<f64, String> {
    let Some(value) = value else {
        return Ok(0.0);
    };
    let ratio = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("bt_seed_ratio must be a number, got {value}"))?;

    if !ratio.is_finite() || ratio < 0.0 {
        return Err(format!(
            "bt_seed_ratio must be a finite non-negative number, got {value}"
        ));
    }
    Ok(ratio)
}

fn parse_minutes(value: Option<&Value>) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(0);
    };
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| format!("bt_seed_minutes must be a non-negative integer, got {value}")),
        Value::String(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("bt_seed_minutes must be a non-negative integer, got {value}")),
        _ => Err(format!(
            "bt_seed_minutes must be a non-negative integer, got {value}"
        )),
    }
}

fn initial_peers(settings: &Value) -> Option<Vec<SocketAddr>> {
    let values = settings.get("initial_peers")?.as_array()?;
    let peers = values
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(|value| value.parse().ok())
        .collect::<Vec<_>>();
    (!peers.is_empty()).then_some(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files() -> Vec<TorrentFile> {
        vec![
            TorrentFile {
                path: "album/a.flac".into(),
                len: 10,
            },
            TorrentFile {
                path: "album/b.flac".into(),
                len: 20,
            },
            TorrentFile {
                path: "cover.jpg".into(),
                len: 5,
            },
        ]
    }

    #[test]
    fn absent_settings_keep_defaults() {
        let settings = BtSettings::from_value(&json!({})).unwrap();
        assert_eq!(settings.files, None);
        assert_eq!(settings.seed_ratio, 0.0);
        assert_eq!(settings.seed_minutes, 0);
        assert!(!settings.seeds());
        assert_eq!(settings.seed_duration(), None);

        let null_files = BtSettings::from_value(&json!({ "bt_files": null })).unwrap();
        assert_eq!(null_files.files, None);
    }

    #[test]
    fn parses_index_and_path_selectors() {
        let settings = BtSettings::from_value(&json!({
            "bt_files": [0, "album/b.flac", 2]
        }))
        .unwrap();
        assert_eq!(
            settings.files.unwrap(),
            vec![
                FileSelector::Index(0),
                FileSelector::Path("album/b.flac".into()),
                FileSelector::Index(2),
            ]
        );
    }

    #[test]
    fn parses_comma_and_semicolon_lists() {
        let settings =
            BtSettings::from_value(&json!({ "bt_files": "0; album/b.flac, cover.jpg" })).unwrap();
        assert_eq!(
            settings.files.unwrap(),
            vec![
                FileSelector::Index(0),
                FileSelector::Path("album/b.flac".into()),
                FileSelector::Path("cover.jpg".into()),
            ]
        );

        // Empty entries are ignored; a fully empty list means "all files".
        let settings = BtSettings::from_value(&json!({ "bt_files": " ; , " })).unwrap();
        assert_eq!(settings.files, None);
        let settings = BtSettings::from_value(&json!({ "bt_files": [] })).unwrap();
        assert_eq!(settings.files, None);
    }

    #[test]
    fn normalises_backslashes_and_dot_prefix() {
        let settings = BtSettings::from_value(&json!({ "bt_files": ".\\album\\a.flac" })).unwrap();
        assert_eq!(
            settings.files.unwrap(),
            vec![FileSelector::Path("album/a.flac".into())]
        );
    }

    #[test]
    fn rejects_invalid_file_selectors() {
        for value in [
            json!(true),
            json!({}),
            json!([1.5]),
            json!([true]),
            json!([{}]),
            json!([-1]),
            json!([""]),
            json!(["/absolute.bin"]),
            json!(["../outside.bin"]),
            json!("-1"),
            json!("album/../escape.bin"),
        ] {
            let settings = json!({ "bt_files": value });
            assert!(
                BtSettings::from_value(&settings).is_err(),
                "expected {value} to be rejected"
            );
        }
    }

    #[test]
    fn resolves_selectors_to_sorted_unique_indices() {
        let files = files();
        assert_eq!(
            resolve_files(&files, &[FileSelector::Index(2), FileSelector::Index(0)]).unwrap(),
            vec![0, 2]
        );
        assert_eq!(
            resolve_files(
                &files,
                &[
                    FileSelector::Path("album/b.flac".into()),
                    FileSelector::Index(1),
                ]
            )
            .unwrap(),
            vec![1]
        );
        assert_eq!(
            resolve_files(&files, &[FileSelector::Path("cover.jpg".into())]).unwrap(),
            vec![2]
        );
    }

    #[test]
    fn reports_every_missing_path() {
        let files = files();
        let error = resolve_files(
            &files,
            &[
                FileSelector::Path("nope/a.bin".into()),
                FileSelector::Path("nope/b.bin".into()),
                FileSelector::Index(9),
            ],
        )
        .unwrap_err();
        assert!(error.contains("nope/a.bin"), "{error}");
        assert!(error.contains("nope/b.bin"), "{error}");
        assert!(error.contains("index 9"), "{error}");
    }

    #[test]
    fn validates_seed_ratio() {
        for value in [json!(0.5), json!(2), json!("0.25"), json!(0)] {
            let settings = json!({ "bt_seed_ratio": value });
            assert!(BtSettings::from_value(&settings).is_ok(), "{value}");
        }
        assert_eq!(
            BtSettings::from_value(&json!({ "bt_seed_ratio": "0.25" }))
                .unwrap()
                .seed_ratio,
            0.25
        );

        for value in [
            json!(-1),
            json!(-0.5),
            json!("abc"),
            json!("NaN"),
            json!("inf"),
            json!(true),
        ] {
            let settings = json!({ "bt_seed_ratio": value });
            assert!(
                BtSettings::from_value(&settings).is_err(),
                "expected {value} to be rejected"
            );
        }
    }

    #[test]
    fn validates_seed_minutes() {
        assert_eq!(
            BtSettings::from_value(&json!({ "bt_seed_minutes": 15 }))
                .unwrap()
                .seed_minutes,
            15
        );
        assert_eq!(
            BtSettings::from_value(&json!({ "bt_seed_minutes": "2" }))
                .unwrap()
                .seed_minutes,
            2
        );

        for value in [
            json!(-1),
            json!(1.5),
            json!("1.5"),
            json!("abc"),
            json!(true),
        ] {
            let settings = json!({ "bt_seed_minutes": value });
            assert!(
                BtSettings::from_value(&settings).is_err(),
                "expected {value} to be rejected"
            );
        }
    }

    #[test]
    fn seeding_is_enabled_by_either_limit() {
        let ratio = BtSettings::from_value(&json!({ "bt_seed_ratio": 0.1 })).unwrap();
        assert!(ratio.seeds());
        assert_eq!(ratio.seed_duration(), None);

        let minutes = BtSettings::from_value(&json!({ "bt_seed_minutes": 1 })).unwrap();
        assert!(minutes.seeds());
        assert_eq!(minutes.seed_duration(), Some(Duration::from_secs(60)));

        let both = BtSettings::from_value(&json!({
            "bt_seed_ratio": 1.0,
            "bt_seed_minutes": 2
        }))
        .unwrap();
        assert_eq!(both.seed_duration(), Some(Duration::from_secs(120)));
    }
}
