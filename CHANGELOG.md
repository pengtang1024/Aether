# Changelog

All notable changes to Aether are documented here. Versions follow semantic
versioning; this is a UI mockup, so 0.x versions may change visual structure.

## [Unreleased]

### Added

- P0 plugin skeleton: `aether-plugin-sdk` (manifest, MessagePack framing, typed
  Host/plugin protocol, `Plugin` runner, `PluginProcess` supervisor) and
  `aether-plugin-fake` (real-process contract tests for handshake, probe,
  progress and completion).
- Workspace test/CI coverage for the plugin crates.
- P1 HTTP plugin: segmented Range downloads, resume state, pause/resume/cancel,
  filename/ETag handling and SHA-256 verification, with a local Range server
  contract test.
- Metalink v3/v4 support inside the HTTP plugin (priority-based mirror choice,
  SHA-256 from the metalink).
- FTP/FTPS (REST resume), HLS/DASH VOD (AES-128, SegmentTemplate/List), BT
  (librqbit), WebDAV, S3 SigV4, SFTP and yt-dlp extractor plugins.
- thunder:// decoder, IPFS/IPNS gateway, RTMP/RTSP via ffmpeg, and Google
  Drive / OneDrive / Dropbox public-share links.
- Baidu Netdisk and Aliyun Drive official-API adapters (`baidupan://<fs_id>`,
  `aliyunpan://<drive>/<file>` with access tokens); shared-link OAuth is pending.
- Host runtime (`aether-host`) with plugin discovery, lazy process startup,
  probe routing, event dispatch and safe finalization; headless `aether-cli`
  with list/probe/download.
- SQLite task store (`tasks`, `task_files`, `plugin_settings`, `task_logs`)
  with schema v1 migration, recovery query, `aether-cli tasks` and
  `aether-cli resume`.
- Optional cdylib plugin ABI (`kind: "cdylib"`, C ABI, `libloading` host
  loader, example cdylib) while process plugins remain the default.
- `aether-daemon`: single-instance Unix-socket daemon with SQLite + plugin
  runtime, `add/list/get/pause/resume/cancel/probe/shutdown`, systemd user
  unit and Linux packaging.
- Linux packages now stage and install all plugin packages under
  `/usr/lib/aether/plugins`, and ship `aether-cli` for headless tasks/resume.

### Changed

- A desktop UI redesign is in progress (floating canvas, filter counts,
  card-based detail surface). It is not part of 0.1.0.
- **Linux is now the primary platform.** Windows/macOS remain buildable
  secondary targets, but signing and notarization are deferred (no certificate
  or Apple Developer spend).
- Clarified license scope: Aether's own code is MIT; vendored Slint widgets and
  the Slint runtime are under the Slint Royalty-free 2.0 option. Packages now
  ship the full Slint license text and third-party notices.

### Removed

- Withdrew the v0.1.0 release binaries: they did not include
  `LICENSES/Slint-Royalty-free-2.0.md` and predate the final third-party
  notices. All assets were deleted; the tag is retained for provenance only.
  See [docs/WITHDRAWN_RELEASES.md](docs/WITHDRAWN_RELEASES.md).

## [0.1.0] - 2026-10-26

### Added

- Native Rust + Slint desktop UI with the vendored Material 3 widget set.
- Virtualized task list with search, status filters, live simulated speeds,
  progress, status tags and row actions.
- New-download dialog with URL/path/segments/speed limit/note, keyboard focus,
  Enter submit and Esc dismiss.
- Task detail drawer with speed chart, chunk grid, file information, logs and
  BT peers.
- Five settings groups: general, bandwidth, BT, appearance and notifications.
- Simulated download engine with 12 demo tasks and a 5 Hz UI timer.
- Brand icon set: transparent mark for the in-app rail, frosted icon for the
  desktop/window/dock, 32–512 hicolor assets and a Windows `.ico`.

### Packaging

- Linux x86_64 and arm64: system `.deb` and portable `.AppImage`, built on
  Ubuntu 22.04 (glibc 2.35+).
- Windows x86_64 and arm64: Inno Setup installer and portable `.zip`, with a
  static MSVC runtime.
- macOS universal (Apple Silicon + Intel) `.dmg`, macOS 11+.
- GitHub Actions builds only on `release/*` branches and attaches all artifacts
  to a draft GitHub release with release notes and SHA-256 checksums.

### Known limitations

- This is a UI mockup: downloads are simulated, with no networking, file I/O or
  persistence.
- macOS and Windows packages are unsigned/not notarized.
- Mobile platforms are not included yet.
- The application window is desktop-oriented (900×600 minimum).

[0.1.0]: https://github.com/pengtang1024/Aether/releases/tag/v0.1.0
