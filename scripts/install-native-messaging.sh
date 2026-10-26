#!/bin/sh
# Install Aether Native Messaging manifests for the current user.
# Usage: install-native-messaging.sh <chrome-extension-id> [host-path]
set -eu

EXTENSION_ID="${1:-}"
HOST_PATH="${2:-/usr/bin/aether-native-host}"

if [ ! -x "$HOST_PATH" ]; then
  echo "native host is not executable: $HOST_PATH" >&2
  exit 1
fi
if [ -z "$EXTENSION_ID" ] && [ -z "${AETHER_ALLOW_FIREFOX_ONLY:-}" ]; then
  echo "usage: $0 <chrome-extension-id> [host-path]" >&2
  echo "Firefox-only: AETHER_ALLOW_FIREFOX_ONLY=1 $0 '' $HOST_PATH" >&2
  exit 1
fi

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
TEMPLATE_DIR="$ROOT/packaging/native-messaging"
if [ ! -d "$TEMPLATE_DIR" ]; then
  TEMPLATE_DIR="/usr/share/aether/native-messaging"
fi

if [ -n "$EXTENSION_ID" ]; then
  for dir in \
    "$HOME/.config/google-chrome/NativeMessagingHosts" \
    "$HOME/.config/chromium/NativeMessagingHosts" \
    "$HOME/.config/microsoft-edge/NativeMessagingHosts" \
    "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
  do
    mkdir -p "$dir"
    sed -e "s|@HOST_PATH@|$HOST_PATH|g" \
        -e "s|@EXTENSION_ID@|$EXTENSION_ID|g" \
        "$TEMPLATE_DIR/com.pengtang1024.aether.chrome.json" \
        > "$dir/com.pengtang1024.aether.json"
    chmod 644 "$dir/com.pengtang1024.aether.json"
  done
  echo "installed Chrome/Edge/Chromium native manifest for $EXTENSION_ID"
fi

mkdir -p "$HOME/.mozilla/native-messaging-hosts"
sed -e "s|@HOST_PATH@|$HOST_PATH|g" \
    "$TEMPLATE_DIR/com.pengtang1024.aether.firefox.json" \
    > "$HOME/.mozilla/native-messaging-hosts/com.pengtang1024.aether.json"
chmod 644 "$HOME/.mozilla/native-messaging-hosts/com.pengtang1024.aether.json"
echo "installed Firefox native manifest"
