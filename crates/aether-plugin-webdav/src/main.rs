use aether_plugin_sdk::run_stdio;
use aether_plugin_webdav::WebdavPlugin;

fn main() {
    if let Err(error) = run_stdio(WebdavPlugin::new()) {
        eprintln!("aether-plugin-webdav: {error}");
        std::process::exit(1);
    }
}
