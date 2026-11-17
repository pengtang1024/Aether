//! Verified launcher for Aether, installed by the `.deb` at `/usr/bin/aether`,
//! `/usr/bin/aether-daemon`, `/usr/bin/aether-cli` and
//! `/usr/bin/aether-native-host`.
//!
//! The launcher picks the newest copy of a program — the package seed in
//! `/usr/lib/aether/seed` or a user-space copy in
//! `~/.local/share/aether/versions/<version>` — verifies it (signature and
//! payload hash for a user copy; the package manager's word for the seed) and
//! then replaces its own process with it via `execveat`. Because updates are
//! written by the user, an update never needs root and never opens an
//! authorization prompt.
//!
//! Only Linux packages use a launcher: the Windows and macOS builds ship their
//! programs directly. The crate therefore compiles to a stub elsewhere, so
//! `cargo build --workspace`, `cargo test --workspace` and `cargo clippy
//! --workspace` stay green on every runner.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("aether-launcher: only the Linux package installs a launcher");
    std::process::ExitCode::FAILURE
}
