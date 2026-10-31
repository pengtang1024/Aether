# Aether Download Helper — privacy policy

The extension sends downloads to the local Aether daemon through Chrome/Firefox
Native Messaging. It has no server component and does not send data to the
extension author.

## What it handles

- The URL, referrer and page context of the link/download the user explicitly
  hands to Aether.
- If the user enables the optional `cookies` permission, cookies for the target
  URL, attached only to that download request.
- Local settings: intercept on/off, notification on/off, cookie on/off and the
  site filter. These are stored in `chrome.storage.local`.

It does not collect browsing history, passwords, payment data, keystrokes or
analytics, and it does not load remote code.

## Retention and sharing

Data is handed to the user's local Aether daemon and may be stored in the
daemon's local task database (see the repository `PRIVACY.md`). The extension
author does not receive or retain it. Uninstalling the extension removes its
local settings.

## Limited Use (Chrome Web Store)

The extension's use of user data is limited to providing the single purpose of
handing downloads to the user's local Aether application. It does not sell
data, use it for advertising, or transfer it to third parties.

## Contact

Questions: open an issue at https://github.com/pengtang1024/Aether/issues.
