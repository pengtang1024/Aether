# Releasing Aether

1. Bump `version` in `Cargo.toml` and update `CHANGELOG.md`.
2. Add user-facing notes at `docs/releases/<version>.md`.
3. Create/push a branch named `release/<version>`; the suffix must match
   `Cargo.toml`. Only these branches trigger packaging.
4. Wait for the `Package release` workflow. It builds:
   - Linux x86_64/arm64 `.deb` + AppImage
   - Windows x86_64/arm64 setup + portable zip
   - macOS universal DMG
   and updates a **draft** GitHub release with the notes and `SHA256SUMS`.
5. Review the draft, then create an annotated tag at the release commit:

   ```bash
   git tag -a v<version> -m "Aether <version>"
   git push origin v<version>
   ```

6. Publish the draft release. Do not move or reuse a tag; create a new version
   for subsequent code changes.

## Linux-first gate

- Linux x86_64 and arm64 `.deb` + AppImage are the **release blockers**. A
  release does not ship if either Linux build fails.
- Windows/macOS artifacts remain in the matrix as secondary targets. The
  publish job runs after all builds finish, but only requires `version` and
  `linux` to succeed; Windows/macOS assets are attached when those builds pass.
  If one fails, note the missing platform in the release notes.
- The `.deb`, AppImage, ZIP, installer and DMG must include `LICENSE`,
  `THIRD_PARTY_NOTICES.md` and `LICENSES/Slint-Royalty-free-2.0.md`.

## Signing and notarization (deferred)

- Signing is **not planned for now**: Windows stays unsigned, macOS stays
  ad-hoc signed. Do not spend money on certificates; revisit only if a free OSS
  certificate (SignPath Foundation) is approved or a sponsor provides an Apple
  Developer account.
- If signing is enabled later and secrets are configured, signing must succeed
  before publishing.
- State clearly in release notes that artifacts are unsigned/adhoc.
- Certificate/secret names and commands: [SIGNING.md](SIGNING.md).
