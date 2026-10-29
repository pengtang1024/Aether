#!/bin/sh
# Build a store-uploadable archive of the browser extension.
# Usage: scripts/build-extension.sh [output-dir]
# The zip contains the extension files at its root (Chrome Web Store and
# Firefox Add-ons both expect that layout).
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist}"
VERSION=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$ROOT/browser-extension/manifest.json" | head -1)
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$OUT"
cp -R "$ROOT/browser-extension/." "$STAGE/aether-extension/"
# Internal docs and the store listing itself are not part of the upload.
rm -f "$STAGE/aether-extension/README.md" "$STAGE/aether-extension/store-listing.md"

(cd "$STAGE/aether-extension" && zip -qr "$OUT/aether-extension-${VERSION}.zip" .)
echo "wrote $OUT/aether-extension-${VERSION}.zip"
