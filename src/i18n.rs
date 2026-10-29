//! Chinese-first i18n for the Rust-side strings (toasts, settings hints,
//! backend notices, status labels).
//!
//! Chinese is the first language: every call site keeps the Chinese literal
//! beside its English pair. Additional languages come from [`table`] rows
//! keyed by that same Chinese string — one row holds every language, so a
//! translation cannot drift away from its key. Missing rows fall back to
//! Chinese honestly rather than showing an empty or wrong string.
//!
//! The language code is process-global (set at startup from `UiPrefs.lang`,
//! updated on switch), so background workers (backend notices) report in the
//! language the UI shows. Slint strings are handled separately through
//! bundled `@tr` translations (see `build.rs` and `translations/`).
mod table;

use std::sync::atomic::{AtomicU8, Ordering};

/// 0 = zh (source), 1 = en, 2 = ja, 3 = zh-TW — matches the UI switch order.
static LANG: AtomicU8 = AtomicU8::new(0);

/// Supported language codes, in UI-switch order.
pub const LANGUAGES: [(&str, &str); 4] = [
    ("zh", "简体中文"),
    ("en", "English"),
    ("ja", "日本語"),
    ("zh-TW", "繁體中文"),
];

/// Set the process language by code; unknown codes fall back to Chinese.
pub fn set_language(code: &str) {
    let index = LANGUAGES
        .iter()
        .position(|(known, _)| *known == code)
        .unwrap_or(0);
    LANG.store(index as u8, Ordering::Relaxed);
}

pub fn current_code() -> &'static str {
    LANGUAGES[LANG.load(Ordering::Relaxed) as usize].0
}

/// Index of a language code for the UI switch mirror.
pub fn index_of(code: &str) -> usize {
    LANGUAGES
        .iter()
        .position(|(known, _)| *known == code)
        .unwrap_or(0)
}

/// Pick the string for the current language. Accepts `&str` or `String` so
/// literals and `format!()` results both pass directly. English rides with
/// the call site (the common case); other languages come from the table
/// rows keyed by the Chinese source, falling back to it when a row is
/// missing.
pub fn tr(zh: impl Into<String>, en: impl Into<String>) -> String {
    let zh = zh.into();
    match current_code() {
        "en" => en.into(),
        "ja" => table::lookup_ja(&zh).unwrap_or(zh),
        "zh-TW" => table::lookup_zh_hant(&zh).unwrap_or(zh),
        _ => zh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_chinese_and_switches() {
        set_language("zh");
        assert_eq!(tr("已添加下载任务", "Download queued"), "已添加下载任务");
        set_language("en");
        assert_eq!(tr("已添加下载任务", "Download queued"), "Download queued");
        assert_eq!(
            tr(format!("第 {} 个", 3), format!("number {}", 3)),
            "number 3"
        );
        set_language("zh");
    }

    #[test]
    fn table_languages_resolve_and_fall_back() {
        // A row-backed string picks up its table translation…
        let sample = table::rows().next().expect("table must not be empty");
        set_language("ja");
        assert_eq!(tr(sample.0, "unused-en"), sample.1);
        set_language("zh-TW");
        assert_eq!(tr(sample.0, "unused-en"), sample.2);
        // …and an unknown string falls back to Chinese instead of empty.
        set_language("ja");
        assert_eq!(tr("绝不在表中的字符串", "fallback"), "绝不在表中的字符串");
        set_language("zh");
    }

    #[test]
    fn language_codes_map_to_switch_indices() {
        // Tests run in parallel over one process-global: always set before
        // asserting so sibling tests' flips never leak in.
        set_language("ja");
        assert_eq!(current_code(), "ja");
        assert_eq!(index_of("ja"), 2);
        set_language("no-such-lang");
        assert_eq!(current_code(), "zh", "unknown code falls back to source");
        set_language("zh");
    }
}
