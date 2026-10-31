#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.md: per-crate licence texts and notices.

This is the strict-attribution companion to THIRD_PARTY_INVENTORY.md. It walks
the normal (compiled-in) dependency graph of every workspace member, finds the
LICENSE/COPYING/NOTICE files shipped inside each crate, and reproduces them
verbatim. Identical texts are included once and referenced by later crates.

Usage:
    python3 scripts/generate-third-party-licenses.py           # write
    python3 scripts/generate-third-party-licenses.py --check   # CI drift check
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "THIRD_PARTY_LICENSES.md"
TARGETS = {
    "linux": "x86_64-unknown-linux-gnu",
    "windows": "x86_64-pc-windows-gnu",
    "macos": "aarch64-apple-darwin",
}
NAME_RE = re.compile(r"(?i)^(licen[cs]e|copying|notice|copyright)")
MAX_TEXT = 300 * 1024


def cargo_metadata() -> dict:
    env = dict(os.environ)
    env["RUSTC_WRAPPER"] = ""
    return json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--all-features"],
            cwd=ROOT,
            env=env,
        )
    )


def graph(target: str) -> set[tuple[str, str]]:
    env = dict(os.environ)
    env["RUSTC_WRAPPER"] = ""
    output = subprocess.check_output(
        [
            "cargo",
            "tree",
            "--workspace",
            "--all-features",
            "--target",
            target,
            "--edges",
            "normal",
            "--prefix",
            "none",
        ],
        cwd=ROOT,
        env=env,
        text=True,
        stderr=subprocess.DEVNULL,
    )
    found: set[tuple[str, str]] = set()
    for line in output.splitlines():
        line = line.split(" (*)")[0].strip()
        match = re.match(r"([A-Za-z0-9_.+-]+) v([0-9][^ ]*)", line)
        if match:
            found.add((match.group(1), match.group(2)))
    return found


def license_files(package_root: Path) -> list[Path]:
    found: list[Path] = []
    for current, dirs, files in os.walk(package_root):
        current_path = Path(current)
        relative = current_path.relative_to(package_root)
        if len(relative.parts) >= 5 or "vendor" in relative.parts:
            dirs[:] = []
            continue
        for name in files:
            path = current_path / name
            in_license_dir = any(part.lower() == "licenses" for part in relative.parts)
            if not (NAME_RE.match(name) or in_license_dir):
                continue
            try:
                if path.stat().st_size == 0 or path.stat().st_size > MAX_TEXT:
                    continue
            except OSError:
                continue
            found.append(path)
    return sorted(found, key=lambda path: str(path.relative_to(package_root)))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    meta = cargo_metadata()
    members = {package["id"] for package in meta["packages"] if package["id"] in meta["workspace_members"]}
    packages = {(p["name"], p["version"]): p for p in meta["packages"]}
    targets = {label: graph(target) for label, target in TARGETS.items()}
    union = sorted(set().union(*targets.values()))

    entries: list[str] = []
    texts: list[tuple[str, str]] = []
    seen: dict[str, int] = {}
    counted = 0

    for name, version in union:
        package = packages.get((name, version))
        if package is None or package["id"] in members:
            continue
        counted += 1
        root = Path(package["manifest_path"]).parent
        used = [label for label, deps in targets.items() if (name, version) in deps]
        lines = [
            f"## {name} {version}",
            "",
            f"- License: `{package.get('license') or 'UNKNOWN'}`",
            f"- Targets: {', '.join(used)}",
        ]
        if package.get("repository"):
            lines.append(f"- Repository: <{package['repository']}>")
        lines.append("")
        files = license_files(root)
        if files:
            lines.append("License/notice files:")
            lines.append("")
            for path in files:
                relative = path.relative_to(root)
                try:
                    text = path.read_text(encoding="utf-8", errors="replace").strip()
                except OSError:
                    continue
                digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
                if digest in seen:
                    lines.append(f"- `{relative}` — same text as entry #{seen[digest]}")
                else:
                    number = len(texts) + 1
                    seen[digest] = number
                    texts.append((f"{name} {version}: {relative}", text))
                    lines.append(f"- `{relative}` — text #{number}")
            lines.append("")
        else:
            lines.append(
                "No license file is shipped inside this crate. The SPDX expression "
                "above applies; the standard text is in `LICENSES/` and the "
                "version is pinned by `Cargo.lock`."
            )
            lines.append("")
        entries.append("\n".join(lines))

    header = [
        "# Third-party license texts",
        "",
        "> Generated by `scripts/generate-third-party-licenses.py`; do not edit by hand.",
        "> Per-crate LICENSE/COPYING/NOTICE texts for every normal (compiled-in)",
        "> dependency of the workspace, across Linux/Windows/macOS. Identical texts",
        "> are included once. `LICENSES/` holds the standard license-family texts;",
        "> `docs/THIRD_PARTY_INVENTORY.md` lists versions and SPDX expressions.",
        "",
        f"Packages covered: **{counted}** · unique texts: **{len(texts)}**",
        "",
    ]
    body = header + entries + ["# License texts", ""]
    for number, (source, text) in enumerate(texts, start=1):
        body += [f"## Text {number}: {source}", "", "~~~~", text, "~~~~", ""]
    rendered = "\n".join(body).rstrip() + "\n"

    if args.check:
        current = OUT.read_text(encoding="utf-8") if OUT.is_file() else ""
        if current != rendered:
            print(f"FAIL: {OUT} is out of date; rerun the generator", file=sys.stderr)
            return 1
        print(f"OK: {OUT} is up to date")
        return 0

    OUT.write_text(rendered, encoding="utf-8")
    print(f"wrote {OUT} ({counted} packages, {len(texts)} texts, {OUT.stat().st_size // 1024} KiB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
