# Privacy

Aether is a local-first desktop download manager. The application does not
ship analytics, telemetry, crash reporting, advertising or remote logging.
This document describes what is stored locally and which network endpoints a
user-triggered operation can contact.

_Last updated: 2026-10-31. This is an engineering description, not legal
advice._

## Data stored on the user's machine

Aether keeps its state in the platform data directory (for example
`~/.local/share/aether` on Linux):

- Download URLs, file names, destination paths, task state, progress and
  history.
- `settings.json`: proxy/PAC configuration (including proxy credentials), DNS,
  User-Agent, default headers, scheduler limits, RPC listener/token and plugin
  policy.
- `aether.db` (SQLite): tasks, per-task settings, plugin state, up to 500 log
  lines per task and the disabled-plugin list.
- Per-task settings may contain credentials the user supplied for that
  download: cookies, `Authorization` headers, proxy passwords, cloud-drive
  access/refresh tokens, FTP/SFTP credentials and OAuth client secrets.
- Update state and downloaded plugin packages/backups.

On Unix, `settings.json` is written with mode `0600`; the daemon data
directory and `aether.db` (plus its journal/WAL siblings) are restricted to
the owner (`0700`/`0600`). The database is **not encrypted at rest**: anyone
who can read the user's files can read the stored credentials. A future
release may move secrets to the OS keyring; until then, users who need
stronger isolation should rely on full-disk encryption and OS account
separation.

## Network connections

Aether connects only to endpoints required by actions the user takes:

- The URLs and mirrors of the downloads the user starts, including redirects.
- BitTorrent trackers, DHT nodes and peers for torrent tasks (the user's IP is
  visible to them, as with any BitTorrent client).
- Cloud-drive provider APIs when the user configures a cloud task, using the
  user's own OAuth application/token.
- The update manifest and release artifacts when an update check or automatic
  update is enabled in a build that has an update source configured (the
  project uses GitHub Releases).
- A PAC URL if the user configures one.
- Browser-extension handoff: the extension talks to the local native host over
  Native Messaging; it does not send browsing data to Aether servers.

There is no Aether-operated backend that receives download history, URLs,
cookies, tokens or telemetry.

## Browser extension

The optional extension asks for the `nativeMessaging`, `downloads`, `storage`
and `notifications` permissions, plus broad host access to read the link the
user invokes. The `cookies` permission is **optional and off by default**; when
enabled, cookies for the target URL are attached to that download request and
may be persisted with the task settings described above. The extension does
not collect passwords, payment data or browsing history, and it does not load
remote code.

## Diagnostics and logs

`aether-cli diag` creates a diagnostics bundle only when the user runs it. It
contains versions, redacted settings (RPC token, custom headers and proxy
user-info are masked), task counts and the plugin inventory; it does not
include task URLs or downloaded file contents. Task logs are local and capped
at the latest 500 lines per task. The retention period is not user-configurable
beyond deleting the task or the application data directory.

## User controls

- Delete individual tasks from the UI/CLI (this removes the task row, staging
  data and logs; already finalized files remain where the user saved them).
- Disable or uninstall plugins, including the cloud-drive plugins.
- Clear or replace the RPC token and disable the RPC listener.
- Disable update checks/automatic updates where the build exposes the setting.
- Remove the application data directory to erase all local state.
- Revoke OAuth access at the provider (Google/Microsoft/Dropbox) and remove the
  task that holds the token.

## Children and sensitive data

Aether is a general-purpose download tool, not directed at children. It does
not intentionally collect special-category personal data. Users should not
store credentials in task URLs or headers unless they accept that the values
are persisted locally in plaintext as described above.

## Changes and contact

Material changes to this policy will be documented in `CHANGELOG.md` and the
release notes. Privacy questions can be raised through the repository's
GitHub Issues; security reports should follow [SECURITY.md](SECURITY.md).
