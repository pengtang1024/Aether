#!/usr/bin/env bash
# Install (or refresh) the Aether desktop shortcut and application-menu entry.
# Re-run this after moving the repository or rebuilding the binary.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/aether"
if [[ -x "$ROOT/target/release/aether" ]]; then
    BIN="$ROOT/target/release/aether"
fi
ICON_NAME="aether"
ICON_THEME="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
DESKTOP_DIR="$(xdg-user-dir DESKTOP 2>/dev/null || printf '%s/Desktop' "$HOME")"

if [[ ! -x "$BIN" ]]; then
    echo "Build the app first: cargo build" >&2
    exit 1
fi

for size in 128 256 512; do
    install -Dm644 "$ROOT/assets/logo-icon-$size.png" \
        "$ICON_THEME/${size}x${size}/apps/$ICON_NAME.png"
done

write_entry() {
    cat > "$1" <<ENTRY
[Desktop Entry]
Type=Application
Version=1.0
Name=Aether
Comment=Native Material 3 download manager mockup
Exec=$BIN
Icon=$ICON_NAME
Terminal=false
Categories=Network;FileTransfer;
StartupWMClass=aether
ENTRY
    chmod +x "$1"
}

mkdir -p "$APPS"
write_entry "$APPS/aether.desktop"

if [[ -d "$DESKTOP_DIR" ]]; then
    write_entry "$DESKTOP_DIR/Aether.desktop"
    gio set "$DESKTOP_DIR/Aether.desktop" metadata::trusted true 2>/dev/null || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$ICON_THEME" >/dev/null 2>&1 || true
fi

echo "installed $APPS/aether.desktop"
[[ -f "$DESKTOP_DIR/Aether.desktop" ]] && echo "installed $DESKTOP_DIR/Aether.desktop"
