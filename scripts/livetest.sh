#!/usr/bin/env bash
# Live reproduction on the REAL display (:0): launch Jetty, type keys into it,
# screenshot ONLY the Jetty window, then kill it cleanly. For diagnosing live
# input/render bugs that need a real shell (zsh + p10k) — which the headless
# jetty-shot can't reproduce.
#
# Usage: scripts/livetest.sh ["keys to type"]   (default: "sadf")
set -u
APP=/home/burak/custom_terminal_project/target/release/jetty
OUT=/tmp/jetty-live.png
LOG=/tmp/jetty-live.log
KEYS="${1:-sadf}"

rm -f "$OUT" "$LOG"
SHELL=/usr/bin/zsh "$APP" >"$LOG" 2>&1 &
PID=$!

cleanup() { kill "$PID" 2>/dev/null; sleep 0.3; kill -9 "$PID" 2>/dev/null; }
trap cleanup EXIT

sleep 3   # let zsh + p10k initialize and the window map

WID=$(xdotool search --sync --name JeTTY 2>/dev/null | tail -1)
if [ -z "$WID" ]; then
  echo "ERROR: Jetty window not found"; tail -5 "$LOG"; exit 1
fi

xdotool windowactivate --sync "$WID" 2>/dev/null
sleep 0.4
xdotool type --delay 90 "$KEYS"   # XTEST to the focused (Jetty) window
sleep 1.2

# Capture only the Jetty window region.
eval "$(xdotool getwindowgeometry --shell "$WID")"   # sets X, Y, WIDTH, HEIGHT
ffmpeg -loglevel error -f x11grab -video_size "${WIDTH}x${HEIGHT}" -i ":0.0+${X},${Y}" -frames:v 1 -y "$OUT" 2>/dev/null

echo "wid=$WID geom=${WIDTH}x${HEIGHT}+${X}+${Y}"
ls -lh "$OUT" 2>/dev/null | awk '{print "png:", $5}'
echo "--- app log (tail) ---"; tail -4 "$LOG"
