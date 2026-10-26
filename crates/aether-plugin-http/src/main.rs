use aether_plugin_http::HttpPlugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(HttpPlugin::new()) {
        eprintln!("aether-plugin-http: {error}");
        std::process::exit(1);
    }
}
