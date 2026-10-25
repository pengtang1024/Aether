# Aether compatibility

Aether targets **desktop PC platforms**. Mobile (Android/iOS) and embedded
targets are deliberately out of scope for now; the UI is still optimized for a
900×600+ desktop window.

## Release matrix

| Platform | Architecture | Package | Minimum target |
| --- | --- | --- | --- |
| Linux | x86_64 | `.deb`, `.AppImage` | Ubuntu 22.04+ / Debian 12+ / glibc 2.35+ |
| Linux | arm64 | `.deb`, `.AppImage` | Ubuntu 22.04 arm64+ / Raspberry Pi OS Bookworm / glibc 2.35+ |
| Windows | x86_64 | `setup.exe`, portable `.zip` | Windows 10/11 |
| Windows | arm64 | `setup.exe`, portable `.zip` | Windows 11 on Arm |
| macOS | universal | `.dmg` | macOS 11+ (Apple Silicon + Intel) |

Only `release/*` branches are built. The branch suffix must match
`Cargo.toml`’s version, e.g. `release/0.1.0`. `main` and feature branches never
publish packages.

## Runtime notes

- **Native rendering.** Slint uses the winit backend with the femtovg/OpenGL
  renderer and a software fallback. There is no WebView, Chromium or Electron.
- **Display servers.** Linux builds support Wayland and X11. The packages
  depend on the usual `libwayland`, `libxkbcommon`, `libx11` and `libgl1`/
  `libegl1` libraries; desktops normally provide them.
- **Fonts.** Text shaping uses the system font stack with font fallback
  (fontique). Linux recommends `fonts-noto-cjk` for the Chinese UI (the `.deb`
  declares it as a recommendation). Windows uses the installed CJK fonts and
  macOS uses PingFang.
- **Windows runtime.** Windows binaries are built with the static MSVC runtime
  (`+crt-static`), so no separate Visual C++ Redistributable is required.
- **AppImage.** The AppImage relies on the host’s graphics/font libraries and
  glibc. If FUSE is unavailable, run it with
  `--appimage-extract-and-run` or install `libfuse2`.
- **Low-resource builds.** Release builds use LTO, stripping and a native
  architecture (no cross-architecture emulation); the software renderer is
  available as a fallback on systems without usable OpenGL.

## Packaging caveats

- macOS builds are **ad-hoc signed but not notarized**. Gatekeeper may require
  right-click → Open, or clearing the quarantine attribute.
- Windows installers are **not code-signed**; SmartScreen may show a warning.
- Linux `.deb` requires glibc 2.35+ (built on Ubuntu 22.04). The AppImage has
  the same glibc baseline. Older distros need a Flatpak or a rebuild on an older
  base image; Flatpak is not part of the current matrix.
- macOS Intel support is provided by the universal binary. There is no 32-bit
  Linux/Windows build.

## CI / local checks

The GitHub Actions workflow is `.github/workflows/package.yml`:

- Linux x86_64: `ubuntu-22.04`
- Linux arm64: `ubuntu-22.04-arm`
- Windows x86_64: `windows-latest`
- Windows arm64: `windows-11-arm`
- macOS universal: `macos-14`
- All artifacts are attached to a **draft** GitHub release.

Local Linux checks:

```bash
make check
make deb ARCH=amd64   # or ARCH=arm64 on an arm64 host
```
