# Third-party notices

Aether's own source code is licensed under the MIT license in [LICENSE](LICENSE).

**This repository is not wholly MIT-licensed.** The vendored Slint widget
library under `ui/material/` and the Slint runtime are third-party components
with their own license terms. See the scope table and details below.

## License scope

| Component | License | MIT? |
| --- | --- | --- |
| Aether's own code (`src/**`, `crates/**`, Aether-authored `ui/*.slint`, `ui/pages/**`, scripts, docs) | MIT | Yes |
| Slint Material 3 widgets vendored under `ui/material/**` | `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` | No |
| Slint runtime and `slint-build` (Cargo dependencies) | Same three options | No |
| Brand logo and icons in `assets/` | Aether project, MIT | Yes |

Aether selects the **Slint Royalty-free Desktop, Mobile, and Web Applications
License 2.0** (`LicenseRef-Slint-Royalty-free-2.0`). The full text is in
[`LICENSES/Slint-Royalty-free-2.0.md`](LICENSES/Slint-Royalty-free-2.0.md).

## Slint Material 3 widgets and runtime

The widget library under `ui/material/` is vendored from Slint 1.18.1
(`widgets/{material,common,interfaces}`) and retains its upstream copyright
headers. Copyright © SixtyFPS GmbH. The vendored files are **not** covered by
Aether's MIT license and are available under a choice of:

- `GPL-3.0-only`
- `LicenseRef-Slint-Royalty-free-2.0`
- `LicenseRef-Slint-Software-3.0`

Aether uses the Royalty-free 2.0 option for both the vendored widgets and the
Slint runtime. That license is not copyleft and does not require Aether's own
code to be GPL, but it does require attribution: the "Made with Slint" badge is
displayed on the public release/download page, and an `AboutSlint` widget will
be added to the in-app About dialog. The license also prohibits distributing
Slint standalone, using it in embedded systems, exposing Slint's APIs, or
removing copyright/license notices from the source files.

The alternative upstream options are GPL-3.0-only and
`LicenseRef-Slint-Software-3.0`; see <https://slint.dev> for the full terms.
Redistributions must comply with the selected license.

## Full license texts

Every license family that can ship in a binary is reproduced under
[`LICENSES/`](LICENSES/):

| Text | Applies to |
| --- | --- |
| `Apache-2.0.txt` | reqwest, hyper, tokio, serde and other Apache-2.0 crates |
| `BSD-2-Clause.txt` | arrayref and other BSD-2-Clause crates |
| `BSD-3-Clause.txt` | curve25519-dalek, ed25519-dalek and other BSD-3-Clause crates |
| `ISC.txt` | libloading, aws-lc and other ISC crates |
| `BSL-1.0.txt` | ryu and other Boost-licensed crates |
| `CDLA-Permissive-2.0.txt` | webpki-roots certificate data |
| `MPL-2.0.txt` | option-ext (via directories/dirs-sys on some targets) |
| `Unicode-3.0.txt` | ICU4X / unicode-ident data |
| `Zlib.txt` | miniz_oxide and other Zlib crates |
| `Slint-Royalty-free-2.0.md` | Slint runtime and vendored Material 3 widgets |
| `AWS-LC-LICENSE.txt` | AWS-LC / BoringSSL / OpenSSL code statically linked through `aws-lc-sys` |
| `ring-LICENSE*.txt`, `ring-once_cell-*.txt`, `ring-fiat-LICENSE.txt` | `ring`'s ISC/BoringSSL/once_cell/fiat composite |
| `libssh2-COPYING.txt`, `libssh2-*.txt` | bundled libssh2 (BSD-3 plus curl/FSFULLR/SAX-PD-2.0 portions) |
| `QuickJS-LICENSE.txt` | QuickJS engine bundled by `rquickjs-sys` for PAC evaluation |
| `moka-NOTICE.txt` | Moka's additional Apache-2.0 notice (frequency sketch / timer wheel) |
| `SQLite-blessing.txt` | public-domain SQLite blessing (bundled `rusqlite`) |

`Cargo.lock` is the authoritative, version-pinned inventory of every Rust
dependency; [`docs/THIRD_PARTY_INVENTORY.md`](docs/THIRD_PARTY_INVENTORY.md)
lists every compiled-in package with its SPDX expression and release targets,
[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) reproduces the per-crate
LICENSE/COPYING/NOTICE texts and copyright lines, and release builds also
attach a CycloneDX SBOM. The texts above are shipped
verbatim in source and binary distributions.

**MPL-2.0 note:** `option-ext` (pulled by `directories`/`dirs-sys` on some
targets) is used unmodified. Its complete source is available from crates.io
at the exact version in `Cargo.lock`; the MPL-2.0 text is in
`LICENSES/MPL-2.0.txt`. No MPL-covered file is modified by Aether.

## Rust dependencies

The workspace uses permissively licensed crates (MIT, Apache-2.0, BSD, ISC,
Zlib, BSL-1.0, Unicode-3.0) plus the Slint exception described above and the
MPL-2.0 `option-ext` note. Exact versions are enumerated in `Cargo.lock`; the
release SBOM names every one.

The Host bundles **QuickJS** through the `rquickjs` crate to evaluate PAC
scripts (daemon only, not linked into plugins or the UI). QuickJS is
MIT-licensed (copyright Fabrice Bellard, Charlie Gordon, Ben Noordhuis and
Saúl Ibarra Corretgé); the full text is in
`LICENSES/QuickJS-LICENSE.txt`. `rquickjs`/`rquickjs-sys` are MIT-licensed.
Prefer updating `rquickjs` over patching the vendored QuickJS sources.

**Bundled C libraries.** Several crates compile third-party C into the
binaries, so their upstream composite licenses travel with the product:

- `aws-lc-sys` (TLS) links AWS-LC, BoringSSL and OpenSSL-derived code —
  `LICENSES/AWS-LC-LICENSE.txt` reproduces the full notice chain.
- `ring` (Ed25519/TLS support) has ISC/BoringSSL/once_cell/fiat components —
  see the `ring-*` texts.
- `libssh2-sys` (SFTP) statically links libssh2; its COPYING plus the bundled
  BSD/curl/FSFULLR/SAX-PD-2.0 texts are in `LICENSES/libssh2-*`.
- `rusqlite`/`libsqlite3-sys` compile SQLite, which is public domain; the
  blessing is preserved in `LICENSES/SQLite-blessing.txt`.
- `moka` (DNS cache) has an additional Apache-2.0 notice for two files —
  `LICENSES/moka-NOTICE.txt`.

## Distribution requirements

- Keep `LICENSE`, this file and `LICENSES/Slint-Royalty-free-2.0.md` in source
  and binary distributions (`.deb`, AppImage, ZIP, installer, DMG).
- Keep the upstream copyright/SPDX headers in `ui/material/**`.
- Do not describe the whole repository or binary as "MIT only".
- Do not remove the "Made with Slint" attribution from the download page until
  an in-app `AboutSlint` widget is shipped.

See [docs/LICENSING.md](docs/LICENSING.md) for the full explanation, including
Linux distribution-repository caveats (Debian/Ubuntu may reject the
Royalty-free option as non-DFSG).
