fn main() -> std::process::ExitCode {
    use std::io::{stdin, stdout};

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--version") {
        println!("aether-native-host {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--url") {
        let Some(url) = args.get(index + 1) else {
            eprintln!("--url needs a value");
            return std::process::ExitCode::FAILURE;
        };
        let response = aether_native_host::handle_message(&serde_json::json!({
            "type": "add",
            "url": url,
        }));
        if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            println!("{}", response);
            return std::process::ExitCode::SUCCESS;
        }
        eprintln!("{}", response);
        return std::process::ExitCode::FAILURE;
    }

    let mut input = stdin();
    let mut output = stdout();
    while let Ok(Some(message)) = aether_native_host::read_message(&mut input) {
        let response = aether_native_host::handle_message(&message);
        if aether_native_host::write_message(&mut output, &response).is_err() {
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
