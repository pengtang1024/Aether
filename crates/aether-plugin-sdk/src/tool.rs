//! Tools shipped inside a plugin package.
//!
//! Packaging puts optional helpers next to the plugin executable
//! (`bin/ffmpeg` beside `bin/aether-plugin-media-*`), so the plugin can
//! find them without knowing its install directory.

use std::path::{Path, PathBuf};

/// Path to `name` (plus `.exe` on Windows) beside the running plugin
/// executable, if the package ships it.
pub fn bundled(name: &str) -> Option<PathBuf> {
    bundled_in(std::env::current_exe().ok()?.parent()?, name)
}

fn bundled_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let candidate = dir.join(file);
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_files_that_exist() {
        let dir = std::env::temp_dir().join(format!("aether-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        std::fs::write(dir.join(name), b"fake").unwrap();
        assert_eq!(bundled_in(&dir, "ffmpeg"), Some(dir.join(name)));
        assert_eq!(bundled_in(&dir, "ffprobe"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
