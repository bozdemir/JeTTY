#!/usr/bin/env bash
#
# verify-idle.sh — v0.23 central-paint-chokepoint PROOF HARNESS (manual gate).
#
# WHY THIS EXISTS (amendment BLOCKING 7): goldens, `jetty-bench`, and
# `JETTY_PERF_LOG` cannot catch the two failure modes this refactor most risks:
#   (a) a DROPPED FINAL PAINT — one-frame staleness after a burst ends, and
#   (b) a BLOCKING-2 regression — an occluded/hidden window that self-drives
#       frames WHILE ITS SHELL PRODUCES OUTPUT (the idle battery is all
#       idle-with-NO-output, so it would miss this).
# This script drives a real `target/release/jetty` on the REAL X11 display,
# samples `pidstat`, and — crucially — reads the `JETTY_FRAME_LOG=1` present
# counter, whose behaviour is the crisp, drain-cost-independent signal:
#   * a burst that ENDS must leave the counter advanced to the settled grid;
#   * an occluded/hidden window with a flooding shell must present ZERO frames
#     (counter FROZEN) — if occlusion gating regressed, the counter would climb.
#
# This CANNOT run in the agent sandbox (no X; MEMORY forbids Xvfb). It is a
# SCRIPTED MANUAL gate the USER runs on their real machine BEFORE any push.
#
# Requires (Linux/X11): xdotool, ffmpeg, pidstat (sysstat), a running WM on :0.
# Optional: imagemagick `compare` for AA-tolerant PNG diffs (falls back to cmp).
#
# Usage:
#   scripts/verify-idle.sh                 # full battery
#   HIDE_KEY="ctrl+grave" scripts/verify-idle.sh   # override the F9/summon hotkey
#   OCC_SECONDS=10 scripts/verify-idle.sh  # longer occlusion CPU window
#
# Exit 0 = all HARD assertions passed. Exit 1 = a regression was caught.

set -u
cd "$(dirname "$0")/.."

APP=./target/release/jetty
SHOTBIN=./target/release/jetty-shot
LOG=/tmp/jetty-verify.log
PNGDIR=/tmp/jetty-verify
HIDE_KEY="${HIDE_KEY:-F9}"        # global summon/hide hotkey (override per config)
OCC_SECONDS="${OCC_SECONDS:-6}"   # pidstat window for the occluded/hidden states
CPU_SOFT_MAX="${CPU_SOFT_MAX:-5.0}"   # soft %CPU ceiling for occluded-with-output
                                       # (draining a `yes` flood is not literally 0;
                                       # the HARD signal is the frozen frame counter)
