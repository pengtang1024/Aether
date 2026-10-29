#!/usr/bin/env python3
"""Generate an update.json manifest from release artifacts.

Usage:
    scripts/make-update-manifest.py --dist dist --version 0.3.0 \
        [--out dist/update.json] [--channel stable] [--rollout 100] \
        [--base-url https://github.com/<owner>/<repo>/releases/download/v0.3.0] \
        [--notes-url https://github.com/<owner>/<repo>/releases/tag/v0.3.0]

Platform keys match aether_plugin_sdk::current_platform_key(). One asset per
key: when a platform ships both a .deb and an AppImage, the AppImage wins
(the updater can swap it in place) and the .deb remains a release asset for
package managers. Windows setup .exe files are release assets but not
manifest entries — the portable zip covers that platform key.

The manifest is only trustworthy once signed (aether-update-sign); the
release workflow drops it when no signing key is configured.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_BASE = "https://github.com/pengtang1024/Aether/releases/download/v{version}"
DEFAULT_NOTES = "https://github.com/pengtang1024/Aether/releases/tag/v{version}"

# (pattern, platform key, kind). First match wins: AppImage leads the .deb
# for the same key because the updater can swap it in place.
# Higher rank wins for the same platform key.
KIND_RANK = {"deb": 0, "zip": 0, "dmg": 0, "appimage": 1}

PATTERNS = [
    (re.compile(r"^aether-(?P<version>[^_]+)-linux-x86_64\.AppImage$"), "linux-x86_64", "appimage"),
    (re.compile(r"^aether-(?P<version>[^_]+)-linux-aarch64\.AppImage$"), "linux-aarch64", "appimage"),
    (re.compile(r"^aether_(?P<version>[^_]+)_amd64\.deb$"), "linux-x86_64", "deb"),
    (re.compile(r"^aether_(?P<version>[^_]+)_arm64\.deb$"), "linux-aarch64", "deb"),
    (re.compile(r"^aether-(?P<version>[^_]+)-windows-x86_64\.zip$"), "windows-x86_64", "zip"),
    (re.compile(r"^aether-(?P<version>[^_]+)-windows-arm64\.zip$"), "windows-arm64", "zip"),
    (re.compile(r"^aether-(?P<version>[^_]+)-macos-universal\.dmg$"), "macos-universal", "dmg"),
]


def digest(path: Path) -> tuple[str, int]:
    hasher = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            hasher.update(chunk)
            size += len(chunk)
    return hasher.hexdigest(), size


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dist", default="dist")
    parser.add_argument("--version", required=True)
    parser.add_argument("--out")
    parser.add_argument("--channel", default="stable")
    parser.add_argument("--rollout", type=int, default=100)
    parser.add_argument("--base-url")
    parser.add_argument("--notes-url")
    args = parser.parse_args()

    if not 0 <= args.rollout <= 100:
        print("rollout must be 0-100", file=sys.stderr)
        return 1

    dist = Path(args.dist)
    if not dist.is_dir():
        print(f"missing dist directory: {dist}", file=sys.stderr)
        return 1
    base = (args.base_url or DEFAULT_BASE).format(version=args.version).rstrip("/")
    notes = (args.notes_url or DEFAULT_NOTES).format(version=args.version)

    assets: dict[str, dict] = {}
    for path in sorted(dist.iterdir()):
        for pattern, key, kind in PATTERNS:
            match = pattern.match(path.name)
            if not match:
                continue
            found = match.group("version")
            if found != args.version:
                print(
                    f"{path.name}: version {found} does not match --version {args.version}",
                    file=sys.stderr,
                )
                return 1
            if key in assets and KIND_RANK.get(kind, 0) <= KIND_RANK.get(assets[key]["kind"], 0):
                # Keep the stronger asset for this key (AppImage can be
                # swapped in place; a .deb next to it is package-manager
                # material only).
                break
            sha256, size = digest(path)
            assets[key] = {
                "url": f"{base}/{path.name}",
                "sha256": sha256,
                "size": size,
                "kind": kind,
            }
            break

    if not assets:
        print(f"no release artifacts found in {dist}", file=sys.stderr)
        return 1

    manifest = {
        "version": args.version,
        "channel": args.channel,
        "published_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "notes_url": notes,
        "rollout": args.rollout,
        "assets": dict(sorted(assets.items())),
    }
    out = Path(args.out) if args.out else dist / "update.json"
    out.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {out} ({len(assets)} platform asset(s), rollout {args.rollout}%)")
    for key in sorted(assets):
        print(f"  {key}: {assets[key]['kind']} {assets[key]['size']} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
