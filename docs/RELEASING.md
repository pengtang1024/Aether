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
   The workflow also runs the license-bundle checks described below; a missing
   notice fails the build before anything is attached.
5. Review the draft and confirm that the license checks passed, then create an
   annotated tag at the release commit:

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
- The `.deb` and AppImage include `LICENSE`, `THIRD_PARTY_NOTICES.md`, every
  text under `LICENSES/` and all staged plugins under
  `/usr/lib/aether/plugins` (`make plugins`).
- Windows/macOS packages currently contain the Host only; plugin packaging for
  those platforms is pending.

## License-bundle gate

Run this before publishing; the package workflow runs the same checks:

```bash
make check-licenses
python3 scripts/check-license-bundles.py dist
```

- `--check-recipes` verifies the source license files exist and that the
  Makefile, `package.yml` and Inno Setup recipe reference them.
- The artifact check opens `.deb`, AppImage, ZIP and DMG files and requires a
  non-empty MIT license file, `THIRD_PARTY_NOTICES.md` and every full license
  text under `LICENSES/`. Inno Setup payloads are covered by the recipe check.
- Do not publish a draft if either check fails. Fix the packaging first; do not
  remove the check.

## Withdrawing a release

If a published artifact is found to be missing required license notices or
attribution, withdraw it immediately:

1. Delete every asset from the GitHub release and delete the
   `release/<version>` branch. Do not replace assets on the existing tag and
   do not move the tag.
2. Replace the release notes with a clear withdrawal notice and link to
   [WITHDRAWN_RELEASES.md](WITHDRAWN_RELEASES.md).
3. Add the asset names and SHA-256 digests to
   [WITHDRAWN_RELEASES.md](WITHDRAWN_RELEASES.md).
4. Keep the source tag for provenance unless the source snapshot itself must be
   removed. Never reuse the withdrawn version number.
5. Ship the fix in a new version after the license-bundle gate passes.

`v0.1.0` was withdrawn on 2026-10-26 for this reason; see
[WITHDRAWN_RELEASES.md](WITHDRAWN_RELEASES.md).

## Signing and notarization (deferred)

- Signing is **not planned for now**: Windows stays unsigned, macOS stays
  ad-hoc signed. Do not spend money on certificates; revisit only if a free OSS
  certificate (SignPath Foundation) is approved or a sponsor provides an Apple
  Developer account.
- If signing is enabled later and secrets are configured, signing must succeed
  before publishing.
- State clearly in release notes that artifacts are unsigned/adhoc.
- Certificate/secret names and commands: [SIGNING.md](SIGNING.md).
