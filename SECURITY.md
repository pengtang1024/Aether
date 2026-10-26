# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.1.x | Yes (UI mockup / early preview) |

## Reporting a vulnerability

Please **do not** open a public issue for security problems. Use GitHub's private
vulnerability reporting (Security → Report a vulnerability) or contact the
maintainer at the address in the repository profile.

Include:

- affected version and platform;
- reproduction steps / proof of concept;
- impact (credential leak, path escape, code execution, etc.);
- whether a plugin, browser native host, or cloud OAuth flow is involved.

We will acknowledge within 72 hours and coordinate a fix/disclosure. Please do
not test against other users' systems or data.

## Scope

In scope: the Aether host, first-party plugins, native messaging host, installer,
update metadata, and signing pipeline. Out of scope: third-party downloads,
DRM/paywall bypass, and vulnerabilities in upstream libraries that are already
public (report those upstream as well).

## Current security posture

- Plugins are first-party and will be signed/versioned; process isolation and
  least-privilege path grants are planned.
- Download paths are sanitized; checksum verification and atomic finalization are
  planned with the engine.
- Browser cookies/cloud tokens require explicit user consent and OS keyring
  storage; no telemetry is enabled by default.
