//! Drawer sections built from a plugin's `detail_schema`.

use aether_daemon::PluginInfo;
use slint::{ModelRc, SharedString, VecModel};

use crate::i18n;
use crate::{DetailRow, DetailSection};

/// Known `detail_schema` keys rendered with a translated label; anything
/// else falls through as the raw key (the schema is plugin-owned data).
pub(crate) fn detail_label(key: &str) -> String {
    match key {
        "speed" => i18n::tr("速度", "speed"),
        "eta" => i18n::tr("剩余时间", "eta"),
        "size" => i18n::tr("大小", "size"),
        "address" => i18n::tr("地址", "address"),
        "kind" => i18n::tr("状态", "kind"),
        other => other.to_string(),
    }
}

/// Render one scalar payload value; structured values are not rows.
pub(crate) fn detail_scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(if *flag {
            i18n::tr("是", "yes")
        } else {
            i18n::tr("否", "no")
        }),
        _ => None,
    }
}

/// One peer cell: the declared column first, then the aria2-shaped keys
/// the BT plugin actually sends. `None` skips the whole row so columns
/// never render with empty holes.
pub(crate) fn peer_cell(
    peer: &serde_json::Map<String, serde_json::Value>,
    column: &str,
) -> Option<String> {
    if let Some(value) = peer.get(column) {
        return detail_scalar(value);
    }
    match column {
        "address" => {
            let ip = peer.get("ip")?.as_str()?;
            let port = peer
                .get("port")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Some(if port.is_empty() {
                ip.to_string()
            } else {
                format!("{ip}:{port}")
            })
        }
        "speed" => peer
            .get("downloadSpeed")
            .and_then(detail_scalar)
            .or_else(|| peer.get("uploadSpeed").and_then(detail_scalar)),
        "kind" => peer.get("state").and_then(detail_scalar).or_else(|| {
            peer.get("seeder")
                .and_then(serde_json::Value::as_bool)
                .map(|flag| if flag { "seeder" } else { "leecher" }.to_string())
        }),
        _ => None,
    }
}

/// Build the drawer's generic sections from a plugin's `detail_schema`
/// and its live `Detail` payload. Only sections with at least one filled
/// row are returned: `logs` / `chunks` / unknown types and missing keys
/// degrade to nothing instead of empty labels.
pub(crate) fn build_detail_sections(
    info: &PluginInfo,
    detail: &serde_json::Value,
) -> Vec<DetailSection> {
    let Some(sections) = info
        .detail_schema
        .as_ref()
        .and_then(|schema| schema.get("sections"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    let mut built = Vec::new();
    for section in sections {
        match section
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
        {
            "metrics" => {
                let Some(items) = section.get("items").and_then(serde_json::Value::as_array) else {
                    continue;
                };
                let rows: Vec<DetailRow> = items
                    .iter()
                    .filter_map(|item| {
                        let key = item.as_str()?;
                        let value = detail.get(key)?;
                        Some(DetailRow {
                            cells: ModelRc::new(VecModel::from(vec![
                                SharedString::from(detail_label(key)),
                                SharedString::from(detail_scalar(value)?),
                            ])),
                        })
                    })
                    .collect();
                if !rows.is_empty() {
                    built.push(DetailSection {
                        kind: "metrics".into(),
                        title: String::new().into(),
                        columns: ModelRc::new(VecModel::default()),
                        rows: ModelRc::new(VecModel::from(rows)),
                    });
                }
            }
            "peers" => {
                let Some(declared) = section.get("columns").and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                let raw: Vec<&str> = declared
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect();
                if raw.is_empty() {
                    continue;
                }
                let Some(peers) = detail.get("peers").and_then(serde_json::Value::as_array) else {
                    continue;
                };
                let rows: Vec<DetailRow> = peers
                    .iter()
                    .filter_map(|peer| {
                        let object = peer.as_object()?;
                        let cells: Vec<SharedString> = raw
                            .iter()
                            .map(|column| peer_cell(object, column).map(SharedString::from))
                            .collect::<Option<_>>()?;
                        Some(DetailRow {
                            cells: ModelRc::new(VecModel::from(cells)),
                        })
                    })
                    .collect();
                if !rows.is_empty() {
                    built.push(DetailSection {
                        kind: "peers".into(),
                        title: String::new().into(),
                        columns: ModelRc::new(VecModel::from(
                            raw.iter()
                                .map(|column| SharedString::from(detail_label(column)))
                                .collect::<Vec<_>>(),
                        )),
                        rows: ModelRc::new(VecModel::from(rows)),
                    });
                }
            }
            // `chunks` stays on the task's own chunk model (the payload
            // carries no chunk list) and `logs` has no pane yet.
            _ => {}
        }
    }
    built
}

#[cfg(test)]
mod tests {
    use slint::Model;

    use super::*;
    use serde_json::json;

    #[test]
    fn metrics_rows_follow_the_schema_order() {
        let info = PluginInfo {
            id: "http".into(),
            name: "HTTP".into(),
            version: "0.1.0".into(),
            protocols: vec!["http".into()],
            capabilities: Vec::new(),
            features: Vec::new(),
            enabled: true,
            blocked: false,
            service: false,
            running: true,
            last_error: None,
            has_backup: false,
            settings_schema: None,
            settings: json!({}),
            detail_schema: Some(json!({
                "sections": [
                    {"type": "metrics", "items": ["speed", "eta", "size"]},
                    {"type": "logs", "max_lines": 200}
                ]
            })),
        };
        let detail = json!({"speed": "936.1 KB/s", "eta": "18 s", "size": "7.7 MB / 25.0 MB"});
        let sections = build_detail_sections(&info, &detail);
        assert_eq!(sections.len(), 1);
        let rows = sections[0].rows.clone();
        assert_eq!(rows.row_count(), 3);
        let labels: Vec<String> = (0..3)
            .map(|index| {
                rows.row_data(index)
                    .unwrap()
                    .cells
                    .row_data(0)
                    .unwrap()
                    .to_string()
            })
            .collect();
        // The process language is global and other test threads flip it while
        // this runs, so each label is accepted in either language. The order is
        // what this test is about.
        let expected = [("速度", "speed"), ("剩余时间", "eta"), ("大小", "size")];
        for (label, (chinese, english)) in labels.iter().zip(expected) {
            assert!(
                label == chinese || label == english,
                "unexpected detail label: {label}"
            );
        }
    }
}
