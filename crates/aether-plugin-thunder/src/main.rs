use aether_plugin_sdk::run_stdio;
use aether_plugin_thunder::ThunderPlugin;

fn main() {
    if let Err(error) = run_stdio(ThunderPlugin::new()) {
        eprintln!("aether-plugin-thunder: {error}");
        std::process::exit(1);
    }
}
