//! Clipboard link detection: a background thread polls the clipboard for a
//! fresh download URL and hands it to the window, which offers a one-click
//! "start download" toast.
//!
//! Off by default (a UI preference, `clipboard_watch`), because reading the
//! clipboard is the kind of access a user should grant explicitly. The poll
//! only keeps *changes*: the first reading after enabling just seeds the
//! baseline, so enabling the toggle never fires on whatever was already
//! copied.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Poll interval. Snappy enough to feel immediate, slow enough to stay
/// invisible in CPU listings.
const POLL_INTERVAL: Duration = Duration::from_millis(1200);

/// What the window shows: the URL that looks downloadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLink {
    pub url: String,
}

/// Handle the settings page uses to flip the poller. Holds the change
/// baseline so disabling re-seeds on the next enable instead of firing
/// stale clipboard content.
#[derive(Clone)]
pub struct ClipboardControl {
    enabled: Arc<Mutex<bool>>,
    last: Arc<Mutex<String>>,
    /// Schemes the installed plugins declare; replaced by each daemon snapshot.
    schemes: Arc<Mutex<Vec<String>>>,
}

impl ClipboardControl {
    /// Accept exactly the protocols the daemon's plugin list declares, so a
    /// plugin that adds a scheme is detected without touching the UI.
    pub fn set_protocols(&self, protocols: &[String]) {
        let mut schemes = self.schemes.lock().unwrap();
        schemes.clear();
        schemes.extend(protocols.iter().cloned());
    }

    pub fn set_enabled(&self, on: bool) {
        *self.enabled.lock().unwrap() = on;
        if !on {
            // Forget history: the next enable must start from "now".
            self.last.lock().unwrap().clear();
        }
    }
}

/// Receiver side of the detected links; the window drains it each tick.
pub struct ClipboardWatch {
    links: Receiver<DetectedLink>,
}

impl ClipboardWatch {
    /// Drain freshly detected links (at most one is produced per change).
    pub fn drain(&self) -> Vec<DetectedLink> {
        let mut out = Vec::new();
        while let Ok(link) = self.links.try_recv() {
            out.push(link);
        }
        out
    }
}

/// Start the poller. Returns `None` when no clipboard backend exists
/// (headless session, SSH, missing Wayland data-control): the caller
/// surfaces that honestly instead of pretending the feature works.
pub fn start() -> Option<(ClipboardWatch, ClipboardControl)> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    // Seed the baseline from the current content so enabling the toggle
    // does not immediately offer whatever was copied earlier.
    let initial = clipboard.get_text().unwrap_or_default();
    let control = ClipboardControl {
        enabled: Arc::new(Mutex::new(false)),
        last: Arc::new(Mutex::new(initial)),
        schemes: Arc::new(Mutex::new(
            DEFAULT_SCHEMES
                .iter()
                .map(|scheme| scheme.to_string())
                .collect(),
        )),
    };
    let (tx, rx) = mpsc::channel::<DetectedLink>();

    let thread_enabled = control.enabled.clone();
    let thread_last = control.last.clone();
    let thread_schemes = control.schemes.clone();
    thread::spawn(move || loop {
        thread::sleep(POLL_INTERVAL);
        if !*thread_enabled.lock().unwrap() {
            continue;
        }
        // get_text can transiently fail (clipboard owner restarting);
        // skip the tick rather than tearing the watcher down.
        let Ok(text) = clipboard.get_text() else {
            continue;
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            continue;
        }
        let mut previous = thread_last.lock().unwrap();
        if *previous == text {
            continue;
        }
        *previous = text.clone();
        if let Some(url) = detectable_url(&text, &thread_schemes.lock().unwrap()) {
            // A failed send means the window is gone; stop quietly.
            if tx.send(DetectedLink { url }).is_err() {
                break;
            }
        }
    });

    Some((ClipboardWatch { links: rx }, control))
}

/// Schemes accepted before the daemon reports its plugin list. Kept identical
/// to what 0.5.0 shipped so the watcher works during startup.
const DEFAULT_SCHEMES: [&str; 7] = ["http", "https", "ftp", "ftps", "magnet", "ed2k", "thunder"];

/// What counts as a download link: a single-token URL with a scheme the
/// engine can actually handle. Anything else (multi-word text, paths,
/// `file://`, javascript:) is not reported — the daemon's plugin probe would
/// just reject it after the user clicked through.
fn detectable_url(text: &str, schemes: &[String]) -> Option<String> {
    // A link copied as "https://a/b (from chat)" has trailing words; accept
    // a leading URL token but never embedded ones.
    let token = text.split_whitespace().next()?;
    let trimmed = token.trim_end_matches([',', ';', '。', '，']);
    let (scheme, _) = trimmed.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    schemes
        .iter()
        .any(|known| known == &scheme)
        .then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect_with_default(text: &str) -> Option<String> {
        let schemes: Vec<String> = DEFAULT_SCHEMES.iter().map(|s| s.to_string()).collect();
        detectable_url(text, &schemes)
    }

    #[test]
    fn accepts_a_plugin_declared_scheme() {
        let schemes = vec!["ipfs".to_string()];
        assert_eq!(
            detectable_url("ipfs://bafy/file", &schemes),
            Some("ipfs://bafy/file".into())
        );
        assert_eq!(detectable_url("https://example.com/a", &schemes), None);
    }

    #[test]
    fn detects_downloadable_schemes() {
        assert_eq!(
            detect_with_default("https://example.com/file.bin"),
            Some("https://example.com/file.bin".into())
        );
        assert_eq!(
            detect_with_default("magnet:?xt=urn:btih:abc"),
            Some("magnet:?xt=urn:btih:abc".into())
        );
        // Trailing punctuation from a chat message is dropped.
        assert_eq!(
            detect_with_default("https://example.com/a.mp4，"),
            Some("https://example.com/a.mp4".into())
        );
        // A URL followed by commentary: first token wins.
        assert_eq!(
            detect_with_default("https://example.com/a.bin (from chat)"),
            Some("https://example.com/a.bin".into())
        );
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(detect_with_default("/home/user/file.iso"), None);
        assert_eq!(detect_with_default("file:///etc/passwd"), None);
        assert_eq!(detect_with_default("javascript:alert(1)"), None);
        assert_eq!(detect_with_default("hello world"), None);
        assert_eq!(detect_with_default(""), None);
        assert_eq!(detect_with_default("see https://example.com"), None);
    }
}
