use aether_plugin_sdk::run_stdio;
use aether_plugin_sftp::SftpPlugin;

fn main() {
    if let Err(error) = run_stdio(SftpPlugin::new()) {
        eprintln!("aether-plugin-sftp: {error}");
        std::process::exit(1);
    }
}
