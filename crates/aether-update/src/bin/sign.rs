//! Sign an update manifest with an offline Ed25519 key.
//!
//! AETHER_UPDATE_SECRET_KEY=<base64 32-byte seed> aether-update-sign update.json
//! writes update.json.sig (base64 raw signature).

use std::fs;
use std::process::ExitCode;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: AETHER_UPDATE_SECRET_KEY=<base64> aether-update-sign <manifest>");
        return ExitCode::FAILURE;
    };
    let secret = match std::env::var("AETHER_UPDATE_SECRET_KEY") {
        Ok(secret) => secret,
        Err(_) => {
            eprintln!("AETHER_UPDATE_SECRET_KEY is not set");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(secret.trim()) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("secret key base64: {error}");
            return ExitCode::FAILURE;
        }
    };
    let seed: [u8; 32] = match bytes.try_into() {
        Ok(seed) => seed,
        Err(_) => {
            eprintln!("secret key must be 32 bytes");
            return ExitCode::FAILURE;
        }
    };
    let signing = SigningKey::from_bytes(&seed);
    let manifest = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let signature = signing.sign(&manifest);
    let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    let signature_path = format!("{path}.sig");
    if let Err(error) = fs::write(&signature_path, encoded) {
        eprintln!("write {signature_path}: {error}");
        return ExitCode::FAILURE;
    }
    let public =
        base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes());
    println!("wrote {signature_path}");
    println!("public key: {public}");
    ExitCode::SUCCESS
}
