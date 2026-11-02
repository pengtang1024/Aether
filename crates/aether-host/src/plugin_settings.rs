//! Plugin-declared settings (`settings_schema` in `plugin.json`).
//!
//! The Host owns storage and validation; the plugin only reads the resulting
//! values from `task.settings`. A plugin setting is a *default* layer: a task
//! that sets the same key keeps its own value.

use serde_json::{Map, Value};

/// Validate `settings` against a manifest `settings_schema`.
///
/// Unknown keys are rejected (same reasoning as daemon settings: a typo must
/// surface, not be stored and silently ignored). Defaults are not materialised
/// here; the plugin applies them when a key is absent.
pub fn validate(schema: Option<&Value>, settings: &Value) -> Result<(), String> {
    let object = settings
        .as_object()
        .ok_or_else(|| "settings must be a JSON object".to_string())?;
    let fields = fields(schema)?;
    for (key, value) in object {
        let field = fields
            .iter()
            .find(|field| field.get("key").and_then(Value::as_str) == Some(key.as_str()))
            .ok_or_else(|| format!("unknown setting {key}"))?;
        check(field, key, value)?;
    }
    Ok(())
}

/// Copy `defaults` into `task_settings` for keys the task does not set.
pub fn apply_defaults(task_settings: &mut Value, defaults: &Value) {
    let Some(defaults) = defaults.as_object() else {
        return;
    };
    if !task_settings.is_object() {
        *task_settings = Value::Object(Map::new());
    }
    let Some(target) = task_settings.as_object_mut() else {
        return;
    };
    for (key, value) in defaults {
        target.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

fn fields(schema: Option<&Value>) -> Result<Vec<&Value>, String> {
    let schema = schema.ok_or_else(|| "plugin declares no settings_schema".to_string())?;
    let sections = schema
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "settings_schema needs a sections array".to_string())?;
    Ok(sections
        .iter()
        .flat_map(|section| {
            section
                .get("fields")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
        })
        .collect())
}

fn check(field: &Value, key: &str, value: &Value) -> Result<(), String> {
    let kind = field
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("setting {key} has no type"))?;
    match kind {
        "bool" => value
            .is_boolean()
            .then_some(())
            .ok_or_else(|| format!("setting {key} must be a boolean")),
        "int" => {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("setting {key} must be an integer"))?;
            bound(field, key, number as f64)
        }
        "float" => {
            let number = value
                .as_f64()
                .ok_or_else(|| format!("setting {key} must be a number"))?;
            bound(field, key, number)
        }
        "select" => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("setting {key} must be a string"))?;
            match field.get("options").and_then(Value::as_array) {
                Some(options) if !options.iter().any(|option| option.as_str() == Some(text)) => {
                    Err(format!("setting {key} must be one of the declared options"))
                }
                _ => Ok(()),
            }
        }
        "string" | "path" | "secret" | "multiline" => value
            .is_string()
            .then_some(())
            .ok_or_else(|| format!("setting {key} must be a string")),
        other => Err(format!("setting {key} has unknown type {other}")),
    }
}

fn bound(field: &Value, key: &str, number: f64) -> Result<(), String> {
    if let Some(min) = field.get("min").and_then(Value::as_f64) {
        if number < min {
            return Err(format!("setting {key} is below {min}"));
        }
    }
    if let Some(max) = field.get("max").and_then(Value::as_f64) {
        if number > max {
            return Err(format!("setting {key} is above {max}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "sections": [{
                "title": "S3",
                "fields": [
                    {"key": "access_key", "type": "string"},
                    {"key": "secret_key", "type": "secret"},
                    {"key": "region", "type": "string", "default": "us-east-1"},
                    {"key": "path_style", "type": "bool", "default": false},
                    {"key": "retries", "type": "int", "min": 0, "max": 5},
                    {"key": "mode", "type": "select", "options": ["fast", "safe"]}
                ]
            }]
        })
    }

    #[test]
    fn accepts_declared_types() {
        let value = json!({
            "access_key": "AKIA",
            "secret_key": "shh",
            "path_style": true,
            "retries": 3,
            "mode": "safe"
        });
        assert_eq!(validate(Some(&schema()), &value), Ok(()));
    }

    #[test]
    fn rejects_unknown_keys_and_wrong_types() {
        assert_eq!(
            validate(Some(&schema()), &json!({"bogus": 1})),
            Err("unknown setting bogus".into())
        );
        assert_eq!(
            validate(Some(&schema()), &json!({"retries": "3"})),
            Err("setting retries must be an integer".into())
        );
        assert_eq!(
            validate(Some(&schema()), &json!({"retries": 9})),
            Err("setting retries is above 5".into())
        );
        assert_eq!(
            validate(Some(&schema()), &json!({"mode": "slow"})),
            Err("setting mode must be one of the declared options".into())
        );
        assert_eq!(
            validate(None, &json!({})),
            Err("plugin declares no settings_schema".into())
        );
    }

    #[test]
    fn defaults_never_override_task_values() {
        let mut task = json!({"region": "eu-west-1"});
        apply_defaults(
            &mut task,
            &json!({"region": "us-east-1", "path_style": true}),
        );
        assert_eq!(task, json!({"region": "eu-west-1", "path_style": true}));
    }

    #[test]
    fn defaults_turn_null_task_settings_into_an_object() {
        let mut task = Value::Null;
        apply_defaults(&mut task, &json!({"retries": 2}));
        assert_eq!(task, json!({"retries": 2}));
    }
}
