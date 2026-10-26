#!/usr/bin/env python3
"""Stage first-party plugin packages for a release build.

Layout:
  build/plugins/<id>/plugin.json
  build/plugins/<id>/bin/<platform-binary>

The manifest's executable path is kept as-is, so the Host can resolve it
relative to the plugin package root.
"""

from __future__ import annotations

import argparse
import json
import platform
import shutil
import sys
from pathlib import Path

# (manifest path, built binary name)
PLUGINS = [
    ("crates/aether-plugin-http/plugin.json", "aether-plugin-http"),
    ("crates/aether-plugin-ftp/plugin.json", "aether-plugin-ftp"),
    ("crates/aether-plugin-media/plugin.json", "aether-plugin-media"),
    ("crates/aether-plugin-bt/plugin.json", "aether-plugin-bt"),
    ("crates/aether-plugin-webdav/plugin.json", "aether-plugin-webdav"),
    ("crates/aether-plugin-s3/plugin.json", "aether-plugin-s3"),
    ("crates/aether-plugin-sftp/plugin.json", "aether-plugin-sftp"),
    ("crates/aether-plugin-extractor/plugin.json", "aether-plugin-extractor"),
    ("crates/aether-plugin-thunder/plugin.json", "aether-plugin-thunder"),
    ("crates/aether-plugin-ipfs/plugin.json", "aether-plugin-ipfs"),
    ("crates/aether-plugin-rtmp/plugin.json", "aether-plugin-rtmp"),
    ("crates/aether-plugin-cloud/plugins/gdrive/plugin.json", "aether-plugin-gdrive"),
    ("crates/aether-plugin-cloud/plugins/onedrive/plugin.json", "aether-plugin-onedrive"),
    ("crates/aether-plugin-cloud/plugins/dropbox/plugin.json", "aether-plugin-dropbox"),
]


def platform_key() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if system == "linux":
        return "linux-aarch64" if machine in ("aarch64", "arm64") else "linux-x86_64"
    if system == "windows":
        return "windows-arm64" if machine in ("arm64", "aarch64") else "windows-x86_64"
    if system == "darwin":
        return "macos-universal"
    raise SystemExit(f"unsupported platform: {system}/{machine}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", default="target/release")
    parser.add_argument("--out", default="build/plugins")
    parser.add_argument("--platform-key")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    target_dir = (root / args.target_dir).resolve()
    out = (root / args.out).resolve()
    key = args.platform_key or platform_key()
    suffix = ".exe" if key.startswith("windows-") else ""
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    staged = 0
    for manifest_rel, binary_name in PLUGINS:
        manifest_path = root / manifest_rel
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        relative = manifest.get("executable", {}).get(key)
        if not relative:
            raise SystemExit(f"{manifest_rel}: no executable for {key}")
        source = target_dir / f"{binary_name}{suffix}"
        if not source.is_file():
            raise SystemExit(f"missing built plugin binary: {source}")
        package = out / manifest["id"]
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        destination.chmod(0o755)
        shutil.copy2(manifest_path, package / "plugin.json")
        shutil.copy2(root / "LICENSE", package / "LICENSE")
        staged += 1

    print(f"staged {staged} plugins for {key} in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
