#!/usr/bin/env python3
"""Build the minimal ffmpeg bundled with the media and rtmp plugins.

Layout:
  build/ffmpeg/<platform-key>/ffmpeg

Only Linux packages bundle ffmpeg for now; Windows and macOS plugins keep
using ffmpeg from PATH. The build is intentionally minimal: all demuxers,
muxers, protocols and bitstream filters, the decoders needed to probe common
streams, and no encoders or filters. That covers RTMP capture and `-c copy`
remuxes at roughly 15 MiB instead of the ~166 MiB full static build.

Source downloads are pinned by URL and SHA-256 and cached under .cache/ffmpeg.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

VERSION = "9.0.2"
SOURCE = (
    f"ffmpeg-{VERSION}.tar.xz",
    f"https://ffmpeg.org/releases/ffmpeg-{VERSION}.tar.xz",
    "8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e",
)

DECODERS = (
    "aac,h264,hevc,av1,vp9,opus,flac,vorbis,mp3,ac3,eac3,"
    "mpeg2video,mpeg4,vc1,pcm_s16le,pcm_s24le"
)

CONFIGURE = [
    "--disable-decoders",
    f"--enable-decoder={DECODERS}",
    "--disable-encoders",
    "--disable-filters",
    "--disable-indevs",
    "--disable-outdevs",
    "--disable-hwaccels",
    "--disable-doc",
    "--disable-debug",
    "--disable-autodetect",
    "--disable-x86asm",
    "--enable-gpl",
    "--extra-cflags=-O2",
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


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(destination: Path) -> None:
    name, url, expected = SOURCE
    destination.parent.mkdir(parents=True, exist_ok=True)
    partial = destination.with_suffix(destination.suffix + ".part")
    print(f"downloading {name} ...", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "aether-build"})
    try:
        with urllib.request.urlopen(request, timeout=300) as response, partial.open("wb") as file:
            shutil.copyfileobj(response, file, length=1024 * 1024)
    except OSError:
        # Some mirrors break Python's TLS fingerprint; curl is the fallback.
        subprocess.run(["curl", "-fL", "--retry", "3", "-o", str(partial), url], check=True)
    actual = sha256(partial)
    if actual != expected:
        partial.unlink()
        raise SystemExit(f"{name}: sha256 {actual} != {expected}")
    partial.replace(destination)


def build(source: Path, work: Path, output: Path) -> None:
    build_dir = work / f"build-{work.name}"
    if build_dir.exists():
        shutil.rmtree(build_dir)
    build_dir.mkdir(parents=True)
    subprocess.run(
        [str(source / "configure"), *CONFIGURE],
        cwd=build_dir,
        check=True,
    )
    subprocess.run(
        ["make", f"-j{os.cpu_count() or 4}", "ffmpeg"],
        cwd=build_dir,
        check=True,
    )
    binary = build_dir / "ffmpeg"
    subprocess.run(["strip", "--strip-all", str(binary)], check=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, output)
    output.chmod(0o755)
    shutil.rmtree(build_dir, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform-key", default=platform_key())
    parser.add_argument("--out", default="build/ffmpeg")
    parser.add_argument("--force", action="store_true", help="rebuild even if cached")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    key = args.platform_key
    if not key.startswith("linux-"):
        print(
            f"WARNING: ffmpeg is only bundled in Linux packages for now; "
            f"{key} plugins will use ffmpeg from PATH.",
            file=sys.stderr,
        )
        return 0

    host = platform_key()
    if key != host:
        raise SystemExit(
            f"cannot build {key} ffmpeg on {host}; run this on a matching host"
        )

    name, _, _ = SOURCE
    cache = root / ".cache" / "ffmpeg"
    archive = cache / name
    source = cache / f"ffmpeg-{VERSION}"
    work = cache / key
    out = Path(args.out)
    if not out.is_absolute():
        out = root / out
    destination = out / key / "ffmpeg"

    if destination.is_file() and not args.force:
        print(f"using cached ffmpeg for {key}: {destination}")
        return 0

    cached = work / "ffmpeg"
    if cached.is_file() and not args.force:
        out.mkdir(parents=True, exist_ok=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(cached, destination)
        destination.chmod(0o755)
        print(f"using cached ffmpeg for {key}: {destination}")
        return 0

    if not archive.is_file() or sha256(archive) != SOURCE[2]:
        download(archive)
    if not source.is_dir():
        with tarfile.open(archive) as bundle:
            bundle.extractall(cache)
    if not (source / "configure").is_file():
        raise SystemExit(f"{archive.name} did not contain ffmpeg-{VERSION}")

    print(f"building ffmpeg {VERSION} for {key} ...", flush=True)
    build(source, work, cached)
    shutil.rmtree(source, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cached, destination)
    destination.chmod(0o755)
    size = destination.stat().st_size
    print(f"built ffmpeg for {key}: {destination} ({size / 1024 / 1024:.0f} MiB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
