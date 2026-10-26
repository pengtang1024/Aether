use aether_plugin_media::MediaPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(MediaPlugin::new()) {
        eprintln!("aether-plugin-media: {error}");
        std::process::exit(1);
    }
}