FAILS=0
mkdir -p "$PNGDIR"; rm -f "$PNGDIR"/*.png "$LOG"

note()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pass()  { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
fail()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAILS=$((FAILS+1)); }
info()  { printf '       %s\n' "$*"; }

# ---- preflight ----
for t in xdotool ffmpeg pidstat; do
  command -v "$t" >/dev/null || { echo "MISSING TOOL: $t (Linux/X11 gate)"; exit 2; }
done
[ -x "$APP" ] || { echo "build first: cargo build --release --bin jetty"; exit 2; }
if [ -z "${DISPLAY:-}" ]; then echo "no DISPLAY — run on the real X11 desktop"; exit 2; fi

# ---- launch jetty with the frame counter on ----
JETTY_FRAME_LOG=1 SHELL=/usr/bin/zsh "$APP" >"$LOG" 2>&1 &
PID=$!
cleanup() { kill "$PID" 2>/dev/null; sleep 0.3; kill -9 "$PID" 2>/dev/null; }
trap cleanup EXIT
sleep 3   # window map + shell init

WID=$(timeout 15 xdotool search --sync --name JeTTY 2>/dev/null | tail -1)
if [ -z "$WID" ]; then echo "ERROR: JeTTY window not found"; tail -8 "$LOG"; exit 1; fi

# ---- helpers ----
frames() { grep -c 'JETTY_FRAME' "$LOG" 2>/dev/null || echo 0; }
frames_main() { grep -c 'JETTY_FRAME .* main' "$LOG" 2>/dev/null || echo 0; }
focus() { xdotool windowactivate --sync "$WID" 2>/dev/null; sleep 0.3; }
is_focused() { [ "$(xdotool getactivewindow 2>/dev/null)" = "$WID" ]; }
typek() { xdotool type --delay 40 -- "$1"; }
keyk()  { xdotool key --clearmodifiers "$1"; }
enter() { xdotool key Return; }
shot() {  # shot <name> — grab ONLY the jetty window
  eval "$(xdotool getwindowgeometry --shell "$WID" 2>/dev/null)"
  ffmpeg -loglevel error -f x11grab -video_size "${WIDTH}x${HEIGHT}" \
    -i ":0.0+${X},${Y}" -frames:v 1 -y "$PNGDIR/$1.png" 2>/dev/null
}
png_same() { # png_same a b -> 0 if visually identical
  if command -v compare >/dev/null; then
    local ae; ae=$(compare -metric AE "$PNGDIR/$1.png" "$PNGDIR/$2.png" null: 2>&1)
    [ "${ae%%.*}" -lt 30 ] 2>/dev/null
  else
    cmp -s "$PNGDIR/$1.png" "$PNGDIR/$2.png"
  fi
}
# max %CPU of the process over DUR seconds
cpu_max() { pidstat -u -p "$PID" 1 "$1" 2>/dev/null \
  | awk '/^[0-9]/ && $NF!="Command" {for(i=1;i<=NF;i++) if($i ~ /^[0-9.]+$/){c=$i} print c}' \
  | sort -rn | head -1; }

focus
is_focused || { echo "ERROR: could not focus JeTTY (WID=$WID) — bail (no typing into other windows)"; exit 1; }

########################################################################
note "1. REPAINT-TRIGGER MATRIX (visible focused main window)"
# Each trigger must advance the frame counter AND change the pixels.
trigger() { # trigger <name> <action-fn>
  local name="$1"; shift
  local f0; f0=$(frames_main); shot "before_$name"
  "$@"; sleep 0.8
  local f1; f1=$(frames_main); shot "after_$name"
  if [ "$f1" -gt "$f0" ]; then pass "$name — frame counter advanced ($f0 -> $f1)"
  else fail "$name — NO new frame presented ($f0 -> $f1)"; fi
  if png_same "before_$name" "after_$name"; then
    info "$name — pixels unchanged (frame-counter is the authority here)"
  else info "$name — pixels changed (expected)"; fi
}
trigger keystroke   bash -c 'xdotool type --delay 40 -- "echo hi"'
trigger pty_output  bash -c 'xdotool key Return'                 # runs `echo hi`
trigger ls_output   bash -c 'xdotool type -- "ls --color"; xdotool key Return'
trigger resize      bash -c 'eval "$(xdotool getwindowgeometry --shell '"$WID"')"; xdotool windowsize '"$WID"' $((WIDTH-40)) $((HEIGHT-40))'
trigger overlay     bash -c 'xdotool key ctrl+shift+p'           # command palette open
keyk Escape; sleep 0.3

########################################################################
note "2. MISSED-PAINT (a burst that ENDS must present its LAST mutation)"
focus
f0=$(frames_main)
typek "seq 1 40"; enter
sleep 2.5                       # let the whole burst drain + settle
f1=$(frames_main)
shot "burst_settle_a"
sleep 1.2                       # no further input
f2=$(frames_main)
shot "burst_settle_b"
if [ "$f1" -gt "$f0" ]; then pass "burst advanced the frame counter ($f0 -> $f1)"
else fail "burst presented no frames ($f0 -> $f1)"; fi
if [ "$f2" -eq "$f1" ]; then pass "counter FROZE after the burst ended (no self-drive; $f1)"
else fail "counter kept climbing with no input ($f1 -> $f2) — a hidden self-drive"; fi
if png_same "burst_settle_a" "burst_settle_b"; then
  pass "final frame is SETTLED (re-grab identical) — no dropped/stale final paint"
else fail "grid still changing after settle — possible dropped final frame"; fi

########################################################################
note "3. OCCLUDED-WITH-OUTPUT (main) — BLOCKING-2 catch"
# Flood the shell, then minimize: an occluded window must present ZERO frames
# (counter FROZEN) and stay ~0% CPU while its shell keeps producing output.
focus
typek "yes > /dev/null &"; enter; sleep 0.2   # background flood, no screen output
typek "yes"; enter                            # foreground flood TO the terminal
sleep 1.0
xdotool windowminimize "$WID"; sleep 1.2      # -> Occluded(true)/iconify path
f0=$(frames)
info "sampling CPU for ${OCC_SECONDS}s while minimized + flooding..."
cmax=$(cpu_max "$OCC_SECONDS")
f1=$(frames)
if [ "${f1:-0}" -eq "${f0:-0}" ]; then
  pass "occluded main presented ZERO frames while flooding ($f0 == $f1)"
else
  fail "occluded main SELF-DROVE $((f1-f0)) frames while flooding — 0%-idle regression"
fi
info "occluded-with-output max CPU = ${cmax:-?}% (soft ceiling ${CPU_SOFT_MAX}%)"
awk -v c="${cmax:-0}" -v m="$CPU_SOFT_MAX" 'BEGIN{exit !(c+0>m+0)}' \
  && fail "occluded CPU ${cmax}% exceeds ${CPU_SOFT_MAX}% (investigate drain cost)" \
  || pass "occluded CPU within soft ceiling"
xdotool windowmap "$WID" 2>/dev/null; xdotool windowactivate --sync "$WID" 2>/dev/null; sleep 0.5
keyk ctrl+c; sleep 0.2; typek "kill %1 2>/dev/null"; enter; keyk ctrl+c; sleep 0.3

########################################################################
note "4. HIDDEN-WITH-OUTPUT (F9 off) — BLOCKING-2 catch"
focus
typek "yes"; enter; sleep 1.0
keyk "$HIDE_KEY"; sleep 1.2                    # global hide (window unmapped)
f0=$(frames)
info "sampling CPU for ${OCC_SECONDS}s while hidden + flooding..."
cmax=$(cpu_max "$OCC_SECONDS")
f1=$(frames)
if [ "${f1:-0}" -eq "${f0:-0}" ]; then
  pass "hidden main presented ZERO frames while flooding ($f0 == $f1)"
else
  fail "hidden main SELF-DROVE $((f1-f0)) frames while flooding — 0%-idle regression"
fi
info "hidden-with-output max CPU = ${cmax:-?}%"
awk -v c="${cmax:-0}" -v m="$CPU_SOFT_MAX" 'BEGIN{exit !(c+0>m+0)}' \
  && fail "hidden CPU ${cmax}% exceeds ${CPU_SOFT_MAX}%" \
  || pass "hidden CPU within soft ceiling"
keyk "$HIDE_KEY"; sleep 0.8                     # re-summon
focus; keyk ctrl+c; sleep 0.3

########################################################################
note "5. DETACHED OCCLUDED-WITH-OUTPUT — BLOCKING-2 catch (per-surface)"
focus
keyk ctrl+shift+d; sleep 1.5                    # detach the active tab
DWID=$(xdotool search --name JeTTY 2>/dev/null | grep -v "^$WID$" | tail -1)
if [ -z "$DWID" ] || [ "$DWID" = "$WID" ]; then
  info "SKIP: could not identify a detached window (detach needs >=2 tabs)."
else
  xdotool windowactivate --sync "$DWID" 2>/dev/null; sleep 0.4
  xdotool type --delay 40 -- "yes"; xdotool key Return; sleep 1.0
  xdotool windowminimize "$DWID"; sleep 1.2
  f0=$(frames)
  info "sampling CPU for ${OCC_SECONDS}s while detached window minimized + flooding..."
  cmax=$(cpu_max "$OCC_SECONDS")
  f1=$(frames)
  if [ "${f1:-0}" -eq "${f0:-0}" ]; then
    pass "occluded DETACHED window presented ZERO frames while flooding ($f0 == $f1)"
  else
    fail "occluded detached window SELF-DROVE $((f1-f0)) frames — 0%-idle regression"
  fi
  info "detached-occluded-with-output max CPU = ${cmax:-?}%"
  xdotool windowmap "$DWID" 2>/dev/null; xdotool windowactivate --sync "$DWID" 2>/dev/null
  xdotool key ctrl+c 2>/dev/null; sleep 0.3
fi

########################################################################
note "6. jetty-shot RENDER GOLDENS (reference PNGs for the reviewer)"
if [ -x "$SHOTBIN" ]; then
  JETTY_SHOT_OUT="$PNGDIR/golden_prompt.png" "$SHOTBIN" >/dev/null 2>&1 \
    && info "wrote $PNGDIR/golden_prompt.png"
  JETTY_SHOT_DETACHED=1 JETTY_SHOT_OUT="$PNGDIR/golden_detached.png" "$SHOTBIN" >/dev/null 2>&1 \
    && info "wrote $PNGDIR/golden_detached.png"
  info "diff these against the pre-refactor goldens (AA tolerance only)."
else
  info "SKIP: build jetty-shot for goldens (cargo build --release --bin jetty-shot)"
fi

########################################################################
note "SUMMARY"
echo "  frame log:   $LOG   (grep JETTY_FRAME)"
echo "  screenshots: $PNGDIR/"
if [ "$FAILS" -eq 0 ]; then
  echo -e "  \033[32mALL HARD ASSERTIONS PASSED\033[0m — chokepoint preserves behaviour on this machine."
  exit 0
else
  echo -e "  \033[31m$FAILS HARD ASSERTION(S) FAILED\033[0m — DO NOT push; investigate above."
  exit 1
fi
