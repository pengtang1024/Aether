//! Sign → verify roundtrip for plugin packages (ROADMAP 4.6a).
//!
//! Runs the real `aether-plugin-sign` binary, then verifies with the same
//! API the daemon uses: signature first, then file hashes. Canonical bytes
//! must survive a reformat/reorder of plugin.json, and any tampering must
//! fail one of the two checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aether_update::{canonical_manifest_bytes, verify_manifest_signature, verify_package_files};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;

fn temp_package(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-plugin-sign-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("bin").join("plugin"), b"#!/bin/sh\necho fake\n").unwrap();
    fs::write(
        dir.join("plugin.json"),
        r#"{
  "id": "demo",
  "name": "Demo plugin",
  "version": "1.2.3",
  "api": 2,
  "protocols": ["demo"],
  "capabilities": ["network"],
  "executable": { "linux-x86_64": "bin/plugin" }
}
"#,
    )
    .unwrap();
    dir
}

/// base64 seed for a fixed test key (tests only; never a real key).
fn test_seed() -> String {
    base64::engine::general_purpose::STANDARD.encode([7_u8; 32])
}

fn public_key() -> String {
    base64::engine::general_purpose::STANDARD.encode(
        SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .as_bytes(),
    )
}

fn sign(package: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_aether-plugin-sign"))
        .env("AETHER_UPDATE_SECRET_KEY", test_seed())
        .arg(package)
        .status()
        .unwrap();
    assert!(status.success(), "aether-plugin-sign failed");
}

fn read_manifest(package: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(package.join("plugin.json")).unwrap()).unwrap()
}

#[test]
fn signed_package_verifies_and_tampering_fails() {
    let package = temp_package("roundtrip");
    sign(&package);

    let manifest = read_manifest(&package);
    assert!(manifest["signature"].as_str().unwrap().len() > 60);
    assert_eq!(manifest["files"]["bin/plugin"].as_str().unwrap().len(), 64);
    // plugin.json itself is never listed: it carries the signature.
    assert!(manifest["files"].get("plugin.json").is_none());

    verify_manifest_signature(&manifest, &public_key()).unwrap();
    verify_package_files(&package, &manifest).unwrap();

    // Canonical bytes ignore formatting and key order: reformat the file
    // (reversed key order, no indentation) and the signature still holds.
    let raw = fs::read_to_string(package.join("plugin.json")).unwrap();
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    let reordered: serde_json::Map<String, Value> = value
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(key, val)| (key.clone(), val.clone()))
        .collect();
    *value.as_object_mut().unwrap() = reordered;
    fs::write(
        package.join("plugin.json"),
        serde_json::to_string(&value).unwrap(),
    )
    .unwrap();
    verify_manifest_signature(&read_manifest(&package), &public_key()).unwrap();

    // Tampering with the binary: signature still valid (manifest unchanged),
    // but the file hash check catches it.
    fs::write(
        package.join("bin").join("plugin"),
        b"#!/bin/sh\necho evil\n",
    )
    .unwrap();
    let manifest = read_manifest(&package);
    verify_manifest_signature(&manifest, &public_key()).unwrap();
    let error = verify_package_files(&package, &manifest).expect_err("tampered file must fail");
    assert!(error.to_string().contains("bin/plugin"), "{error}");

    // Tampering with the manifest itself: hash check passes (files are
    // unchanged), but the signature no longer matches.
    let mut manifest = read_manifest(&package);
    manifest["version"] = Value::String("9.9.9".into());
    let error = verify_manifest_signature(&manifest, &public_key()).expect_err("tampered manifest");
    assert!(error.to_string().contains("signature"), "{error}");

    fs::remove_dir_all(&package).ok();
}

#[test]
fn wrong_key_and_missing_pieces_fail() {
    let package = temp_package("wrong-key");

    // Unsigned manifest: no signature key at all.
    let manifest = read_manifest(&package);
    let error =
        verify_manifest_signature(&manifest, &public_key()).expect_err("unsigned must fail");
    assert!(error.to_string().contains("no signature"), "{error}");

    sign(&package);
    let manifest = read_manifest(&package);

    // A different key rejects the signature.
    let other = base64::engine::general_purpose::STANDARD.encode(
        SigningKey::from_bytes(&[9_u8; 32])
            .verifying_key()
            .as_bytes(),
    );
    assert!(verify_manifest_signature(&manifest, &other).is_err());

    // A manifest without a files map cannot claim binary integrity.
    let mut stripped = manifest.clone();
    stripped.as_object_mut().unwrap().remove("files");
    let error = verify_package_files(&package, &stripped).expect_err("no files map");
    assert!(error.to_string().contains("files"), "{error}");

    // A listed file that is missing from disk is an error, not a skip.
    let mut missing = manifest.clone();
    missing["files"]["bin/extra"] = Value::String("0".repeat(64));
    let error = verify_package_files(&package, &missing).expect_err("missing file");
    assert!(error.to_string().contains("bin/extra"), "{error}");

    // Unsafe paths in the files map are rejected outright.
    let mut traversal = manifest.clone();
    traversal["files"]["../escape"] = Value::String("0".repeat(64));
    assert!(verify_package_files(&package, &traversal).is_err());

    // Canonical bytes are identical for equal content in different shapes.
    let a: Value = serde_json::from_str(r#"{"id":"x","signature":"s"}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"signature":"s","id":"x"}"#).unwrap();
    assert_eq!(canonical_manifest_bytes(&a), canonical_manifest_bytes(&b));

    fs::remove_dir_all(&package).ok();
}
