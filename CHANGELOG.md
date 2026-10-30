# Changelog

All notable changes to Aether are documented here. Versions follow semantic
versioning; 0.x releases may still change structure.

## [Unreleased]

## [0.2.1] - 2026-10-30

### Added

- Settings can now actually check for app updates. A new `update_check` RPC
  fetches and verifies the signed manifest on a worker thread (the download
  loop keeps running), and the About panel renders the live state from
  `stats.update`: idle, checking, up to date, available, downloaded and
  ready to restart, or the error. A daemon started without `--update-url`
  answers `disabled`, and the panel says "no update source" instead of
  showing a fake "already latest 0.1.0" toast.
- Background app downloads are a real setting (`app_auto_update`). The
  periodic updater re-reads `settings.json` on every pass, so turning it off
  takes effect without a restart: a new release is then only reported as
  `available`. Missing key keeps the previous behaviour (download).
- The About panel's documentation, community, feedback, release-notes and
  licence rows open the real GitHub URLs (repository docs, Discussions,
  Issues, `CHANGELOG.md`, `LICENSE`). If no desktop opener can be started the
  toast prints the URL instead.
- `docs/SETTINGS_AUDIT.md` maps every settings control to the code that
  executes it and records the honest boundaries (read-only paths, launch
  configuration, external accounts).
- Settings → Advanced now configures remote control (RPC) for real: a listen
  address, a password-style access token, "apply" and "clear token" buttons,
  and a status line. The address is persisted and bound on the next daemon
  start (the line says so), while token changes rotate live. A non-loopback
  address without a token is rejected by the daemon's existing validation.
  Verified end to end: `aether-cli remote stats --socket 127.0.0.1:7788
  --token …` works, missing/wrong tokens are refused.

- The UI is bilingual with Chinese as the built-in first language. All 200+
  Slint strings are wrapped in `@tr()` with the Chinese source remaining the
  default (the compiler reserves index 0 for it); `translations/en` provides
  the English gettext translations, bundled at build time. A language switch
  in Settings → Appearance applies instantly and persists; the language is
  selected before the first render so the window never flashes the wrong
  one. ComboBox option strings are deliberately not translated: they double
  as the functional values Rust matches on. The Rust side speaks the same
  language: toasts, settings hints, daemon notices, relative times and
  ETAs, plugin descriptions and dashboard strings pair Chinese with English
  through i18n::tr(zh, en) at the call site, driven by one process-wide
  flag that the language switch flips. Two more languages ship complete:
  日本語 and 繁體中文 (250 translated UI strings each via bundled gettext,
  plus a single-row table for the Rust side whose missing rows fall back
  to Chinese). Adding a language is three entries: a .po file, a table
  column, and one item in the language list — all keyed from one place.
  The browser extension also gained store packaging: `scripts/build-extension.sh`
  builds the upload zip and `store-listing.md` carries the store copy.

- A system tray for Linux (StatusNotifier over zbus, which was already in
  the dependency tree through Slint): left-click or the context menu's
  "显示主窗口" summons the window, "退出" quits it. Sessions without a D-Bus
  bus or a StatusNotifier host degrade to "no tray" with a single log line
  and the app works window-only; downloads keep running in the daemon
  either way. Verified end-to-end on a private session bus: name
  acquisition, properties, GetLayout, Activate, and the quit path actually
  exiting the app.

- The tray now reaches Windows. `src/tray.rs` dispatches per platform and
  Windows gets its own implementation in `src/tray_windows.rs`, written in
  the project's raw-FFI style (shell32/user32/kernel32 directly, no new
  dependencies): a message-only window hosts a Shell_NotifyIconW icon with
  the same "显示主窗口 / 退出" menu, the icon loads from the logo.ico that
  the installer places next to the exe and falls back to the stock
  application icon otherwise. Start-up degrades honestly when the session
  has no shell to host an icon — one log line, window-only, exactly like
  the Linux path without a StatusNotifier host. The Windows CI job runs
  the bin tests now: a layout test pins NotifyIconData to the SDK's exact
  size and offsets (976 bytes on x64), a window test covers creation and
  the WM_DESTROY round trip, and the start handshake accepts Some or None
  on a headless runner as long as it reports promptly without crashing.
  zbus moved to a unix-only target dependency, so Windows builds no longer
  compile it. macOS stays out on purpose: an NSStatusItem needs objc2 and
  this repo's macOS CI is check-only, leaving no way to verify it.

- Persistence and support tooling: the daemon database can be backed up
  online (`aether-cli remote backup <dest>`, SQLite backup API — no daemon
  downtime) and checked for consistency (`remote db-check`). `aether-cli
  diag` writes an `aether-diagnostics/1` bundle with versions, redacted
  settings, per-state task counts and the plugin inventory; RPC secrets,
  custom headers and proxy userinfo are masked, task URLs and task-level
  settings never leave the machine. Task logs are now capped at the newest
  500 lines per task so a chatty plugin cannot grow the database without
  bound. The Windows uninstaller also removes the installer-owned plugins
  and license folders, leaving user data alone.

