//! Filesystem helpers shared by plugins.

use std::fs::File;
use std::io;

/// Write all bytes at an absolute offset, retrying short writes.
#[cfg(unix)]
pub fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buffer.is_empty() {
        let written = file.write_at(buffer, offset)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write file chunk",
            ));
        }
        buffer = &buffer[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(windows)]
pub fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        let written = file.seek_write(buffer, offset)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write file chunk",
            ));
        }
        buffer = &buffer[written..];
        offset += written as u64;
    }
    Ok(())
}

/// Sanitize a remote filename for use inside a task's staging directory.
///
/// Drops directory components, control characters, Windows-reserved characters
/// and reserved device names. Returns `None` for empty, `.` or `..`.
pub fn sanitize_filename(name: &str) -> Option<String> {
    let name = name.rsplit(['/', '\\']).next()?.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    let mut filtered: String = name
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                ch
            }
        })
        .collect();

    if filtered.len() > 200 {
        filtered.truncate(200);
        while !filtered.is_char_boundary(filtered.len()) {
            filtered.pop();
        }
    }

    let stem = filtered
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        filtered.insert(0, '_');
    }

    Some(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_paths_and_reserved_names() {
        assert_eq!(sanitize_filename("a/b.txt").as_deref(), Some("b.txt"));
        assert_eq!(sanitize_filename("..").as_deref(), None);
        assert_eq!(sanitize_filename("CON").as_deref(), Some("_CON"));
        assert_eq!(sanitize_filename("a?b.txt").as_deref(), Some("a_b.txt"));
    }
}
