# Privacy

Aether is a local desktop download manager. This document describes the
intended privacy behavior of the Host and first-party plugins.

## Data the app handles

- Download URLs, file names, paths, task history and settings are stored
  locally on the user's machine.
- Browser integration (P5) may receive a URL, referrer and, only with explicit
  user permission, request headers/cookies for the requested download. These
  are used for that task and are not sent to Aether servers.
- Cloud-drive plugins (P6) use OAuth tokens stored in the OS keyring; tokens are
  never written to logs or sent anywhere except the provider's API.
- Proxy/PAC settings and credentials are stored locally; PAC evaluation happens
  locally.

## Network connections

Aether connects only to endpoints needed for the user's requested downloads,
trackers/peers for BitTorrent, update metadata if enabled, and the APIs of
plugins the user enables. There is no analytics/telemetry enabled by default.

## Logs and diagnostics

Logs are local. Diagnostics bundles are exported only when the user asks and
must redact credentials, cookies and tokens. Log retention is configurable.

## User controls

Users can clear task history, remove tasks and temporary files, disable
plugins, and revoke cloud-drive tokens. Browser cookie/header sharing is opt-in
per site and can be disabled at any time.

## Current status

Aether 0.1.0 is a UI mockup and performs no download networking. This policy is
the target for the engine/plugin implementation; it is not a claim that all
features are already implemented.
