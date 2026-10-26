use aether_plugin_cloud::{CloudPlugin, Provider};
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(CloudPlugin::new(Provider::Dropbox)) {
        eprintln!("aether-plugin-dropbox: {error}");
        std::process::exit(1);
    }
}
