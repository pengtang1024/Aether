# Withdrawn releases

This page records releases whose published binaries were withdrawn. Withdrawn
assets are removed from GitHub Releases so they can no longer be downloaded.
Source tags are kept for provenance unless the source snapshot itself must be
removed.

## v0.1.0 — withdrawn 2026-10-26

**Status:** withdrawn; no binaries are available.

**Reason:** the v0.1.0 packages were built before the third-party license
notices were complete. They did not contain
`LICENSES/Slint-Royalty-free-2.0.md`, and the bundled
`THIRD_PARTY_NOTICES.md` did not record the selected Slint license or the full
redistribution requirements. Shipping them would not meet Aether's own
distribution requirements in [LICENSING.md](LICENSING.md).

**Action:** every release asset was deleted on 2026-10-26, and the
`release/0.1.0` branch was deleted so the old packaging workflow cannot
re-create them. The annotated tag `v0.1.0` remains for source provenance, but
it must not be used to rebuild or redistribute the original packages. Do not
redistribute copies downloaded before the withdrawal.

**Affected assets and their withdrawn SHA-256 digests:**

| Asset | SHA-256 |
| --- | --- |
| `aether-0.1.0-linux-aarch64.AppImage` | `aaf64e463b29de1d13164d1374db8ac57d848c6060de316406ad458164ee07c6` |
| `aether-0.1.0-linux-x86_64.AppImage` | `a8453f0743d426e8bf2aa30fa1b20e99a5b427cc5b418fa66aace9068279bb57` |
| `aether-0.1.0-macos-universal.dmg` | `afba3f06cd1f7102b01867d5bd0f661a39dc29ddde167f8fbfe0f20b71b1d882` |
| `aether-0.1.0-windows-arm64.zip` | `10d8a63fe3162c37ff479e97fe28daa7db4a84a56f5b65142e245e840d9732fc` |
| `aether-0.1.0-windows-x86_64.zip` | `d536e3cbd0dab89ea552df9cc27fe3bb379f09c721f03e80ef6cf1d705f97c95` |
| `aether_0.1.0_amd64.deb` | `6c467345985746eec7813eeaa3e8f4c0efc4e113b7455cdb06b19d56823022e0` |
| `aether_0.1.0_arm64.deb` | `163e23727669a5643998fb9ccc4fa306f0c39a3245fdafa61f2ace011453c4aa` |
| `aether_0.1.0_windows_arm64_setup.exe` | `2364ddb6cec4951fde4b97f43f352df84032ed79cba8f78802afde45905f1252` |
| `aether_0.1.0_windows_x86_64_setup.exe` | `b6a5a19e9438e673cc667f957e77f89b6dfdef370244bedffade5c55aad28778` |
| `SHA256SUMS` | `48db527ad4ab1f4679f85778df6b6d55f015737528d3edf33e56dbc638d5d18d` |

**Replacement:** none yet. The next release must pass the license-bundle gate
and include `LICENSE`, `THIRD_PARTY_NOTICES.md` and the full Slint license text
in every package.

## Policy

- A release is withdrawn when a distributed artifact is missing required
  license notices or attribution.
- Withdraw by deleting every asset and adding a withdrawal notice to the
  release page. Do not silently replace the assets on an existing tag.
- Delete the `release/<version>` branch so the packaging workflow cannot
  rebuild the withdrawn artifacts.
- Keep the source tag for provenance when the source archive is acceptable;
  delete the tag only when the source snapshot is also prohibited.
- Never reuse a withdrawn version number or move its tag. Ship the fix in a
  new version.
- Record the asset names and SHA-256 digests here so users can identify old
  copies.
- Every new package must pass
  `python3 scripts/check-license-bundles.py dist` before publishing.
