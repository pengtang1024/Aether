//! Sign a plugin package's manifest with the offline release key.
//!
//! AETHER_UPDATE_SECRET_KEY=<base64 32-byte seed> aether-plugin-sign <package-dir>
//!
//! Walks the package (everything except `plugin.json`), records each file's
//! SHA-256 into the manifest's `files` map, then writes the Ed25519 signature
//! over the canonical manifest bytes (JSON with the `signature` key removed)
//! into `signature`. Re-running refreshes both: hashes for changed files, a
//! new signature for the changed manifest.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    let Some(package) = std::env::args().nth(1) else {
        eprintln!("usage: AETHER_UPDATE_SECRET_KEY=<base64> aether-plugin-sign <package-dir>");
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

    let package = Path::new(&package);
    let manifest_path = package.join("plugin.json");
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("read {}: {error}", manifest_path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut manifest: Value = match serde_json::from_str(&raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("parse plugin.json: {error}");
            return ExitCode::FAILURE;
        }
    };

    let files = match hash_package(package) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("hash package: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(object) = manifest.as_object_mut() else {
        eprintln!("plugin.json is not a JSON object");
        return ExitCode::FAILURE;
    };
    object.insert("files".into(), Value::Object(files));
    object.remove("signature");

    let canonical = aether_update::canonical_manifest_bytes(&manifest);
    let signing = SigningKey::from_bytes(&seed);
    let signature = signing.sign(&canonical);
    manifest["signature"] =
        Value::String(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()));

    let mut out = match serde_json::to_string_pretty(&manifest) {
        Ok(out) => out,
        Err(error) => {
            eprintln!("serialize plugin.json: {error}");
            return ExitCode::FAILURE;
        }
    };
    out.push('\n');
    if let Err(error) = fs::write(&manifest_path, out) {
        eprintln!("write {}: {error}", manifest_path.display());
        return ExitCode::FAILURE;
    }
    let public =
        base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes());
    println!("signed {}", manifest_path.display());
    println!("public key: {public}");
    ExitCode::SUCCESS
}

/// Every regular file under `package` except `plugin.json`, keyed by
/// package-relative path with forward slashes (stable across platforms).
fn hash_package(package: &Path) -> Result<serde_json::Map<String, Value>, String> {
    let mut out = serde_json::Map::new();
    walk(package, package, &mut out)?;
    if out.is_empty() {
        return Err("package contains no files besides plugin.json".into());
    }
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if relative == "plugin.json" {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        let mut digest = Sha256::new();
        digest.update(&bytes);
        let hash = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        out.insert(relative, Value::String(hash));
    }
    Ok(())
}
