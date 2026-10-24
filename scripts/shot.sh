#!/usr/bin/env bash
# Launch Aether on a private Xvfb display, capture a screenshot, then shut down.
#
#   ./scripts/shot.sh <output.png> [seconds-to-wait] [width] [height]
set -euo pipefail

OUT="${1:-/tmp/opencode/aether.png}"
WAIT="${2:-6}"
W="${3:-1600}"
H="${4:-1000}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DISP="${AETHER_DISPLAY:-:97}"

# Make sure no stale instance is holding the display.
pkill -f '[t]arget/debug/aether' 2>/dev/null || true
pkill -f "Xvfb ${DISP}" 2>/dev/null || true
sleep 0.3

Xvfb "$DISP" -screen 0 "${W}x${H}x24" >/dev/null 2>&1 &
XVFB_PID=$!
sleep 1.2

# WAYLAND_DISPLAY must be cleared: winit would otherwise connect to the host
# Wayland compositor instead of our private X server.
env -u WAYLAND_DISPLAY DISPLAY="$DISP" \
    WINIT_UNIX_BACKEND=x11 \
    LIBGL_ALWAYS_SOFTWARE=1 \
    "$ROOT/target/debug/aether" >/tmp/opencode/aether.log 2>&1 &
APP_PID=$!

sleep "$WAIT"

env -u WAYLAND_DISPLAY DISPLAY="$DISP" import -window root "$OUT" 2>/dev/null || true

sleep 0.3
kill "$APP_PID" 2>/dev/null || true
kill "$XVFB_PID" 2>/dev/null || true
wait "$APP_PID" 2>/dev/null || true
wait "$XVFB_PID" 2>/dev/null || true

if [[ ! -s "$OUT" ]]; then
    echo "screenshot failed"; cat /tmp/opencode/aether.log; exit 1
fi
echo "captured $OUT ($(identify -format '%wx%h' "$OUT" 2>/dev/null || echo '?'))"
