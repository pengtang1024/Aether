# Aether browser helper

MV3 extension that sends links/downloads to the local Aether daemon through
`com.pengtang1024.aether` Native Messaging.

## Load locally

1. Build/install Aether (`aether-native-host` must be on `PATH`).
2. Load this directory as an unpacked extension (Chrome/Edge).
3. Copy the extension ID shown by the browser.
4. Install the native manifest:
   ```bash
   # Linux/macOS
   scripts/install-native-messaging.sh <extension-id> /usr/bin/aether-native-host
   # Windows (per-user, no admin)
   powershell -File scripts/install-native-messaging.ps1 -ExtensionId <extension-id> -HostPath <path-to-aether-native-host.exe>
   ```
5. Open the extension options and click “Test native host”.

Firefox uses `allowed_extensions` with `aether@pengtang1024.github.io`; see the
template in `packaging/native-messaging/`.

## Behavior

- Context menu: send link/video/audio URL.
- Optional interception: `downloads.onCreated` → native host → cancel browser
  download only after the daemon accepts the task.
- Site filter (options page): interception applies only to the listed hosts
  (`example.com` or `*.example.com`); blank = all sites.
- Cookies (options page, **off by default**): when enabled, the browser asks
  for the optional `cookies` permission; cookies for the download URL are
  attached to the task request so authenticated/intranet links work. They are
  only sent with the request and never stored.
- If Aether/native host is unavailable, the browser download is left untouched.

## Store submission

Build the upload artifact:

```bash
scripts/build-extension.sh          # writes dist/aether-extension-<version>.zip
```

Store copy (name, summaries, description, permission rationale) lives in
`store-listing.md` — paste it into the Chrome Web Store / Edge / Firefox
Add-ons forms. Submission itself is manual: it needs store accounts, and
AMO additionally wants a privacy policy: use
[`PRIVACY.md`](PRIVACY.md) (also linked from the repository root policy).

Development builds keep working alongside a store install: the native
manifest is keyed to the extension ID you install with (step 4 above).

This is a development build; store packaging/signing is tracked in
`docs/BROWSER_INTEGRATION.md`.
