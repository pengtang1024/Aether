//! Chinese-first i18n for the Rust-side strings (toasts, settings hints,
//! backend notices, status labels).
//!
//! Chinese is the first language: every call site keeps the Chinese literal
//! and pairs it with English beside it — no key table that can drift from
//! its translation. The process-global flag is set at startup from
//! `UiPrefs.lang` and updated on switch, so background workers (backend
//! notices) report in the same language the UI shows. Slint strings are
//! handled separately through bundled `@tr` translations (see `build.rs`
//! and `translations/`).
use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Set the process language; call at startup and on the language switch.
pub fn set_language(english: bool) {
    ENGLISH.store(english, Ordering::Relaxed);
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Pick the string for the current language. Accepts `&str` or `String` so
/// literals and `format!()` results both pass directly; the unused side is
/// constructed anyway, which keeps call sites readable and costs nothing
/// that matters.
pub fn tr(zh: impl Into<String>, en: impl Into<String>) -> String {
    if is_english() {
        en.into()
    } else {
        zh.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_by_default_and_switches_to_english() {
        set_language(false);
        assert_eq!(tr("已添加下载任务", "Download queued"), "已添加下载任务");
        set_language(true);
        assert_eq!(tr("已添加下载任务", "Download queued"), "Download queued");
        assert_eq!(
            tr(format!("第 {} 个", 3), format!("number {}", 3)),
            "number 3"
        );
        set_language(false);
    }
}
