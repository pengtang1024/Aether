//! Plugin settings schema to dialog form conversion.

use slint::{Model, ModelRc, SharedString, VecModel};

use aether_daemon::PluginInfo;

use crate::i18n;
use crate::SettingField;

#[cfg(test)]
mod tests;

pub(crate) fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        other => other.to_string(),
    }
}

/// Flatten a plugin's `settings_schema` into the dialog form: field metadata
/// in schema order — the order the dialog renders, and therefore saves, in —
/// plus each field's initial text. A stored value wins; "" means "not set",
/// so the field starts empty and the default shows as its placeholder. Bools
/// and selects always resolve to a concrete value so their control never sits
/// on an ambiguous empty state.
pub(crate) fn plugin_settings_form(info: &PluginInfo) -> (Vec<SettingField>, Vec<String>) {
    let mut fields = Vec::new();
    let mut values = Vec::new();
    let Some(schema) = info.settings_schema.as_ref() else {
        return (fields, values);
    };
    let stored = info.settings.as_object();
    let Some(sections) = schema.get("sections").and_then(serde_json::Value::as_array) else {
        return (fields, values);
    };
    for section in sections {
        let section_title = section
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let Some(section_fields) = section.get("fields").and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for field in section_fields {
            let Some(key) = field.get("key").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let kind = field
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("string");
            let default = field.get("default").map(json_text).unwrap_or_default();
            let options: Vec<String> = field
                .get("options")
                .and_then(serde_json::Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|option| option.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let stored_value = stored
                .and_then(|settings| settings.get(key))
                .filter(|value| !value.is_null());
            let value = match kind {
                "bool" => stored_value
                    .and_then(serde_json::Value::as_bool)
                    .or(match default.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    })
                    .unwrap_or(false)
                    .to_string(),
                "select" => {
                    let in_options = |choice: &str| options.iter().any(|option| option == choice);
                    stored_value
                        .and_then(serde_json::Value::as_str)
                        .filter(|choice| in_options(choice))
                        .map(str::to_string)
                        .or_else(|| in_options(&default).then(|| default.clone()))
                        .or_else(|| options.first().cloned())
                        .unwrap_or_default()
                }
                _ => stored_value.map(json_text).unwrap_or_default(),
            };
            let min = field.get("min").map(json_text);
            let max = field.get("max").map(json_text);
            fields.push(SettingField {
                section: section_title.into(),
                key: key.into(),
                kind: kind.into(),
                default: default.into(),
                has_min: min.is_some(),
                min: min.unwrap_or_default().into(),
                has_max: max.is_some(),
                max: max.unwrap_or_default().into(),
                options: ModelRc::new(VecModel::from(
                    options
                        .iter()
                        .map(String::as_str)
                        .map(SharedString::from)
                        .collect::<Vec<_>>(),
                )),
                options_text: options.join(" / ").into(),
                value: value.clone().into(),
            });
            values.push(value);
        }
    }
    (fields, values)
}

#[derive(Debug)]
pub(crate) enum FormError {
    Integer { key: String, text: String },
    Number { key: String, text: String },
}

impl FormError {
    pub(crate) fn message(self) -> String {
        match self {
            FormError::Integer { key, text } => format!(
                "{} · {key} = {text}",
                i18n::tr("需要整数", "Must be an integer")
            ),
            FormError::Number { key, text } => format!(
                "{} · {key} = {text}",
                i18n::tr("需要数字", "Must be a number")
            ),
        }
    }
}

/// Turn the dialog's text slots into the JSON object stored for a plugin.
/// Bools are always sent (the switch always shows a concrete state); an empty
/// number or string means "not set" and is skipped — except that clearing a
/// string the daemon still holds sends `""`, because the save replaces the
/// whole object.
pub(crate) fn plugin_settings_json(
    fields: &slint::ModelRc<SettingField>,
    values: &slint::ModelRc<SharedString>,
    stored: &serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, FormError> {
    let mut settings = serde_json::Map::new();
    for index in 0..fields.row_count() {
        let Some(field) = fields.row_data(index) else {
            continue;
        };
        let text = values.row_data(index).unwrap_or_default();
        let key = field.key.to_string();
        match field.kind.as_str() {
            "bool" => {
                settings.insert(key, serde_json::Value::Bool(text == "true"));
            }
            "int" => {
                if text.is_empty() {
                    continue;
                }
                let Ok(number) = text.parse::<i64>() else {
                    return Err(FormError::Integer {
                        key,
                        text: text.to_string(),
                    });
                };
                settings.insert(key, serde_json::Value::from(number));
            }
            "float" => {
                if text.is_empty() {
                    continue;
                }
                let number = text
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64);
                let Some(number) = number else {
                    return Err(FormError::Number {
                        key,
                        text: text.to_string(),
                    });
                };
                settings.insert(key, serde_json::Value::Number(number));
            }
            _ => {
                if !text.is_empty() {
                    settings.insert(key, serde_json::Value::String(text.to_string()));
                } else if stored
                    .get(key.as_str())
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|stored| !stored.is_empty())
                {
                    settings.insert(key, serde_json::Value::String(String::new()));
                }
            }
        }
    }
    Ok(settings)
}

/// Render the daemon's `plugin_pins` map as the text field's
/// `id=version; id=version` form (sorted, so a refresh does not shuffle it).
pub(crate) fn format_plugin_pins(value: &serde_json::Value) -> String {
    let Some(pins) = value.as_object() else {
        return String::new();
    };
    let mut entries = pins
        .iter()
        .filter_map(|(id, version)| version.as_str().map(|version| format!("{id}={version}")))
        .collect::<Vec<_>>();
    entries.sort();
    entries.join("; ")
}

/// Parse the pin text field. Validation of ids/semver happens in the daemon
/// (`set_settings`); this only rejects entries without an `=`.
pub(crate) fn parse_plugin_pins(text: &str) -> Result<serde_json::Value, String> {
    let mut pins = serde_json::Map::new();
    for entry in text.split([';', '\n']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (id, version) = entry.split_once('=').ok_or_else(|| {
            i18n::tr(
                format!("条目缺少 =：{entry}"),
                format!("Missing = in {entry}"),
            )
        })?;
        let (id, version) = (id.trim(), version.trim());
        if id.is_empty() || version.is_empty() {
            return Err(i18n::tr(
                format!("条目不完整：{entry}"),
                format!("Incomplete entry: {entry}"),
            ));
        }
        pins.insert(
            id.to_string(),
            serde_json::Value::String(version.to_string()),
        );
    }
    Ok(serde_json::Value::Object(pins))
}
