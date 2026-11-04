//! Preset dropdown models.
//!
//! Settings that only offer a few recommended values are still stored as
//! plain numbers/strings: an old `settings.json` or `ui.json` can hold a
//! value outside those presets. A `ComboBox` with a fixed model would show a
//! blank slot for it and hide the truth, so every preset combo gets its model
//! from here: the fixed labels plus, only when the stored value sits outside
//! them, one extra `自定义（<value>）` row selected in its place. The extra row
//! is display-only — [`commits`] tells the panels it must never be written
//! back, so merely showing an old value cannot rewrite it.

use slint::{Model, ModelRc, SharedString, VecModel};

use crate::i18n;
use crate::prefs::UiPrefs;
use crate::AppWindow;

/// Where a stored value sits relative to the fixed presets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StoredValue {
    /// The value is one of the presets, at this index.
    Preset(usize),
    /// The value is outside the presets and is shown verbatim in a custom row.
    Custom(String),
    /// There is no value at all: the combo stays unselected.
    Empty,
}

/// Model and selected index for one preset combo. The model is rebuilt from
/// the first `preset_len` rows of the current model, so a custom row added
/// for an earlier value never accumulates.
pub(crate) fn selection(
    current: &ModelRc<SharedString>,
    preset_len: usize,
    stored: StoredValue,
) -> (ModelRc<SharedString>, i32) {
    let mut rows: Vec<SharedString> = current.iter().take(preset_len).collect();
    let index = match stored {
        StoredValue::Preset(index) => index as i32,
        StoredValue::Custom(raw) if !raw.is_empty() => {
            rows.push(custom_label(&raw).into());
            preset_len as i32
        }
        StoredValue::Custom(_) | StoredValue::Empty => -1,
    };
    (ModelRc::new(VecModel::from(rows)), index)
}

/// Whether a combo edit came from a real preset row. Selecting the appended
/// custom row is just an echo of the stored value, not a choice. The panels
/// enforce this with `current-index < preset_len`; this keeps the rule under
/// test.
#[cfg(test)]
pub(crate) fn commits(index: i32, preset_len: usize) -> bool {
    index >= 0 && index < preset_len as i32
}

/// The same model without any custom row; used when a user choice brings the
/// value back inside the presets.
pub(crate) fn preset_only(
    current: &ModelRc<SharedString>,
    preset_len: usize,
) -> ModelRc<SharedString> {
    selection(current, preset_len, StoredValue::Empty).0
}

fn custom_label(raw: &str) -> String {
    i18n::tr("自定义（{}）", "Custom ({})").replacen("{}", raw, 1)
}

/// Position of a numeric value among the presets.
pub(crate) fn number_choice(presets: &[u64], value: u64) -> StoredValue {
    match presets.iter().position(|preset| *preset == value) {
        Some(index) => StoredValue::Preset(index),
        None => StoredValue::Custom(value.to_string()),
    }
}

/// Position of a preference index among the presets.
pub(crate) fn index_choice(preset_len: usize, value: i32) -> StoredValue {
    if value >= 0 && (value as usize) < preset_len {
        StoredValue::Preset(value as usize)
    } else if value >= 0 {
        StoredValue::Custom(value.to_string())
    } else {
        StoredValue::Empty
    }
}

/// Position of a canonical string (a daemon mode, a language code) among the
/// presets; `fallback` covers a missing value, which means the default preset.
pub(crate) fn string_choice(presets: &[&str], value: Option<&str>, fallback: usize) -> StoredValue {
    match value {
        Some(value) => match presets.iter().position(|preset| *preset == value) {
            Some(index) => StoredValue::Preset(index),
            None => StoredValue::Custom(value.to_string()),
        },
        None => StoredValue::Preset(fallback),
    }
}

/// Mirror the UI-local preference combos (accent, start page, completion
/// action, language). Values outside the presets stay visible as custom rows.
pub(crate) fn mirror_ui_prefs(ui: &AppWindow, prefs: &UiPrefs) {
    let (model, index) = selection(
        &ui.get_settings_start_page_model(),
        3,
        index_choice(3, prefs.start_page),
    );
    ui.set_settings_start_page_model(model);
    ui.set_settings_start_page(index);

    let (model, index) = selection(
        &ui.get_settings_after_done_model(),
        3,
        index_choice(3, prefs.after_done),
    );
    ui.set_settings_after_done_model(model);
    ui.set_settings_after_done(index);

    let (model, index) = selection(
        &ui.get_settings_accent_model(),
        6,
        index_choice(6, prefs.accent),
    );
    ui.set_settings_accent_model(model);
    ui.set_settings_accent(index);

    let codes: Vec<&str> = i18n::LANGUAGES.iter().map(|(code, _)| *code).collect();
    let (model, index) = selection(
        &ui.get_settings_language_model(),
        codes.len(),
        string_choice(&codes, Some(prefs.lang.as_str()), 0),
    );
    ui.set_settings_language_model(model);
    ui.set_settings_language(index);
}

