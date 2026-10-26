use aether_plugin_ipfs::IpfsPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(IpfsPlugin::new()) {
        eprintln!("aether-plugin-ipfs: {error}");
        std::process::exit(1);
    }
}
