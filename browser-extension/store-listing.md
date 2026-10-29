# Store listing copy (Chrome Web Store / Firefox Add-ons)

Upload artifact: `scripts/build-extension.sh` produces the zip; paste the
texts below into the store forms. Submission itself is manual (store
accounts and review are outside the repo).

## Chrome Web Store / Edge

**Name:** Aether Download Helper

**Summary (132 chars max):**
Send downloads to the local Aether download manager: right-click any link,
video or audio to hand it off, or optionally intercept browser downloads.

**Description:**
```
Aether Download Helper hands downloads to the local Aether daemon over
Native Messaging — nothing leaves your machine and no analytics are sent.

• Right-click a link, video or audio element → "Download with Aether".
• Optional interception: browser downloads are cancelled only after Aether
  has accepted the task (never before), and only for the sites you list.
• Optional cookie attachment (off by default): enable it in the options so
  authenticated or intranet links keep working. Cookies are attached to the
  download request only and are never stored.
• If the Aether desktop app is not running or the native host is missing,
  your browser download is left untouched.

Permissions rationale:
- nativeMessaging: talk to the local Aether host.
- downloads: observe and cancel browser downloads when interception is on.
- cookies (optional): attach cookies to downloads you start; off by default.
- host_permissions: read the referrer/context of the link you clicked.
- notifications: report a successful hand-off.

Requires the Aether desktop app and an installed native messaging manifest
(browser-extension/README.md covers the two install steps).
```

**Category:** Utilities / Developer tools

## Firefox Add-ons

**Name:** Aether Download Helper

**Summary (250 chars max):**
Send downloads to the local Aether daemon — right-click to hand off links,
optionally intercept downloads, and attach cookies only when you enable it.

**Description:** same body as above (first paragraph through permissions).

**Privacy policy:** required by AMO; Aether's PRIVACY.md in the repository
serves as the starting point (no telemetry, no remote code).
