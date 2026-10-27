# Changelog

All notable changes to Aether are documented here. Versions follow semantic
versioning; 0.x releases may still change structure.

## [Unreleased]

### Added

- Nothing yet.

## [0.2.0] - 2026-10-27

First release of the real download engine. The desktop window is still the
0.1.0 preview and does not drive the daemon yet; everything below is usable
through `aether-daemon`, `aether-cli`, the browser native host or the socket
API.

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
- Task queue: `AETHER_MAX_CONCURRENT` (default 3) with FIFO promotion on
  completion/failure/cancel; `resume` now restarts failed(retryable)/canceled
  tasks from the stored record and paused tasks stay paused across daemon
  restarts; cancel-in-progress is detected to avoid double starts.
- Speed limiting: per-task `max_speed_bps` setting and global
  `AETHER_MAX_SPEED_BPS` spread across running tasks, enforced by a shared
  token bucket in the HTTP download loop (inherited by delegating plugins).
- HTTP plugin consumes browser-forwarded settings: `referrer`, `user_agent`,
  `cookies`, `headers` (managed headers rejected), plus `max_speed_bps`.
- Completed downloads now land directly in the download directory (previously
  buried under `.aether/<task>/final/`), with automatic `-1`, `-2`… renaming on
  filename conflicts instead of a hard error.
- Automatic retries are now durable (schema v3, `retry_attempts`/`retry_at`):
  a daemon restart resumes pending retries with their remaining delay instead of
  dropping them. This also fixes the attempt counter resetting on every retry,
  which made `AETHER_RETRY_MAX` ineffective (retries could loop forever). The
  counter is kept when the cap is hit, cleared by resume/cancel/completion, and
  exposed on `StoredTask` for a UI countdown.
- PAC (auto-proxy): the Host evaluates `FindProxyForURL` in a `rquickjs`
  sandbox (16 MiB heap, 300 ms deadline, no host APIs) from a script URL,
  `file://`/local path or inline script, caches it for 30 minutes and keeps the
  last good copy when a refresh fails. The resolved proxy travels per task
  (`TaskSpec.proxy`, probe included); HTTP and media plugins build a per-task
  client when it differs from the process default. New daemon method
  `resolve_proxy` and setting `{"mode":"pac", …}`; `AETHER_PAC_URL`/`--pac-url`.
  Only the first directive of a PAC result is used.
- Per-task destination directory: `add` now honors the `dir` field (absolute
  paths as-is, relative paths confined to the download root), reports it back
  as `dir`, and stores non-default destinations in a new `tasks.target_dir`
  column (schema v2 migration keeps v1 databases and tasks). `aether-cli`
  records its output directory so `resume` returns to the same place.
- Proxy configuration now reaches the plugins: `get_settings`/`set_settings`
  on the daemon (persisted in `.aether/settings.json`, mode `0600`), the
  `AETHER_PROXY` one-line override (`system`/`none`/`http://user:pass@host:port`/
  `socks5://host:port`), and `PluginManager::set_proxy`. HTTP and media plugins
  already honored `Init.proxy`; before this the daemon always passed `None`.
- Task logs API: daemon `logs` method, typed `Client::logs` (`TaskLog` with
  level/message/timestamp/fields, oldest first) and `aether-cli logs`. The
  daemon now records start/queue/completion/failure/finalize/retry/pause/
  resume/cancel lines, so the UI can show a task detail log.
- Daemon `delete` method for terminal tasks (record + staging removed, files
  kept) with a `task_deleted` event.
- Automatic retry for retryable failures: exponential backoff
  (`AETHER_RETRY_BASE_SECS`, default 30 s) with an attempt cap
  (`AETHER_RETRY_MAX`, default 5), retries re-enter the queue FIFO, manual
  resume resets the count and cancel stops the cycle; `retry_scheduled`
  events and task_logs entries record each attempt.
- `aether-cli import <file|->` queues many URLs through the running daemon
  (one per line, `#` comments ignored), reporting per-URL task ids and a
  summary; exit code reflects failures.
- Daemon `subscribe` event stream + typed `list_tasks`/`task_record`/`add_task`
  client APIs for the Slint UI.
- Browser Native Messaging host + MV3 extension skeleton + `aether://`/`magnet:`/
  `ed2k:`/`thunder:` desktop URL handler; native host auto-starts the daemon.
- Signed auto-update client (Ed25519 detached signature, SHA-256, AppImage
  atomic replace/rollback) with a daemon update loop and `update_status`;
  idle daemon no longer polls plugins and unloads them after 5 minutes.
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
