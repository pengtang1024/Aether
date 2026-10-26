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

## Rust dependencies

The plugin crates use `serde`, `serde_json`, `rmp-serde`, `semver` and
`thiserror`. They are permissively licensed (MIT and/or Apache-2.0) and are
enumerated with exact versions in `Cargo.lock`.

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