- Accessibility groundwork: the UI now builds with Slint's AccessKit
  bridge. Custom controls carry real semantics instead of being invisible
  to assistive tech — navigation and settings items are buttons with
  labels and checked state, filter pills are checkable buttons, task rows
  are list items with the task name and status as label/description,
  icon-only row actions have spoken names, the toast action is a button,
  and progress tracks expose a percentage. Verified to compile and render
  unchanged on Xvfb; real screen-reader runs (NVDA/Orca) are noted as an
  external step because this repo has no such environment.

- The macOS app bundle now ships the whole application: universal
  aether-daemon, aether-cli and aether-native-host live in Contents/MacOS
  next to the UI, and every first-party plugin is lipo'd to universal and
  placed under Contents/Resources/plugins, which the plugin directory
  lookup now understands. The release pipeline is the first real run (no
  macOS runtime here) — documented as such.

- Staged rollouts and migration exports. Update manifests can carry a
  `rollout` percentage (default 100); each machine hashes its hostname with
  the target version into a stable bucket, so a 25% rollout reaches roughly
  a quarter of installs and the same machine always agrees. The signature
  covers the percentage, so raising it means re-signing — documented. The
  release pipeline now generates update.json from the built artifacts
  (AppImage preferred over .deb for the same platform, version mismatches
  fail the build) and only attaches it when the offline signing key is
  configured — an unsigned manifest is dropped with a warning. On the
  migration side `aether-cli export` writes settings plus a task list as
  `aether-export/1` (task-level settings are deliberately excluded — they
  can hold cookies and tokens), and `import` recognises both that document
  and the plain URL list, applying exported daemon settings only when
  `--settings` is passed.

- Background behaviour: Aether is now single-instance (an advisory lock in
  the config directory; a second launch notifies "already running" and exits
  instead of racing for the daemon and tray), closes to the tray when one is
  actually visible (closing hides the window and the tray menu quits; a
  session without a tray keeps close = quit), and can start at login through
  the OS's own mechanism — an XDG autostart entry, an HKCU Run value, or a
  LaunchAgent plist. The start-at-login switch reads the real OS state at
  startup, so removing the entry by hand shows up honestly, and a refused
  write bounces the switch back with the reason. Along the way the Linux tray
  stopped claiming success when no StatusNotifier host exists: it now returns
  "no tray", which is what makes close-to-tray safe.

- Desktop notifications reach Windows: a WinRT toast through notify-rust
  (the platform's only toast path is not reachable via an external
  command, unlike notify-send and osascript). The toast is attributed to
  Aether through an AppUserModelID registered under HKCU on first use —
  no admin, no installer step — and falls back to notify-rust's built-in
  attribution if the registration is refused; a failed toast logs once
  and never panics. Linux and macOS keep their existing paths untouched.

- The Windows artifacts now ship the whole application instead of the UI
  alone: the installer and the portable zip carry aether-daemon,
  aether-cli, aether-native-host and every first-party plugin binary
  staged under `plugins\`. The pieces line up: the UI starts the daemon
  from beside its own exe, and the plugin directory lookup on Windows now
  leads with the exe-adjacent `plugins\` folder, so both the installed
  (`{app}\plugins`) and portable layouts resolve. The macOS bundle still
  carries only the UI — honestly noted, not yet fixed.

- Supply-chain hardening: every GitHub Action is pinned to a full commit
  SHA with a version comment resolved through the API (Dependabot keeps
  them current), `deny.toml` plus a CI cargo-deny job enforce license,
  advisory, ban and source policy (Slint's tri-license handled through
  documented exceptions), release assets gain a CycloneDX SBOM generated
  before the checksums so it is covered by SHA256SUMS, and the AppStream
  metainfo now describes the real product instead of the old mockup
  wording. The download queue ages priorities — waiting earns one point
  per five minutes (capped at 100) — so a starved low-priority task
  overtakes a freshly queued high-priority one after a few hours. The
  new-download dialog accepts whitespace-separated batches: paste
  several links or magnets at once and it queues them all.

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

### Changed

- Every settings switch is now a two-way binding. The one-way `checked:`
  bindings meant a programmatic bounce was invisible: turning on the
  download schedule without start/end times showed the switch as on while
  the daemon never received it, and an OS refusal of start-at-login left the
  switch on. The switches now follow the real state, and the schedule bounce
  was re-tested in the running UI.
- "Reset UI preferences" now also restores the language (Simplified Chinese)
  and the theme (follow the system) immediately, not just accent, motion,
  flow, start page, after-done action and notifications.
- Selecting PAC in the proxy combo box bounces back to the mode the daemon is
  actually using and explains that `AETHER_PAC_URL`/`--pac-url` decides it.
  The UI no longer displays a mode the daemon is not running.
- The fake "update channel" (stable/beta) combo box is gone: the update source
  is a read-only row driven by the daemon's launch configuration. Since there
  is no channel concept in the backend, a switch would have been a lie.

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
