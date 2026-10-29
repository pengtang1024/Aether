# Changelog

All notable changes to Aether are documented here. Versions follow semantic
versioning; 0.x releases may still change structure.

## [Unreleased]

### Added

- The plugin update channel is live. The update manifest gains an optional
  `plugins` map (id → version/url/sha256, parsed compatibly with older
  manifests); the daemon's `plugin_updates` RPC compares installed versions
  against it (reporting `configured: false` when no update URL/key is set
  instead of pretending), and `plugin_update_all` downloads each outdated
  plugin as a `.tar.gz`, verifies the SHA-256 from the signed manifest,
  unpacks it through a hardened extractor (relative paths only, no links,
  256 MiB cap) and feeds it through the same signature-verified install
  path — one bad package (busy plugin, hostile archive, hash mismatch) is
  reported without blocking the others. The periodic update loop now runs a
  plugin pass every cycle when the new `plugin_auto_update` setting is on,
  and the plugin page's "update all" button and auto-update switch are real
  controls. Verified by schema-compat and unpack-guard unit tests plus four
  daemon e2e cases served from a local signed-manifest HTTP fixture.

- Plugins can be installed with verification. The manifest gains a `files`
  map (relative path → SHA-256, excluding `plugin.json` itself); `signature`
  now carries an Ed25519 signature over the canonical manifest bytes (the
  JSON value without the `signature` key, so formatting and key order do not
  matter), which transitively covers every file in the package. The new
  `aether-plugin-sign <package-dir>` tool recomputes `files` and signs with
  the existing release key. The daemon's `install_plugin` RPC validates
  identity/API/host compatibility/executable path safety, then — when
  `AETHER_UPDATE_PUBKEY` is configured — requires a valid signature and
  checks every file hash; without a key, unsigned installs need the explicit
  `AETHER_ALLOW_UNSIGNED_PLUGINS=1` dev opt-in. Installs stage a copy and
  swap with rollback, refuse to replace a plugin with running tasks, retire
  idle old processes, and re-apply the persisted disable list on rescan
  (`rescan_plugins` RPC, wired to the plugin page's reload button). The
  plugin page's install row now takes a package path, and the three switches
  with no mechanism behind them became honest capability labels.

- Clipboard link detection: an opt-in toggle (settings → general) starts a
  background poll that watches for a *changed* clipboard value whose first
  token is a download URL (http/https/ftp/ftps/magnet/ed2k/thunder). When it
  fires, a snackbar offers a one-click "new task" action that opens the
  download dialog with the URL prefilled. The first reading seeds a baseline,
  so enabling never fires on stale content; sessions without a clipboard
  backend get a disabled switch that says so instead of a dead control.
  `file://`, embedded URLs and plain paths are rejected up front. The snackbar
  itself grew an optional action button; verification: unit tests for the
  detection rules plus an Xvfb + xclip end-to-end smoke (copy → toast →
  click → prefilled dialog).

- HLS/DASH downloads can be remuxed to another container after they finish:
  the task-level `transcode_to` setting names the container and the media
  plugin runs `ffmpeg -c copy` (container change only, no re-encode) with
  `-progress pipe:1` parsed into the task's progress. A failed or missing
  ffmpeg never costs the user the download — the original file is kept, the
  task completes, and the reason (including ffmpeg's stderr tail) lands in
  `plugin_state.transcode.note`; half-written output is removed. The new
  daemon-wide `ffmpeg_path` setting is injected into task settings at add
  time (a task-level value wins) and is shared with rtmp capture; the media
  settings page gained the path row. Acceptance: fake-ffmpeg e2e
  (`crates/aether-plugin-media/tests/transcode_e2e.rs`) asserting the argv,
  exit 0 renaming, exit 1 keeping the original, a missing binary, and the
  no-setting passthrough.

- Browser takeover is now end-to-end wired. The extension attaches cookies to
  downloads only after the user enables it in the options page (the browser's
  own optional-permission prompt; off by default, never stored), interception
  honors a per-host site filter (`example.com` / `*.example.com`), and
  `scripts/install-native-messaging.ps1` registers the native host on Windows
  through HKCU (no admin) plus the Firefox `%APPDATA%` manifest — the UI's
  "extension installed" check reads the registry there. The native host's
  referrer/cookies/headers/filename passthrough into `TaskSpec.settings` is
  asserted by an integration test, alongside a `file://` rejection case.
  Extension-side behavior has no browser test harness; it is verified through
  the options page "Test native host" plus manual smoke testing.

- RPC over TCP is available as an opt-in remote-control channel. The daemon
  gains `rpc_listen` / `rpc_secret` settings (plus `--rpc-listen` /
  `AETHER_RPC_LISTEN`); every TCP connection must open with an `auth` request
  whose token is compared in constant time, and a non-loopback listen address
  without a secret makes the daemon refuse to start. Token rotation via
  `set_settings` applies to the next connection without a listener restart.
  `Client::connect_rpc(addr, token)` and
  `aether-cli remote … --socket ip:port [--token …]` are the client entries.
  The wire protocol (including event subscriptions) is unchanged from the
  local socket. No TLS: use a trusted network or an SSH tunnel.
  Acceptance: `crates/aether-daemon/tests/rpc.rs` (good/wrong/missing token,
  authed session with subscribe + shutdown, wide-open refusal, rotation) —
  also runs in the CI Windows job.

- The daemon and UI now run on Windows. `aether-daemon` speaks JSON lines over
  a per-user named pipe (`\\.\pipe\aether-<user>`, default DACL = same user,
  no new dependency — kernel32 is bound directly in
  `crates/aether-daemon/src/transport.rs`) instead of a Unix socket; the wire
  contract in `docs/UI_INTEGRATION.md` is unchanged. The UI backend, the
  daemon client, `aether-cli remote|import` and `aether-native-host` lost
  their `#[cfg(unix)]` stubs, and a transport loopback test
  (`crates/aether-daemon/tests/transport.rs`) runs on both platforms —
  including in the CI Windows job. Desktop notifications remain silent on
  Windows (honest gap; `notify-rust` is the candidate).

- Dashboard and plugin management are real: the daemon serves `stats`
  (today/week volume, per-day bars, per-type storage split, disk usage, task
  and plugin counts, real paths) and
  `plugins` / `set_plugin_enabled` (persistent disable list in SQLite, plugins
  with running tasks refuse to be disabled, disabled plugins refuse new work).
  The home dashboard and the settings → plugins page are driven by them; fake
  paths (`/home/aether/…`) and the simulated plugin catalogue are gone.
- `aether-cli remote stats|plugins|enable|disable` for the same over a
  terminal.
- The desktop window is wired to the daemon: `src/backend.rs` keeps a
  subscription and pushes snapshots to Slint, so tasks, counts, speed/ETA,
  pause/resume, cancel+delete, the new-download dialog (with the daemon's real
  download root) and the proxy setting (system/direct/manual) all drive real
  downloads. The simulated engine (`src/engine.rs`) is gone; chart geometry
  moved to `src/chart.rs`.
- The settings page shows the binary version (`Aether 0.2.0`) and the
  user-agent default from `CARGO_PKG_VERSION` instead of hardcoded `0.1.0`.
- Queue priorities: `add` (and `aether-cli remote add --priority`) accepts an
  integer priority, clamped to `-100..=100`. Promotion is
  `priority DESC, created_at ASC` (FIFO within a priority), exposed on
  `StoredTask.priority`, so a small download no longer waits behind a large one.
- Disk-space preflight before a task starts: `add` compares the probe size
  against the free space of the destination (minus the remaining bytes of
  downloads already writing there) and fails with a clear message instead of
  dying halfway. `ENOSPC` during a download now surfaces as a non-retryable
  `DiskFull` error in the HTTP plugin.
- `aether-cli remote <ping|add|list|get|logs|pause|resume|cancel|rm|settings|
  set-settings|resolve-proxy|watch>`: the whole daemon API from a terminal, so
  headless installs no longer need the UI to control tasks.
- `system` proxy mode now reads the Linux desktop configuration: GNOME
  (`gsettings`, manual and automatic/PAC modes) and KDE (`kioslaverc`), with
  proxy environment variables staying authoritative. Detection is cached for
  60 seconds and the resolved proxy is applied per task, probe included.
  WPAD autodiscovery is deliberately not implemented.

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
