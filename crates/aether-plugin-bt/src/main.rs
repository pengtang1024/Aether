use aether_plugin_bt::BtPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(BtPlugin::new()) {
        eprintln!("aether-plugin-bt: {error}");
        std::process::exit(1);
    }
}
