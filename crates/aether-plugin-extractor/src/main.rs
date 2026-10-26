use aether_plugin_extractor::ExtractorPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(ExtractorPlugin::new()) {
        eprintln!("aether-plugin-extractor: {error}");
        std::process::exit(1);
    }
}
