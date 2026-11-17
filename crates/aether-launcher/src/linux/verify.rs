//! Signature and payload checks for the launcher.
//!
//! A user copy is trusted only when the release manifest stored beside it
//! verifies against the built-in key *and* the program's SHA-256 matches the
//! manifest entry for this program. The seed needs no manifest: dpkg/apt wrote
//! it as root, and the launcher cannot be tricked into executing arbitrary
//! seed bytes by that path.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Signed release manifest, released next to `update.json.sig`.
pub const MANIFEST_FILE: &str = "update.json";
/// Detached Ed25519 signature of the manifest bytes, base64.
pub const SIGNATURE_FILE: &str = "update.json.sig";
/// Official release key, embedded at build time. Public by design.
const RELEASE_KEY: &str = include_str!("../../../../packaging/release-public-key.txt");

#[derive(Debug)]
pub struct Payload {
    pub file: File,
    pub path: PathBuf,
    pub sha256: String,
}

/// Release verifying key from the embedded base64 (32 raw bytes).
pub fn release_key() -> Result<VerifyingKey, String> {
    decode_key(RELEASE_KEY.trim())
}

fn decode_key(text: &str) -> Result<VerifyingKey, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|error| format!("public key base64: {error}"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "public key must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&bytes).map_err(|error| format!("public key: {error}"))
}

/// Open a user copy after checking the signed manifest stored beside it.
pub fn open_verified(dir: &Path, program: &str, key: &VerifyingKey) -> Result<Payload, String> {
    let manifest_bytes = fs::read(dir.join(MANIFEST_FILE))
        .map_err(|error| format!("读取 {MANIFEST_FILE} 失败：{error}"))?;
    let signature = fs::read_to_string(dir.join(SIGNATURE_FILE))
        .map_err(|error| format!("读取 {SIGNATURE_FILE} 失败：{error}"))?;
    verify_signature(&manifest_bytes, &signature, key)?;

    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("{MANIFEST_FILE} 不是合法 JSON：{error}"))?;
    // The directory name orders candidates before verification; the manifest
    // inside has to agree with it, so a stale or relabelled directory cannot
    // win the sort.
    let dir_version = dir
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let manifest_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{MANIFEST_FILE} 缺少 version"))?;
    if manifest_version.trim() != dir_version.trim() {
        return Err(format!(
            "清单版本 {manifest_version} 与目录名 {dir_version} 不一致"
        ));
    }
    let expected = manifest_sha256(&manifest, program)
        .ok_or_else(|| format!("清单里没有 {program} 的运行体哈希"))?;

    let path = dir.join(program);
    let file = open_payload(&path)?;
    let sha256 = hash_open_file(&file)?;
    if !sha256.eq_ignore_ascii_case(&expected) {
        return Err("运行体 sha256 与签名清单不一致".to_string());
    }
    Ok(Payload { file, path, sha256 })
}

/// Open a package-installed program without a manifest: the seed directory is
/// root-owned, so its contents are as trustworthy as the `.deb` that shipped
/// them.
pub fn open_trusted(dir: &Path, program: &str) -> Result<Payload, String> {
    let path = dir.join(program);
    let file = open_payload(&path)?;
    let sha256 = hash_open_file(&file)?;
    Ok(Payload { file, path, sha256 })
}

/// SHA-256 recorded for `program` in the manifest's `runtimes` map. Every
/// platform entry is searched: a match on any signed entry is a match on the
/// release's own bytes, and a platform mismatch cannot produce one.
fn manifest_sha256(manifest: &serde_json::Value, program: &str) -> Option<String> {
    let runtimes = manifest.get("runtimes")?.as_object()?;
    for bundle in runtimes.values() {
        let Some(files) = bundle.get("files").and_then(serde_json::Value::as_object) else {
            continue;
        };
        if let Some(sha) = files
            .get(program)
            .and_then(|entry| entry.get("sha256"))
            .and_then(serde_json::Value::as_str)
        {
            return Some(sha.to_string());
        }
    }
    None
}

fn verify_signature(
    manifest: &[u8],
    signature_text: &str,
    key: &VerifyingKey,
) -> Result<(), String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_text.trim())
        .map_err(|error| format!("签名不是合法 base64：{error}"))?;
    let signature =
        Signature::from_slice(&bytes).map_err(|error| format!("签名格式错误：{error}"))?;
    key.verify_strict(manifest, &signature)
        .map_err(|_| "发布签名校验失败".to_string())
}

