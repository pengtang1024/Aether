# Code signing policy

This project follows the SignPath Foundation policy for free open-source code
signing, if and when the project is accepted into that program.

- Free code signing provided by [SignPath.io](https://signpath.io), certificate
  by [SignPath Foundation](https://signpath.org).
- Project: Aether (management panel + first-party download plugins).
- Repository: https://github.com/pengtang1024/Aether
- License: MIT for Aether's own code; vendored Slint components retain their
  upstream license (see `THIRD_PARTY_NOTICES.md`).
- Maintainer/approver: `pengtang1024` (repository owner).
- Committers/reviewers: repository owner; changes from others require review
  before merge.
- Release process: `.github/workflows/package.yml` builds only from `release/*`
  branches; each release is manually reviewed before signing/publishing.
- Signing requests are approved manually per release; artifacts are built from
  source in GitHub Actions, never from local unverified binaries.
- Privacy: see [PRIVACY.md](PRIVACY.md). The application does not transfer data
  to other systems unless the user explicitly requests a download/API operation.
- Security reports: see [SECURITY.md](SECURITY.md).

## Current status

Aether is currently unsigned on Windows and ad-hoc signed/not notarized on
macOS. SignPath Foundation eligibility is not yet confirmed, in particular
because Slint 1.18.1 offers GPL/Royalty-free/commercial options and SignPath's
OSS certificate conditions require confirming the license of all components.
No signature should be claimed until a signed artifact is verified with
`signtool verify` (Windows) or `spctl` (macOS).
