# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.3.x | Yes |
| 0.2.x | Security fixes only |
| 0.1.x | No (withdrawn preview) |

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

## Security support period and CRA reporting

Aether provides security fixes for the current release line and the previous
minor line; older releases receive fixes only when a backport is practical.
The maintainer keeps the CycloneDX SBOM and `cargo-deny` advisory scan in CI
(including a weekly scheduled run) and coordinates disclosure privately.

Where the EU Cyber Resilience Act (CRA) applies, the maintainer's target is:

- acknowledge a report within 72 hours;
- issue an early warning to the relevant CSIRT/ENISA within 24 hours of
  confirming an actively exploited vulnerability;
- provide a final report within the CRA's 72-hour window and a fix or mitigation
  in a signed update.

These are engineering targets, not a warranty; the MIT licence's warranty
disclaimer still applies.

## Current security posture

- Plugins are first-party, signed/versioned, and run in separate processes.
  There is **no OS-level sandbox yet** (Landlock/seccomp/AppContainer are
  future work); the host validates executable paths and declared capabilities.
- Download paths are sanitized by the host before finalization; plugins write
  only inside the task staging directory. Checksums and atomic finalization are
  implemented.
- Browser cookies and cloud tokens require explicit user action. Credentials in
  task settings are stored locally in plaintext inside an owner-only (`0600`)
  SQLite database; OS keyring storage is not implemented yet.
- No telemetry is enabled. See [PRIVACY.md](PRIVACY.md) for the exact data
  flows.