/// Open the payload with `O_PATH`: the handle cannot read data but it can be
/// hashed through `/proc/self/fd` and passed to `execveat`.
fn open_payload(path: &Path) -> Result<File, String> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(path)
        .map_err(|error| format!("打开 {} 失败：{error}", path.display()))
}

/// SHA-256 of exactly the inode behind `file`, read back through
/// `/proc/self/fd/<n>`. Path-based hashing would leave a window for the file
/// to be replaced between check and exec.
fn hash_open_file(file: &File) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    let fd_path = format!("/proc/self/fd/{}", file.as_raw_fd());
    let mut handle =
        File::open(&fd_path).map_err(|error| format!("读取 {fd_path} 失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = handle
            .read(&mut buffer)
            .map_err(|error| format!("读取 {fd_path} 失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const TEST_SEED: [u8; 32] = [7; 32];

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_SEED)
    }

    fn payload(dir: &Path, program: &str, contents: &[u8]) -> String {
        let path = dir.join(program);
        fs::write(&path, contents).expect("payload written");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("mode set");
        hex_sha256(contents)
    }

    fn hex_sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Writes a signed manifest describing `files` (program → payload bytes).
    fn signed_manifest(dir: &Path, version: &str, files: &[(&str, &[u8])]) {
        let mut entries = serde_json::Map::new();
        for (name, contents) in files {
            entries.insert(
                (*name).to_string(),
                serde_json::json!({
                    "url": format!("https://example.test/{name}"),
                    "sha256": hex_sha256(contents),
                }),
            );
        }
        let manifest = serde_json::json!({
            "version": version,
            "runtimes": {
                "linux-x86_64": { "files": entries },
            },
        });
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest encodes");
        let signature = signing_key().sign(&bytes);
        use base64::Engine as _;
        let signature = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
        fs::write(dir.join(MANIFEST_FILE), &bytes).expect("manifest written");
        fs::write(dir.join(SIGNATURE_FILE), signature).expect("signature written");
    }

    fn version_dir(root: &Path, version: &str) -> PathBuf {
        let dir = root.join(version);
        fs::create_dir_all(&dir).expect("version dir");
        dir
    }

    #[test]
    fn verifies_signed_copy() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = version_dir(root.path(), "0.7.0");
        let hash = payload(&dir, "aether", b"new-aether");
        signed_manifest(&dir, "0.7.0", &[("aether", b"new-aether")]);

        let opened = open_verified(&dir, "aether", &signing_key().verifying_key())
            .expect("signed copy verifies");
        assert_eq!(opened.sha256, hash);
    }

    #[test]
    fn rejects_tampered_payload() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = version_dir(root.path(), "0.7.0");
        payload(&dir, "aether", b"original-aether");
        signed_manifest(&dir, "0.7.0", &[("aether", b"original-aether")]);
        fs::write(dir.join("aether"), b"swapped-aether").expect("payload swapped");

        let error = open_verified(&dir, "aether", &signing_key().verifying_key())
            .expect_err("tampered payload refused");
        assert!(error.contains("sha256"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_tampered_manifest() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = version_dir(root.path(), "0.7.0");
        payload(&dir, "aether", b"real-aether");
        signed_manifest(&dir, "0.7.0", &[("aether", b"real-aether")]);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(MANIFEST_FILE)).expect("manifest"))
                .expect("json");
        manifest["version"] = serde_json::json!("9.9.9");
        fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).expect("encode"),
        )
        .expect("manifest rewritten");

        let error = open_verified(&dir, "aether", &signing_key().verifying_key())
            .expect_err("rewritten manifest refused");
        assert!(error.contains("签名"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_directory_that_disagrees_with_manifest() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = version_dir(root.path(), "9.0.0");
        payload(&dir, "aether", b"old-aether");
        signed_manifest(&dir, "0.7.0", &[("aether", b"old-aether")]);

        let error = open_verified(&dir, "aether", &signing_key().verifying_key())
            .expect_err("relabelled directory refused");
        assert!(error.contains("不一致"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_copy_without_manifest() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = version_dir(root.path(), "0.7.0");
        payload(&dir, "aether", b"unsigned-aether");

        open_verified(&dir, "aether", &signing_key().verifying_key())
            .expect_err("missing manifest refused");
    }

    #[test]
    fn trusts_package_seed_without_manifest() {
        let root = tempfile::tempdir().expect("temp dir");
        payload(root.path(), "aether", b"seed-aether");

        let opened = open_trusted(root.path(), "aether").expect("seed opens");
        assert_eq!(opened.sha256, hex_sha256(b"seed-aether"));
    }
}
