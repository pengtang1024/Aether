use aether_plugin_s3::S3Plugin;
use aether_plugin_sdk::run_stdio;

fn main() {
    if let Err(error) = run_stdio(S3Plugin::new()) {
        eprintln!("aether-plugin-s3: {error}");
        std::process::exit(1);
    }
}
