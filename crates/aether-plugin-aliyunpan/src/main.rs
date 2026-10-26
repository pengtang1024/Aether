use aether_plugin_aliyunpan::AliyunPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(AliyunPlugin::new()) {
        eprintln!("aether-plugin-aliyunpan: {error}");
        std::process::exit(1);
    }
}
