use aether_plugin_ftp::FtpPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(FtpPlugin::new()) {
        eprintln!("aether-plugin-ftp: {error}");
        std::process::exit(1);
    }
}