/// Theme is stored in its own file, so it is mirrored separately from the
/// other UI preferences.
pub(crate) fn mirror_theme(ui: &AppWindow, mode: i32) {
    let (model, index) = selection(&ui.get_settings_theme_model(), 3, index_choice(3, mode));
    ui.set_settings_theme_model(model);
    ui.set_theme_mode(index);
}

/// Proxy mode is a daemon string ("system" / "none" / "custom" / "pac").
pub(crate) fn mirror_proxy_mode(ui: &AppWindow, mode: Option<&str>) {
    let (model, index) = selection(
        &ui.get_settings_proxy_mode_model(),
        4,
        string_choice(&["system", "none", "custom", "pac"], mode, 0),
    );
    ui.set_settings_proxy_mode_model(model);
    ui.set_settings_proxy_mode(index);
}

/// IP mode only has three display options; `ipv4_only` / `ipv6_only` are the
/// stored spellings, anything else means the default dual-stack preset.
pub(crate) fn mirror_ip_mode(ui: &AppWindow, mode: Option<&str>) {
    let (model, index) = selection(
        &ui.get_settings_ip_mode_model(),
        3,
        string_choice(&["", "ipv4_only", "ipv6_only"], mode, 0),
    );
    ui.set_settings_ip_mode_model(model);
    ui.set_settings_ip_mode(index);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(rows: &[&str]) -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(
            rows.iter()
                .map(|row| SharedString::from(*row))
                .collect::<Vec<_>>(),
        ))
    }

    fn rows(model: &ModelRc<SharedString>) -> Vec<String> {
        model.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn preset_values_keep_the_fixed_model() {
        let preset = model(&["2", "4", "8", "16"]);
        let (built, index) = selection(&preset, 4, number_choice(&[2, 4, 8, 16], 8));
        assert_eq!(rows(&built), ["2", "4", "8", "16"]);
        assert_eq!(index, 2);
    }

    #[test]
    fn out_of_preset_values_become_a_custom_row() {
        let preset = model(&["2", "4", "8", "16"]);
        let (built, index) = selection(&preset, 4, number_choice(&[2, 4, 8, 16], 6));
        assert_eq!(
            rows(&built),
            [
                "2".to_string(),
                "4".into(),
                "8".into(),
                "16".into(),
                custom_label("6")
            ]
        );
        assert_eq!(index, 4, "the custom row is the selected one");

        let preset = model(&["不重试", "3", "5", "10"]);
        let (built, index) = selection(&preset, 4, number_choice(&[0, 3, 5, 10], 7));
        assert_eq!(
            rows(&built),
            [
                "不重试".to_string(),
                "3".into(),
                "5".into(),
                "10".into(),
                custom_label("7")
            ]
        );
        assert_eq!(index, 4);
    }

    #[test]
    fn custom_row_is_read_only_and_presets_commit() {
        // Selecting the appended row (index == preset_len) must not write the
        // stored value back; a real preset row commits its value.
        assert!(!commits(4, 4));
        assert!(!commits(-1, 4));
        assert!(commits(0, 4));
        assert!(commits(3, 4));
    }

    #[test]
    fn custom_rows_do_not_accumulate() {
        let with_custom = model(&["2", "4", "8", "16", "自定义（6）"]);
        let (built, index) = selection(&with_custom, 4, number_choice(&[2, 4, 8, 16], 4));
        assert_eq!(rows(&built), ["2", "4", "8", "16"]);
        assert_eq!(index, 1);
        assert_eq!(rows(&preset_only(&with_custom, 4)), ["2", "4", "8", "16"]);
    }

    #[test]
    fn preference_indices_outside_the_presets_stay_visible() {
        let preset = model(&["A", "B", "C", "D", "E", "F"]);
        let (built, index) = selection(&preset, 6, index_choice(6, 7));
        assert_eq!(rows(&built).len(), 7);
        assert_eq!(rows(&built)[6], custom_label("7"));
        assert_eq!(index, 6);
        assert!(!commits(index, 6));
    }

    #[test]
    fn accent_beyond_the_six_presets_stays_visible() {
        let preset = model(&["Aurora 紫", "靛蓝", "青碧", "苔绿", "琥珀", "蔷薇"]);
        let (built, index) = selection(&preset, 6, index_choice(6, 7));
        assert_eq!(rows(&built).len(), 7);
        assert_eq!(rows(&built)[6], custom_label("7"));
        assert_eq!(index, 6, "the custom accent row is selected");
    }

    #[test]
    fn unknown_language_codes_stay_visible() {
        let preset = model(&["zh", "en", "ja", "zh-TW"]);
        let (built, index) = selection(
            &preset,
            4,
            string_choice(&["zh", "en", "ja", "zh-TW"], Some("fr"), 0),
        );
        assert_eq!(rows(&built)[4], custom_label("fr"));
        assert_eq!(index, 4);
    }
}
