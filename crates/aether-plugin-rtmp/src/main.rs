use aether_plugin_rtmp::RtmpPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(RtmpPlugin::new()) {
        eprintln!("aether-plugin-rtmp: {error}");
        std::process::exit(1);
    }
}
