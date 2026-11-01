//! Filesystem identity for the staging layout.
//!
//! Unix answers with `st_dev`; Windows compares volume serial numbers. `std`'s
//! `volume_serial_number` is still unstable (`windows_by_handle`), so the
//! Windows path asks kernel32 directly — the same raw-FFI approach the tray
//! uses, which keeps the daemon dependency-free.

use std::path::Path;

/// Whether both paths live on one filesystem. Unknown metadata reports `true`,
/// which keeps the centralized staging root: copying at finalize is safer than
/// staging next to a target whose volume could not be identified.
pub(crate) fn same_filesystem(left: &Path, right: &Path) -> bool {
    match (volume_id(left), volume_id(right)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

#[cfg(unix)]
fn volume_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|metadata| metadata.dev())
}

#[cfg(windows)]
fn volume_id(path: &Path) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    // kernel32 is linked into every Windows process. Declaring the two calls
    // here avoids a winapi/windows-sys dependency for a single comparison.
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(path: *const u16, volume_path: *mut u16, len: u32) -> i32;
        fn GetVolumeInformationW(
            root: *const u16,
            volume_name: *mut u16,
            volume_name_len: u32,
            volume_serial: *mut u32,
            max_component_len: *mut u32,
            flags: *mut u32,
            filesystem_name: *mut u16,
            filesystem_name_len: u32,
        ) -> i32;
    }

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    // MAX_PATH is the documented bound for a volume root path.
    let mut root = vec![0u16; 261];
    let len = u32::try_from(root.len()).ok()?;
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), len) } == 0 {
        return None;
    }

    let mut serial = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            ptr::null_mut(),
            0,
            &mut serial,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    (ok != 0).then_some(serial)
}

#[cfg(test)]
mod tests {
    use super::same_filesystem;

    #[test]
    fn same_filesystem_is_reflexive() {
        let dir = std::env::temp_dir();
        assert!(same_filesystem(&dir, &dir));
    }

    #[test]
    fn same_filesystem_missing_paths_keep_centralized_staging() {
        let dir = std::env::temp_dir();
        let missing = dir.join("aether-missing-volume-target");
        assert!(
            same_filesystem(&dir, &missing),
            "unknown metadata must keep the centralized staging root"
        );
    }
}
