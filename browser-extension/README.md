# Aether browser helper

MV3 extension that sends links/downloads to the local Aether daemon through
`com.pengtang1024.aether` Native Messaging.

## Load locally

1. Build/install Aether (`aether-native-host` must be on `PATH`).
2. Load this directory as an unpacked extension (Chrome/Edge).
3. Copy the extension ID shown by the browser.
4. Install the native manifest:
   ```bash
   scripts/install-native-messaging.sh <extension-id> /usr/bin/aether-native-host
   ```
5. Open the extension options and click “Test native host”.

Firefox uses `allowed_extensions` with `aether@pengtang1024.github.io`; see the
template in `packaging/native-messaging/`.

## Behavior

- Context menu: send link/video/audio URL.
- Optional interception: `downloads.onCreated` → native host → cancel browser
  download only after the daemon accepts the task.
- If Aether/native host is unavailable, the browser download is left untouched.
- Cookies/headers are not read by default; the host accepts them only if a
  future UI explicitly adds them.
