#!/usr/bin/env python3
"""Verify that release artifacts carry the required license files.

The Slint Royalty-free 2.0 terms require attribution, and Aether's own
distribution policy requires the MIT license, the third-party notices and the
full Slint license text in every package. This script checks both the source
recipes and the built artifacts.

Usage:
    python3 scripts/check-license-bundles.py dist
    python3 scripts/check-license-bundles.py dist/aether_0.1.0_amd64.deb
    python3 scripts/check-license-bundles.py --check-recipes
"""

from __future__ import annotations

import argparse
import io
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
NOTICE = "THIRD_PARTY_NOTICES.md"
TEXTS = "THIRD_PARTY_LICENSES.md"
MIT_NAMES = ("LICENSE", "copyright")
SUPPORTED_SUFFIXES = (".deb", ".AppImage", ".zip", ".dmg", ".exe")

RECIPES = {
    "Makefile": ("LICENSE", NOTICE, TEXTS, "LICENSES/*"),
    ".github/workflows/package.yml": ("LICENSE", NOTICE, TEXTS, "LICENSES/*"),
    "packaging/windows/aether.iss": ("LICENSE", NOTICE, TEXTS, "LICENSES\\*"),
    "scripts/stage-plugins.py": ("LICENSE", NOTICE, TEXTS, "LICENSES"),
}


def license_texts() -> list[Path]:
    """Every full license text shipped under LICENSES/."""
    return sorted(
        path for path in (ROOT / "LICENSES").iterdir() if path.is_file()
    )


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)


def check_recipes() -> int:
    errors = 0
    for name in ("LICENSE", NOTICE, TEXTS):
        path = ROOT / name
        if not path.is_file() or path.stat().st_size == 0:
            fail(f"missing or empty source license file: {name}")
            errors += 1
    # Slint RF 2.0 attribution: every published release note is the body of
    # the download page, so each one must carry the badge (or the app must
    # ship AboutSlint, which it currently does not).
    for release in sorted((ROOT / "docs" / "releases").glob("*.md")):
        if "Made with Slint" not in release.read_text(encoding="utf-8"):
            fail(f"{release.relative_to(ROOT)} is missing the 'Made with Slint' badge")
            errors += 1
    texts = license_texts()
    if not texts:
        fail("LICENSES/ has no license texts")
        errors += 1
    for path in texts:
        if path.stat().st_size == 0:
            fail(f"empty license text: {path.name}")
            errors += 1
    for relative_path, needles in RECIPES.items():
        path = ROOT / relative_path
        if not path.is_file():
            fail(f"missing packaging recipe: {relative_path}")
            errors += 1
            continue
        text = path.read_text(encoding="utf-8")
        for needle in needles:
            if needle not in text:
                fail(f"{relative_path} does not reference {needle}")
                errors += 1
    if errors:
        return 1
    for script in (
        "scripts/generate-third-party-inventory.py",
        "scripts/generate-third-party-licenses.py",
    ):
        result = subprocess.run(
            [sys.executable, script, "--check"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            fail(f"{script} --check failed: {result.stdout}{result.stderr}")
            errors += 1
    if errors == 0:
        print("OK: license files, recipes and generated bundles are current")
    return 1 if errors else 0


def names_from_deb(path: Path) -> list[tuple[str, int]]:
    archive = subprocess.run(
        ["dpkg-deb", "--fsys-tarfile", str(path)],
        check=True,
        capture_output=True,
    )
    with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as tar:
        return [
            (member.name, member.size)
            for member in tar.getmembers()
            if member.isfile()
        ]


def names_from_zip(path: Path) -> list[tuple[str, int]]:
    with zipfile.ZipFile(path) as archive:
        return [
            (info.filename, info.file_size)
            for info in archive.infolist()
            if not info.is_dir()
        ]


def names_from_appimage(path: Path) -> list[tuple[str, int]]:
    with tempfile.TemporaryDirectory(prefix="aether-appimage-") as temp:
        path.chmod(0o755)
        subprocess.run(
            [str(path.resolve()), "--appimage-extract"],
            cwd=temp,
            check=True,
            capture_output=True,
        )
        root = Path(temp) / "squashfs-root"
        if not root.is_dir():
            raise RuntimeError("AppImage extraction produced no squashfs-root")
        return [
            (str(file.relative_to(root)), file.stat().st_size)
            for file in root.rglob("*")
            if file.is_file()
        ]


def names_from_dmg(path: Path) -> list[tuple[str, int]]:
    mount = tempfile.mkdtemp(prefix="aether-dmg-")
    subprocess.run(
        [
            "hdiutil",
            "attach",
            "-nobrowse",
            "-readonly",
            "-noverify",
            "-mountpoint",
            mount,
            str(path),
        ],
        check=True,
        capture_output=True,
    )
    try:
        root = Path(mount)
        return [
            (str(file.relative_to(root)), file.stat().st_size)
            for file in root.rglob("*")
            if file.is_file()
        ]
    finally:
        subprocess.run(
            ["hdiutil", "detach", mount, "-quiet"],
            check=False,
            capture_output=True,
        )
        shutil.rmtree(mount, ignore_errors=True)


def check_artifact(path: Path) -> int:
    suffix = path.suffix
    if suffix == ".exe":
        print(
            f"SKIP: {path.name} (Inno Setup payload is covered by the "
            "packaging recipe check)",
        )
        return 0
    readers = {
        ".deb": names_from_deb,
        ".AppImage": names_from_appimage,
        ".zip": names_from_zip,
        ".dmg": names_from_dmg,
    }
    reader = readers.get(suffix)
    if reader is None:
        fail(f"unsupported artifact type: {path}")
        return 1

    try:
        names = reader(path)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        fail(f"could not inspect {path.name}: {error}")
        return 1

    missing = []
    if not any(
        Path(name).name in MIT_NAMES and size > 0 for name, size in names
    ):
        missing.append("LICENSE or copyright")
    if not any(
        Path(name).name == NOTICE and size > 0 for name, size in names
    ):
        missing.append(NOTICE)
    if not any(
        Path(name).name == TEXTS and size > 0 for name, size in names
    ):
        missing.append(TEXTS)
    shipped = {Path(name).name for name, size in names if size > 0}
    for text in license_texts():
        if text.name not in shipped:
            missing.append(f"LICENSES/{text.name}")

    if missing:
        fail(f"{path.name} is missing: {', '.join(missing)}")
        return 1
    print(
        f"OK: {path.name} carries LICENSE, {NOTICE}, {TEXTS} and all LICENSES texts",
    )
    return 0


def expand(paths: list[str]) -> list[Path]:
    artifacts: list[Path] = []
    for raw in paths:
        path = Path(raw)
        if path.is_dir():
            artifacts.extend(
                child
                for child in sorted(path.iterdir())
                if child.suffix in SUPPORTED_SUFFIXES
            )
        else:
            artifacts.append(path)
    return artifacts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        help="artifact files, or directories containing artifacts",
    )
    parser.add_argument(
        "--check-recipes",
        action="store_true",
        help="check the source files and packaging recipes only",
    )
    args = parser.parse_args()

    if not args.paths and not args.check_recipes:
        parser.error("pass at least one artifact path or --check-recipes")

    status = 0
    if args.check_recipes:
        status |= check_recipes()
    for artifact in expand(args.paths):
        status |= check_artifact(artifact)
    return 1 if status else 0


if __name__ == "__main__":
    sys.exit(main())
