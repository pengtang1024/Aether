//! Replace the launcher process with a payload that is already open and
//! verified.
//!
//! `execveat(fd, "", …, AT_EMPTY_PATH)` executes exactly the inode the
//! launcher hashed, so there is no path to swap between the check and the
//! execution — the classic TOCTOU hole in "verify this file, then run it".

use std::env;
use std::ffi::{c_char, CString, OsString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Environment markers the payload reads back. `AETHER_LAUNCHER` is how the
/// daemon knows this install is package-managed and may update into user
/// space.
pub const LAUNCHER_MARKER: &str = "AETHER_LAUNCHER";
pub const TARGET_MARKER: &str = "AETHER_RUNTIME_TARGET";

/// Executes `file`; only returns (with the OS error) when the exec fails.
pub fn spawn(file: &File, display: &Path) -> String {
    let args: Vec<OsString> = env::args_os().collect();
    let mut argv = Vec::with_capacity(args.len() + 1);
    // argv[0] is the real program path: the payload runs as if it had been
    // started directly, which keeps diagnostics readable.
    argv.push(to_c_string(display.as_os_str()));
    for arg in args.iter().skip(1) {
        argv.push(to_c_string(arg));
    }
    let mut argv_ptrs: Vec<*mut c_char> = argv.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    argv_ptrs.push(std::ptr::null_mut());

    let environment: Vec<CString> = payload_environment(display)
        .iter()
        .map(|pair| to_c_string(pair))
        .collect();
    let mut env_ptrs: Vec<*mut c_char> = environment
        .iter()
        .map(|entry| entry.as_ptr().cast_mut())
        .collect();
    env_ptrs.push(std::ptr::null_mut());

    let empty = c"";
    // SAFETY: every pointer array is NULL-terminated and stays alive across the
    // call; `file` is open for the whole call. On success this never returns.
    unsafe {
        libc::execveat(
            file.as_raw_fd(),
            empty.as_ptr(),
            argv_ptrs.as_ptr(),
            env_ptrs.as_ptr(),
            libc::AT_EMPTY_PATH,
        );
    }
    format!(
        "启动 {} 失败：{}",
        display.display(),
        std::io::Error::last_os_error()
    )
}

/// The caller's environment with the launcher markers replaced rather than
/// duplicated (a payload may itself start programs through the launcher).
fn payload_environment(display: &Path) -> Vec<OsString> {
    let mut pairs: Vec<OsString> = Vec::new();
    let mut marker = OsString::from(LAUNCHER_MARKER);
    marker.push("=1");
    pairs.push(marker);
    let mut target = OsString::from(TARGET_MARKER);
    target.push("=");
    target.push(display.as_os_str());
    pairs.push(target);
    for (key, value) in env::vars_os() {
        if key == LAUNCHER_MARKER || key == TARGET_MARKER {
            continue;
        }
        let mut pair = key;
        pair.push("=");
        pair.push(value);
        pairs.push(pair);
    }
    pairs
}

fn to_c_string(value: &std::ffi::OsStr) -> CString {
    use std::os::unix::ffi::OsStrExt;
    CString::new(value.as_bytes()).unwrap_or_else(|_| {
        // Arguments with interior NUL bytes cannot be passed through anyway;
        // dropping the byte keeps the rest of the command line intact.
        CString::new(value.to_string_lossy().replace('\0', ""))
            .unwrap_or_else(|_| CString::new("").expect("empty CString"))
    })
}
