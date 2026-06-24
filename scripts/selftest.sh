#!/usr/bin/env bash
# Jetty self-test harness.
#
# Runs Jetty on an ISOLATED virtual display (Xvfb) so it never touches — or can
# crash — the real desktop. Optionally types a command via xdotool, then grabs a
# screenshot to /tmp/jetty-selftest.png for inspection.
#
# Requires: Xvfb, xdotool (apt install xvfb xdotool). ffmpeg is used for the
# screenshot and is already present.
#
# Usage:
#   scripts/selftest.sh                 # just open + screenshot the prompt
#   scripts/selftest.sh "ls --color"    # open, type the command + Enter, screenshot
set -u

DISP=:99
W=1000
H=640
OUT=/tmp/jetty-selftest.png
APP=./target/release/jetty
APPLOG=/tmp/jetty-app.log

cleanup() {
  [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
  [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null
}
trap cleanup EXIT

if ! command -v Xvfb >/dev/null || ! command -v xdotool >/dev/null; then
  echo "MISSING TOOLS: need Xvfb and xdotool -> sudo apt install -y xvfb xdotool"
  exit 2
fi

pkill -f "Xvfb $DISP" 2>/dev/null
sleep 0.3
rm -f "$OUT" "$APPLOG"

# Force software rendering on the virtual display: there is no real GPU usable
# under Xvfb (NVIDIA Vulkan can't present without DRI3), and this keeps the
# harness entirely off the NVIDIA path. Rasterization is identical to hardware.
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export VK_ICD_FILENAMES=/nonexistent-disable-vulkan.json

# 1) Isolated virtual display
Xvfb "$DISP" -screen 0 "${W}x${H}x24" >/tmp/jetty-xvfb.log 2>&1 &
XVFB_PID=$!
sleep 1

# 2) Launch Jetty (zsh) on the virtual display
SHELL=/usr/bin/zsh DISPLAY="$DISP" "$APP" >"$APPLOG" 2>&1 &
APP_PID=$!
sleep 2.5   # allow window creation + first render

# 3) Optional input (windowfocus works without a window manager)
if [ "${1:-}" != "" ]; then
  WID=$(DISPLAY="$DISP" xdotool search --sync --name JeTTY 2>/dev/null | head -1)
  if [ -n "$WID" ]; then
    DISPLAY="$DISP" xdotool windowfocus "$WID" 2>/dev/null
    sleep 0.3
    DISPLAY="$DISP" xdotool type --delay 60 "$1"
    DISPLAY="$DISP" xdotool key Return
    sleep 1.2
  else
    echo "WARN: Jetty window not found — app may have failed (see $APPLOG)"
  fi
fi

# 4) Screenshot via ffmpeg x11grab (preinstalled)
ffmpeg -loglevel error -f x11grab -video_size "${W}x${H}" -i "${DISP}.0" -frames:v 1 -y "$OUT" 2>/dev/null

echo "=== screenshot: $OUT ==="
ls -lh "$OUT" 2>/dev/null | awk '{print $5, $9}'
echo "=== app log (last 10) ==="
tail -10 "$APPLOG"
