use aether_plugin_baidupan::BaiduPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(BaiduPlugin::new()) {
        eprintln!("aether-plugin-baidupan: {error}");
        std::process::exit(1);
    }
}
