# Aether compatibility

Aether targets **desktop PC platforms**, with **Linux as the primary platform**
(x86_64 and arm64). Windows and macOS remain buildable secondary targets. Mobile
(Android/iOS) and embedded targets are deliberately out of scope for now; the UI
is still optimized for a 900×600+ desktop window.

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
- **Proxy.** HTTP/HTTPS and SOCKS5/SOCKS5h proxies work end to end (daemon
  settings or `AETHER_PROXY`); `system` mode honors `HTTP_PROXY`/`ALL_PROXY` and
  falls back to GNOME (`gsettings`) / KDE (`kioslaverc`) desktop settings,
  including a desktop-configured PAC URL.
  PAC is evaluated in the Host (`rquickjs` sandbox) from a URL, `file://` path or
  inline script and passed to HTTP/media plugins per task; WPAD autodiscovery is
  still planned for P2.
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

- macOS builds are **ad-hoc signed and not notarized**. Notarization requires a
  paid Apple Developer account and is not planned for now; see
  [SIGNING.md](SIGNING.md). On recent macOS, use System Settings → Privacy &
  Security → Open Anyway, or build from source.
- Windows installers are **not code-signed**. Authenticode is not planned for
  now (Linux is the priority). Use `SHA256SUMS`; a free OSS certificate may be
  revisited later.
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
