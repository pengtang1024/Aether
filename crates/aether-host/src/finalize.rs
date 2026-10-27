//! Host-owned finalization: validate staging files and move them to the final
//! directory. Plugins never write to the final directory.

use std::fs;
use std::path::{Component, Path, PathBuf};

use aether_plugin_sdk::{ChecksumAlgorithm, FileInfo};

use crate::error::HostError;

pub fn finalize_task(
    staging: &Path,
    final_dir: &Path,
    files: &[FileInfo],
) -> Result<Vec<PathBuf>, HostError> {
    fs::create_dir_all(final_dir)?;
    let canonical_staging = staging.canonicalize()?;
    let mut moved = Vec::new();

    for file in files {
        let relative = safe_relative(&file.relative_path)?;
        let source = staging.join(&relative);
        let canonical_source = source.canonicalize()?;
        if !canonical_source.starts_with(&canonical_staging) {
            return Err(HostError::UnsafePath(file.relative_path.clone()));
        }

        let actual = fs::metadata(&canonical_source)?.len();
        if actual != file.size {
            return Err(HostError::SizeMismatch {
                path: file.relative_path.clone(),
                expected: file.size,
                actual,
            });
        }

        if let Some(checksum) = &file.checksum {
            if checksum.algorithm != ChecksumAlgorithm::Sha256 {
                return Err(HostError::Protocol(
                    "only SHA-256 finalization is supported".into(),
                ));
            }
            let actual = aether_plugin_sdk::checksum::sha256_file(&canonical_source)?;
            if !actual.eq_ignore_ascii_case(&checksum.value) {
                return Err(HostError::ChecksumMismatch(file.relative_path.clone()));
            }
        }

        let destination = unique_destination(final_dir, &relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        move_file(&canonical_source, &destination)?;
        moved.push(destination);
    }

    Ok(moved)
}

/// Never overwrite existing files: `report.pdf` becomes `report-1.pdf`,
/// `report-2.pdf`, … The rename applies to the last path segment so nested
/// files keep their directory layout.
fn unique_destination(final_dir: &Path, relative: &Path) -> PathBuf {
    let destination = final_dir.join(relative);
    if !destination.exists() {
        return destination;
    }
    let parent = destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let file_name = destination
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    let stem = Path::new(&file_name)
        .file_stem()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| file_name.clone());
    let extension = Path::new(&file_name)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    for index in 1..1000_u32 {
        let candidate = parent.join(format!("{}-{index}{}", stem.to_string_lossy(), extension));
        if !candidate.exists() {
            return candidate;
        }
    }
    destination
}

fn safe_relative(value: &str) -> Result<PathBuf, HostError> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(HostError::UnsafePath(value.to_string()));
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            _ => return Err(HostError::UnsafePath(value.to_string())),
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(HostError::UnsafePath(value.to_string()));
    }
    Ok(safe)
}

fn move_file(source: &Path, destination: &Path) -> Result<(), HostError> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            fs::copy(source, destination)?;
            fs::File::open(destination)?.sync_all()?;
            fs::remove_file(source)?;
            Ok(())
        }
        Err(error) => Err(HostError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin_sdk::{Checksum, FileInfo};

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aether-finalize-{label}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_and_verifies_a_file() {
        let root = temp_dir("ok");
        let staging = root.join("staging");
        let final_dir = root.join("final");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("a.bin"), b"hello").unwrap();
        let files = vec![FileInfo {
            relative_path: "a.bin".into(),
            size: 5,
            checksum: Some(Checksum {
                algorithm: ChecksumAlgorithm::Sha256,
                value: aether_plugin_sdk::checksum::sha256_bytes(b"hello"),
            }),
        }];
        let moved = finalize_task(&staging, &final_dir, &files).unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(fs::read(final_dir.join("a.bin")).unwrap(), b"hello");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_dir("traversal");
        let staging = root.join("staging");
        let final_dir = root.join("final");
        fs::create_dir_all(&staging).unwrap();
        let files = vec![FileInfo {
            relative_path: "../escape".into(),
            size: 0,
            checksum: None,
        }];
        assert!(matches!(
            finalize_task(&staging, &final_dir, &files),
            Err(HostError::UnsafePath(_))
        ));
        fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    #[test]
    fn renames_on_conflict() {
        let root =
            std::env::temp_dir().join(format!("aether-finalize-conflict-{}", std::process::id()));
        let final_dir = root.join("final");
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("a.bin"), b"old").unwrap();
        assert_eq!(
            unique_destination(&final_dir, Path::new("a.bin")),
            final_dir.join("a-1.bin")
        );
        fs::write(final_dir.join("a-1.bin"), b"old").unwrap();
        assert_eq!(
            unique_destination(&final_dir, Path::new("a.bin")),
            final_dir.join("a-2.bin")
        );
        // No extension and no conflict keep the plain name.
        assert_eq!(
            unique_destination(&final_dir, Path::new("nested/b.bin")),
            final_dir.join("nested/b.bin")
        );
        fs::remove_dir_all(root).ok();
    }
}
