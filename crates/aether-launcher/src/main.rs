//! Verified launcher for Aether.
//!
//! The `.deb` installs this binary at `/usr/bin/aether`, `/usr/bin/aether-daemon`,
//! `/usr/bin/aether-cli` and `/usr/bin/aether-native-host`, and moves the real
//! programs into `/usr/lib/aether/seed/`. Updates land in
//! `~/.local/share/aether/versions/<version>/` (written by the daemon, verified
//! against the signed release manifest), so a normal user can replace the
//! running program without root — and therefore without an authorization
//! prompt on every update.
//!
//! Whichever copy the launcher starts is checked first: the user copy against
//! the Ed25519 signature inside its own `update.json`, the seed against the
//! package manager that put it there. The payload is opened with `O_PATH`, its
//! SHA-256 is read back through `/proc/self/fd/<n>` (the same inode that will
//! be executed) and it is started with `execveat(AT_EMPTY_PATH)`, which leaves
//! no window between "verified" and "executed".

mod exec;
mod verify;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use semver::Version;

/// Package-installed seed directory (root-owned, written by dpkg/apt).
const SEED_DIR: &str = "/usr/lib/aether/seed";
/// `$XDG_DATA_HOME/aether` when the launcher runs from a package install.
const USER_SUBDIR: &str = "aether";
const VERSIONS_DIR: &str = "versions";
const VERSION_FILE: &str = "version";
/// Prints the program that would run, then exits 0.
const PRINT_TARGET: &str = "--print-target";

/// Programs the package ships and the launcher is installed as.
const PROGRAMS: [&str; 4] = [
    "aether",
    "aether-daemon",
    "aether-cli",
    "aether-native-host",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether-launcher: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let program = program_name()?;
    let key = verify::release_key().map_err(|error| format!("内置公钥无效：{error}"))?;
    let mut candidates = candidates(&program);
    if candidates.is_empty() {
        return Err(format!(
            "找不到可执行的 {program}（查找路径：{} 与 {}）",
            seed_dir().display(),
            user_root().join(VERSIONS_DIR).display()
        ));
    }
    // Highest version first; on a tie the package-installed copy wins, because
    // it is root-owned and dpkg can account for it.
    candidates.sort_by(|left, right| {
        right
            .version
            .cmp(&left.version)
            .then_with(|| right.trusted.cmp(&left.trusted))
    });

    let args: Vec<_> = env::args_os().skip(1).collect();
    let print_only = args.iter().any(|arg| arg == PRINT_TARGET);
    let verbose = env::var_os("AETHER_LAUNCHER_VERBOSE").is_some();
    let mut failures = Vec::new();
    for candidate in candidates {
        let payload = if candidate.trusted {
            verify::open_trusted(&candidate.dir, &program)
        } else {
            verify::open_verified(&candidate.dir, &program, &key)
        };
        let payload = match payload {
            Ok(payload) => payload,
            Err(error) => {
                // Verbose builds explain every rejected candidate, not just
                // the ones that leave nothing to run: a stale user copy that
                // silently loses to the seed is the common support question.
                if verbose {
                    eprintln!(
                        "aether-launcher: 跳过 {} {}（{}）：{error}",
                        candidate.dir.display(),
                        candidate.version,
                        candidate.label
                    );
                }
                failures.push(format!("跳过 {}：{error}", candidate.label));
                continue;
            }
        };
        if verbose {
            eprintln!(
                "aether-launcher: {} {} ({}) sha256={}",
                payload.path.display(),
                candidate.version,
                candidate.label,
                payload.sha256
            );
        }
        if print_only {
            println!("{}", payload.path.display());
            return Ok(());
        }
        return Err(exec::spawn(&payload.file, &payload.path));
    }
    failures.push(format!("没有一份 {program} 通过校验"));
    Err(failures.join("；"))
}

/// Program this launcher was installed as; package symlinks and copies both
/// keep the name the user typed.
fn program_name() -> Result<String, String> {
    let argv0 = env::args_os()
        .next()
        .ok_or_else(|| "缺少 argv[0]".to_string())?;
    let name = Path::new(&argv0)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if PROGRAMS.contains(&name.as_str()) {
        return Ok(name);
    }
    // Installed under an unexpected name (a test harness, say): default to the
    // desktop program rather than refusing to start.
    Ok(PROGRAMS[0].to_string())
}

/// Where dpkg put the programs. Only a debug build honours `AETHER_SEED_DIR`:
/// a release launcher must not be talked into trusting a user-writable
/// "seed", since that path is trusted without a manifest check.
fn seed_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(dir) = env::var_os("AETHER_SEED_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(SEED_DIR)
}

fn candidates(program: &str) -> Vec<Candidate> {
    let mut found = Vec::new();
    let seed = seed_dir();
    if let Some(version) = read_version(&seed.join(VERSION_FILE)) {
        if seed.join(program).is_file() {
            found.push(Candidate {
                label: "系统种子",
                dir: seed,
                version,
                trusted: true,
            });
        }
    }
    let versions = user_root().join(VERSIONS_DIR);
    let Ok(entries) = fs::read_dir(&versions) else {
        return found;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(version) = read_version(&dir.join(VERSION_FILE))
            .or_else(|| entry.file_name().to_string_lossy().parse().ok())
        else {
            continue;
        };
        if !dir.join(program).is_file() {
            continue;
        }
        found.push(Candidate {
            label: "用户副本",
            dir,
            version,
            trusted: false,
        });
    }
    found
}

fn read_version(path: &Path) -> Option<Version> {
    let text = fs::read_to_string(path).ok()?;
    Version::parse(text.trim().trim_start_matches('v')).ok()
}

/// `$AETHER_RUNTIME_ROOT`, else `$XDG_DATA_HOME/aether`, else
/// `~/.local/share/aether` — the same directory the daemon installs into.
fn user_root() -> PathBuf {
    if let Some(root) = env::var_os("AETHER_RUNTIME_ROOT") {
        return PathBuf::from(root);
    }
    if let Some(data) = env::var_os("XDG_DATA_HOME") {
        if !data.is_empty() {
            return PathBuf::from(data).join(USER_SUBDIR);
        }
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share").join(USER_SUBDIR);
    }
    PathBuf::from("/nonexistent").join(USER_SUBDIR)
}

struct Candidate {
    label: &'static str,
    dir: PathBuf,
    version: Version,
    trusted: bool,
}
