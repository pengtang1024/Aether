#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use aether_daemon::{run, DaemonConfig};
    use std::path::PathBuf;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--version") {
        println!("aether-daemon {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    let mut config = DaemonConfig::default();
    if let Some(index) = args.iter().position(|arg| arg == "--download-dir") {
        if let Some(value) = args.get(index + 1) {
            config.download_dir = PathBuf::from(value);
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--plugins") {
        if let Some(value) = args.get(index + 1) {
            config.plugins_dir = PathBuf::from(value);
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--socket") {
        if let Some(value) = args.get(index + 1) {
            config.socket = PathBuf::from(value);
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--update-url") {
        if let Some(value) = args.get(index + 1) {
            config.update_url = Some(value.clone());
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--update-key") {
        if let Some(value) = args.get(index + 1) {
            config.update_public_key = Some(value.clone());
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--update-appimage") {
        if let Some(value) = args.get(index + 1) {
            config.update_appimage = Some(PathBuf::from(value));
        }
    }
    match run(config) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether-daemon: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!(
        "aether-daemon: Unix sockets are required; Windows named pipes are not implemented yet"
    );
    std::process::exit(1);
}
