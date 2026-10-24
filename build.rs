// Build script for Aether.
//
// The UI lives in `ui/`. The Material 3 widget library is vendored under
// `ui/material/` and registered as the style "material", so the whole app —
// both the upstream widgets and Aether's own components — shares one
// brand-tinted Material palette (#6750A4).

fn main() {
    // The vendored Material 3 widget library uses a couple of experimental
    // language features (`interface`, …) that the upstream style relies on too.
    std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("material".into())
        .with_include_paths(vec![
            std::path::PathBuf::from("ui"),
            std::path::PathBuf::from("ui/material"),
        ]);

    slint_build::compile_with_config("ui/app.slint", config)
        .expect("failed to compile the Aether UI");
}
