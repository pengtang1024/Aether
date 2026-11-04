#!/usr/bin/env bash
# Reproduce docs/screenshots/ from a fixed fixture.
#
#   ./scripts/capture-docs-screenshots.sh [output-dir]
#
# Environment:
#   AETHER_SHOT_ROOT     demo root (default /tmp/aether-shots)
#   AETHER_SHOT_PLUGINS  first-party plugin directory (default: packaged
#                        /usr/lib/aether/plugins, else build/plugins, else
#                        staged from the build into $AETHER_SHOT_ROOT/plugins)
#   AETHER_BIN_DIR       directory holding aether/aether-daemon/aether-cli
#                        (default target/debug, falls back to target/release)
#   AETHER_SHOT_DISPLAY  private X display (default :97)
#   AETHER_SHOT_KEEP=1   keep the demo root instead of deleting it
#
# Requires Xvfb, ImageMagick `import`, python3 and (optionally) xdotool.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEMO="${AETHER_SHOT_ROOT:-/tmp/aether-shots}"
DISP="${AETHER_SHOT_DISPLAY:-:97}"
OUT="${1:-$ROOT/docs/screenshots}"
BIN="${AETHER_BIN_DIR:-$ROOT/target/debug}"
[ -x "$BIN/aether" ] || BIN="$ROOT/target/release"
for tool in "$BIN/aether" "$BIN/aether-daemon" "$BIN/aether-cli"; do
    [ -x "$tool" ] || { echo "missing $tool (build first)" >&2; exit 1; }
done

DAEMON_PID="" SERVE_PID="" XVFB_PID="" APP_PID=""
cleanup() {
    for pid in "$APP_PID" "$DAEMON_PID" "$SERVE_PID" "$XVFB_PID"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    done
    [ "${AETHER_SHOT_KEEP:-0}" = "1" ] || rm -rf "$DEMO"
}
trap cleanup EXIT

pkill -f "Xvfb $DISP" 2>/dev/null || true
rm -rf "$DEMO"
mkdir -p "$DEMO/config" "$DEMO/downloads" "$DEMO/bin" "$OUT"
cp "$BIN/aether-daemon" "$DEMO/bin/aether-daemon"

if [ -n "${AETHER_SHOT_PLUGINS:-}" ]; then
    PLUGINS="$AETHER_SHOT_PLUGINS"
elif [ -d /usr/lib/aether/plugins ]; then
    PLUGINS=/usr/lib/aether/plugins
elif [ -d "$ROOT/build/plugins" ]; then
    PLUGINS="$ROOT/build/plugins"
else
    # Nothing packaged yet: stage the build's plugin binaries. This runs
    # scripts/build-ffmpeg.py, which downloads and compiles a minimal ffmpeg
    # on first use (cached under .cache/ffmpeg).
    PLUGINS="$DEMO/plugins"
    python3 "$ROOT/scripts/stage-plugins.py" --target-dir "$BIN" --out "$PLUGINS" >/dev/null
fi
[ -d "$PLUGINS" ] || { echo "missing plugin directory: $PLUGINS" >&2; exit 1; }

Xvfb "$DISP" -screen 0 900x600x24 >/dev/null 2>&1 &
XVFB_PID=$!
python3 "$ROOT/scripts/shot-fixture.py" serve >"$DEMO/serve.log" 2>&1 &
SERVE_PID=$!

export AETHER_SOCKET="$DEMO/daemon.sock"
export XDG_CONFIG_HOME="$DEMO/config"
export AETHER_PLUGINS_DIR="$PLUGINS"
export AETHER_UPDATE_URL=
export AETHER_RPC_LISTEN=127.0.0.1:6899
export AETHER_NO_AUTOSTART=1
"$DEMO/bin/aether-daemon" --download-dir "$DEMO/downloads" >"$DEMO/daemon.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 50); do
    [ -S "$AETHER_SOCKET" ] && break
    sleep 0.1
done
python3 "$ROOT/scripts/shot-fixture.py" seed
"$BIN/aether-cli" remote add http://127.0.0.1:8899/archive-demo.zip \
    --settings '{"connections":8}' --socket "$AETHER_SOCKET" >/dev/null
"$BIN/aether-cli" remote add http://127.0.0.1:8899/video-demo.mp4 \
    --settings '{"connections":4}' --socket "$AETHER_SOCKET" >/dev/null
sleep 1

# capture <file> <seconds> [VAR=value ...]
capture() {
    local file="$1" wait="$2"
    shift 2
    env -u WAYLAND_DISPLAY DISPLAY="$DISP" WINIT_UNIX_BACKEND=x11 \
        LIBGL_ALWAYS_SOFTWARE=1 "$@" "$BIN/aether" >"$DEMO/app.log" 2>&1 &
    APP_PID=$!
    sleep "$wait"
    if command -v xdotool >/dev/null 2>&1; then
        env -u WAYLAND_DISPLAY DISPLAY="$DISP" xdotool mousemove 1 1 2>/dev/null || true
        sleep 0.2
    fi
    env -u WAYLAND_DISPLAY DISPLAY="$DISP" import -window root "$OUT/$file" 2>/dev/null || true
    kill "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
    echo "captured $OUT/$file"
}

capture home.png 8 AETHER_THEME=dark
capture downloads.png 8 AETHER_VIEW=downloads AETHER_THEME=dark
capture settings.png 10 AETHER_VIEW=settings AETHER_THEME=dark
capture settings-light.png 10 AETHER_VIEW=settings AETHER_THEME=light
capture plugins.png 10 AETHER_VIEW=settings AETHER_SETTINGS_TAB=5 AETHER_THEME=dark
capture settings-theme.png 10 AETHER_VIEW=settings AETHER_SETTINGS_TAB=7 AETHER_THEME=dark
capture home-light.png 8 AETHER_THEME=light
capture dialog.png 8 AETHER_VIEW=dialog AETHER_THEME=dark
capture drawer.png 10 AETHER_VIEW=drawer AETHER_THEME=dark
