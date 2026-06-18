#!/usr/bin/env bash
# Comprehensive UI test harness using Xephyr (a NESTED X server).
#
# Fully isolated from the real desktop: the Jetty window, keyboard, and mouse
# input all live on the nested display :99 — they do NOT touch the user's real
# session, focus, or windows. Unlike Xvfb, Xephyr provides GLX/Present so wgpu
# can actually present. Lets Claude self-test the WHOLE UI (typing, shortcuts,
# mouse clicks/drags, the Settings panel, scrollbar) and screenshot the result.
#
# Usage: scripts/uitest.sh OUT.png 'ACTIONS'
#   ACTIONS: shell commands (xdotool/sleep) run with DISPLAY=:99 after the Jetty
#   window is focused. xdotool examples:
#     xdotool type --delay 60 "ls -la"      # type text
#     xdotool key Return                     # press a key
#     xdotool key ctrl+comma                 # chord (open Settings panel)
#     xdotool key ctrl+shift+t               # cycle theme
#     xdotool mousemove 500 300; xdotool click 1     # move + left click
#     xdotool mousemove 400 320; xdotool mousedown 1; xdotool mousemove 600 320; xdotool mouseup 1  # drag
set -u
DISP=:99
W=1000; H=640
OUT="${1:-/tmp/jetty-ui.png}"
ACTIONS="${2:-}"
APP=/home/burak/custom_terminal_project/target/release/jetty
LOG=/tmp/jetty-ui.log

XEPHYR_PID=""; APP_PID=""
cleanup() {
  [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null
  [ -n "$XEPHYR_PID" ] && kill "$XEPHYR_PID" 2>/dev/null
  sleep 0.3
  [ -n "$APP_PID" ] && kill -9 "$APP_PID" 2>/dev/null
}
trap cleanup EXIT

pkill -f "Xephyr $DISP" 2>/dev/null; sleep 0.3
rm -f "$OUT" "$LOG"

# Nested X server (renders as a window on the real :0, but is its own isolated
# display). -ac = no access control, -noreset = stay up across client exits.
Xephyr "$DISP" -screen "${W}x${H}" -ac -noreset >/tmp/jetty-xephyr.log 2>&1 &
XEPHYR_PID=$!
sleep 1.2

# Force software GL: Vulkan can't present to a nested/virtual X server (no DRI3),
# but Xephyr provides GLX, so llvmpipe presents fine. Also keeps us off NVIDIA.
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export VK_ICD_FILENAMES=/nonexistent-disable-vulkan.json

# Launch Jetty on the nested display with zsh.
SHELL=/usr/bin/zsh DISPLAY="$DISP" "$APP" >"$LOG" 2>&1 &
APP_PID=$!
sleep 3.5   # zsh + p10k init + first render

WID=$(DISPLAY="$DISP" xdotool search --sync --name Jetty 2>/dev/null | tail -1)
if [ -z "$WID" ]; then
  echo "ERROR: Jetty window not found on $DISP"; tail -8 "$LOG"; exit 1
fi
DISPLAY="$DISP" xdotool windowfocus "$WID" 2>/dev/null
DISPLAY="$DISP" xdotool windowactivate "$WID" 2>/dev/null
sleep 0.4

if [ -n "$ACTIONS" ]; then
  DISPLAY="$DISP" bash -c "$ACTIONS"
  sleep 0.8
fi

ffmpeg -loglevel error -f x11grab -video_size "${W}x${H}" -i "${DISP}.0" -frames:v 1 -y "$OUT" 2>/dev/null
echo "wid=$WID  out=$OUT  size=$(ls -lh "$OUT" 2>/dev/null | awk '{print $5}')"
echo "--- app log (tail) ---"; tail -4 "$LOG"
