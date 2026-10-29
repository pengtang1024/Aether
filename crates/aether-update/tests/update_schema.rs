//! Update-manifest schema compatibility and plugin-archive unpack guards
//! (ROADMAP 4.6b).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aether_update::{unpack_plugin_archive, UpdateError, UpdateManifest};

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-update-schema-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A manifest as published before 4.6b: assets only, no plugins key.
const OLD_MANIFEST: &str = r#"{
  "version": "0.3.0",
  "channel": "stable",
  "assets": {
    "linux-x86_64": { "url": "https://example.test/aether.AppImage", "sha256": "aa", "kind": "appimage" }
  }
}"#;

const NEW_MANIFEST: &str = r#"{
  "version": "0.3.0",
  "assets": {},
  "plugins": {
    "http": { "version": "0.2.0", "url": "https://example.test/http.tar.gz", "sha256": "bb" }
  }
}"#;

#[test]
fn manifests_without_plugins_still_parse() {
    let manifest: UpdateManifest = serde_json::from_str(OLD_MANIFEST).unwrap();
    assert_eq!(manifest.version, "0.3.0");
    assert!(manifest.plugins.is_empty(), "old manifests keep parsing");
    assert_eq!(manifest.assets.len(), 1);
}

#[test]
fn plugin_entries_parse() {
    let manifest: UpdateManifest = serde_json::from_str(NEW_MANIFEST).unwrap();
    let http = &manifest.plugins["http"];
    assert_eq!(http.version, "0.2.0");
    assert_eq!(http.size, None);
}

/// Build a .tar.gz from (name, contents, kind) triples. `kind` 0 = file,
/// 7 = symlink (used for the hostile-archive cases).
fn build_archive(dir: &Path, entries: &[(&str, &str, u8)]) -> PathBuf {
    let archive_path = dir.join("package.tar.gz");
    let file = fs::File::create(&archive_path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, contents, kind) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        if *kind == 7 {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/etc/passwd").unwrap();
        }
        builder
            .append_data(&mut header, name, contents.as_bytes())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    archive_path
}

/// Hand-rolled single-entry tar.gz. The tar Builder refuses `..` paths —
/// which is exactly what the hostile cases need, so craft the ustar header
/// by hand (the parser, unlike the builder, accepts whatever it is given
/// and must defend itself).
fn raw_archive_with_path(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let mut header = vec![0_u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size = format!("{:011o}\0", contents.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|byte| *byte as u32).sum();
    let checksum = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(checksum.as_bytes());

    let path = dir.join("raw.tar.gz");
    let file = fs::File::create(&path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&header).unwrap();
    encoder.write_all(contents).unwrap();
    let padding = (512 - contents.len() % 512) % 512;
    encoder.write_all(&vec![0_u8; padding]).unwrap();
    encoder.write_all(&vec![0_u8; 1024]).unwrap();
    encoder.finish().unwrap();
    path
}

#[test]
fn unpacks_plain_packages() {
    let dir = temp_dir("plain");
    let archive = build_archive(
        &dir,
        &[
            ("plugin.json", r#"{"id":"demo"}"#, 0),
            ("bin/plugin", "binary", 0),
        ],
    );
    let out = dir.join("out");
    unpack_plugin_archive(&archive, &out, 1024 * 1024).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("plugin.json")).unwrap(),
        r#"{"id":"demo"}"#
    );
    assert_eq!(
        fs::read_to_string(out.join("bin/plugin")).unwrap(),
        "binary"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn hostile_archives_are_rejected() {
    // Path traversal.
    let dir = temp_dir("traversal");
    let archive = raw_archive_with_path(&dir, "../escape", b"pwned");
    let error =
        unpack_plugin_archive(&archive, &dir.join("out"), 1024 * 1024).expect_err("traversal");
    assert!(error.to_string().contains("escapes"), "{error}");
    assert!(!dir.join("escape").exists());
    fs::remove_dir_all(&dir).ok();

    // Absolute path.
    let dir = temp_dir("absolute");
    let archive = raw_archive_with_path(&dir, "/tmp/aether-absolute-pwn", b"pwned");
    let error =
        unpack_plugin_archive(&archive, &dir.join("out"), 1024 * 1024).expect_err("absolute");
    assert!(error.to_string().contains("escapes"), "{error}");
    fs::remove_dir_all(&dir).ok();

    // Symlink entries are never needed by a plugin package.
    let dir = temp_dir("symlink");
    let archive = build_archive(&dir, &[("bin/plugin", "", 7)]);
    let error =
        unpack_plugin_archive(&archive, &dir.join("out"), 1024 * 1024).expect_err("symlink entry");
    assert!(error.to_string().contains("non-regular"), "{error}");
    fs::remove_dir_all(&dir).ok();

    // Size cap: declared sizes count before unpacking.
    let dir = temp_dir("bomb");
    let big = "x".repeat(4096);
    let archive = build_archive(&dir, &[("big", &big, 0)]);
    let error = unpack_plugin_archive(&archive, &dir.join("out"), 1024).expect_err("oversized");
    assert!(error.to_string().contains("larger than"), "{error}");
    fs::remove_dir_all(&dir).ok();

    // Not a gzip stream at all.
    let dir = temp_dir("garbage");
    let garbage = dir.join("garbage.tar.gz");
    fs::write(&garbage, b"not gzip").unwrap();
    assert!(unpack_plugin_archive(&garbage, &dir.join("out"), 1024).is_err());
    fs::remove_dir_all(&dir).ok();
}
