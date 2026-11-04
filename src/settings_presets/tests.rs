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
    assert_eq!(rows(&built)[6], unknown_label());
    assert_eq!(index, 6);
    assert!(!commits(index, 6));
}

#[test]
fn enum_rows_never_show_the_stored_code() {
    // An index or string outside the presets is an implementation detail:
    // the row must read as "unknown option", not "自定义（7）" / "fr".
    let preset = model(&["A", "B", "C", "D", "E", "F"]);
    let (built, _) = selection(&preset, 6, index_choice(6, 7));
    let label = &rows(&built)[6];
    assert_eq!(label, &unknown_label());
    assert!(
        !label.contains('7'),
        "raw index leaked into the row: {label}"
    );

    let preset = model(&["system", "none", "custom", "pac"]);
    let (built, _) = selection(
        &preset,
        4,
        string_choice(&["system", "none", "custom", "pac"], Some("wpad"), 0),
    );
    let label = &rows(&built)[4];
    assert_eq!(label, &unknown_label());
    assert!(
        !label.contains("wpad"),
        "raw mode leaked into the row: {label}"
    );
}

#[test]
fn numeric_presets_keep_the_raw_value() {
    // Numeric settings stay readable on their own, so their out-of-range
    // row keeps the exact value the daemon will honour.
    let preset = model(&["2", "4", "8", "16"]);
    let (built, _) = selection(&preset, 4, number_choice(&[2, 4, 8, 16], 6));
    assert_eq!(rows(&built)[4], custom_label("6"));
}

#[test]
fn accent_beyond_the_six_presets_stays_visible() {
    let preset = model(&["Aurora 紫", "靛蓝", "青碧", "苔绿", "琥珀", "蔷薇"]);
    let (built, index) = selection(&preset, 6, index_choice(6, 7));
    assert_eq!(rows(&built).len(), 7);
    assert_eq!(rows(&built)[6], unknown_label());
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
    assert_eq!(rows(&built)[4], unknown_label());
    assert_eq!(index, 4);
}
