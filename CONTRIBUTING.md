# Contributing

Aether's own code is MIT-licensed (see `LICENSE`); vendored Slint widgets and
third-party dependencies keep their upstream licenses (see
`THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.md`).

## Inbound license and sign-off

By submitting a pull request you agree to license your contribution under the
project's MIT license. Use the Developer Certificate of Origin 1.1 by signing
off every commit:

```bash
git commit -s -m "feat: ..."
```

The sign-off certifies that you have the right to submit the code. There is no
separate CLA; if the project later needs dual licensing, contributors will be
asked explicitly.

## Third-party code

- Do not paste code from Stack Overflow, blogs, or other projects without
  recording its license and adding the required notice.
- New dependencies must be permissively licensed. Run
  `cargo deny --workspace check` (licenses/bans/sources/advisories) before
  opening a PR.
- If you add a dependency with a NOTICE or a non-standard license text, update
  `THIRD_PARTY_NOTICES.md` and rerun the generators:

  ```bash
  python3 scripts/generate-third-party-inventory.py
  python3 scripts/generate-third-party-licenses.py
  ```

- Do not remove copyright/SPDX headers from `ui/material/**`.

## Security

Please follow `SECURITY.md` for vulnerability reports; do not open a public
issue for an exploitable problem.

## AI-assisted contributions

AI-assisted patches are welcome under the same terms. You remain responsible
for the code's correctness, licensing, and provenance, and for signing off the
commit.
