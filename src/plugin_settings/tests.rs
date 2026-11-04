//! Plugin settings form and pin round-trip tests.

use serde_json::json;

use super::*;

#[test]
fn pin_text_roundtrips_through_the_daemon_shape() {
    let value = parse_plugin_pins("http=0.2.0; bt=1.0.0\ns3 = 0.1.0").unwrap();
    assert_eq!(
        value,
        json!({"http": "0.2.0", "bt": "1.0.0", "s3": "0.1.0"})
    );
    assert_eq!(format_plugin_pins(&value), "bt=1.0.0; http=0.2.0; s3=0.1.0");
    assert_eq!(format_plugin_pins(&json!(null)), "");
    assert_eq!(parse_plugin_pins("  ; \n").unwrap(), json!({}));
}

#[test]
fn pin_text_rejects_entries_without_a_version() {
    assert!(parse_plugin_pins("http").is_err());
    assert!(parse_plugin_pins("http=").is_err());
    assert!(parse_plugin_pins("=0.2.0").is_err());
}

fn field(key: &str, kind: &str) -> SettingField {
    SettingField {
        section: String::new().into(),
        key: key.into(),
        kind: kind.into(),
        default: String::new().into(),
        has_min: false,
        min: String::new().into(),
        has_max: false,
        max: String::new().into(),
        options: ModelRc::new(VecModel::from(Vec::<SharedString>::new())),
        options_text: String::new().into(),
        value: String::new().into(),
    }
}

fn run(
    fields: Vec<SettingField>,
    values: &[&str],
    stored: &serde_json::Value,
) -> Result<serde_json::Value, FormError> {
    let fields = ModelRc::new(VecModel::from(fields));
    let values = ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|text| SharedString::from(*text))
            .collect::<Vec<_>>(),
    ));
    plugin_settings_json(&fields, &values, stored).map(serde_json::Value::Object)
}

#[test]
fn sends_bools_and_skips_empty_values() {
    let stored = json!({"ua": "old", "retries": 2});
    let settings = run(
        vec![
            field("seed", "bool"),
            field("retries", "int"),
            field("ua", "string"),
        ],
        &["false", "", ""],
        &stored,
    )
    .expect("no parse errors");
    // Bool keeps its concrete state; the emptied int is dropped; the
    // cleared string is sent empty so the replace drops the stored value.
    assert_eq!(settings, json!({"seed": false, "ua": ""}));
}

#[test]
fn parses_numbers_and_text() {
    let settings = run(
        vec![
            field("retries", "int"),
            field("ratio", "float"),
            field("ua", "string"),
        ],
        &["3", "1.5", "aether"],
        &json!({}),
    )
    .expect("valid values");
    assert_eq!(
        settings,
        json!({"retries": 3, "ratio": 1.5, "ua": "aether"})
    );
}

#[test]
fn rejects_unparseable_numbers_with_their_field() {
    assert!(matches!(
        run(vec![field("retries", "int")], &["abc"], &json!({})),
        Err(FormError::Integer { key, text }) if key == "retries" && text == "abc"
    ));
    assert!(matches!(
        run(vec![field("ratio", "float")], &["fast"], &json!({})),
        Err(FormError::Number { key, .. }) if key == "ratio"
    ));
}

fn select_plugin(stored: &str) -> PluginInfo {
    serde_json::from_value(json!({
        "id": "demo",
        "name": "Demo",
        "version": "1.0.0",
        "settings_schema": {
            "sections": [{
                "title": "Mode",
                "fields": [{
                    "key": "mode",
                    "type": "select",
                    "default": "fast",
                    "options": ["fast", "slow"]
                }]
            }]
        },
        "settings": {"mode": stored},
    }))
    .expect("plugin info decodes")
}

#[test]
fn select_shows_a_stored_value_outside_the_options() {
    let (fields, values) = plugin_settings_form(&select_plugin("turbo"));
    assert_eq!(fields.len(), 1);
    let options: Vec<String> = fields[0].options.iter().map(|o| o.to_string()).collect();
    // The stored value is appended instead of being filtered away, and it
    // is the one selected in the form.
    assert_eq!(options, ["fast", "slow", "turbo"]);
    assert_eq!(values[0], "turbo");
}

#[test]
fn select_inside_the_options_adds_no_extra_row() {
    let (fields, values) = plugin_settings_form(&select_plugin("slow"));
    let options: Vec<String> = fields[0].options.iter().map(|o| o.to_string()).collect();
    assert_eq!(options, ["fast", "slow"]);
    assert_eq!(values[0], "slow");
}
