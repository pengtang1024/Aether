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

macOS packages are ad-hoc signed but not notarized; Windows packages are
unsigned. Mention this in release notes when relevant.
