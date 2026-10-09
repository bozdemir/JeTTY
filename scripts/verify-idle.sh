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
# This script drives a real `target/release/jetty` live on a NESTED display
# (scripts/nested-live.sh: an invisible Xvfb with software Vulkan and its own
# KWin, config dir, IPC socket and session bus — so it is safe next to the
# JeTTY you are using: nothing here can reach it or your desktop), samples
# `pidstat`, and — crucially — reads the `JETTY_FRAME_LOG=1` present counter,
# whose behaviour is the crisp, drain-cost-independent signal:
#   * a burst that ENDS must leave the counter advanced to the settled grid;
#   * an occluded/hidden window with a flooding shell must present ZERO frames
#     (counter FROZEN) — if occlusion gating regressed, the counter would climb.
#
# A SCRIPTED MANUAL gate: run it before a push that touches the paint paths.
#
# Requires what scripts/nested-live.sh needs (Xvfb, xdotool, python3 with
# python-xlib + Pillow, the lavapipe ICD), plus kwin_x11 (minimize needs a
# window manager) and pidstat (sysstat). Optional: imagemagick `compare` for
# AA-tolerant PNG diffs (falls back to cmp).
#
# Usage:
#   scripts/verify-idle.sh                  # full battery, nested display :189
#   DISPLAY_NUM=191 scripts/verify-idle.sh  # another nested display
#   OCC_SECONDS=10 scripts/verify-idle.sh   # longer occlusion CPU window
#
# Exit 0 = all HARD assertions passed. Exit 1 = a regression was caught.

set -u
cd "$(dirname "$0")/.."

export DISPLAY_NUM="${DISPLAY_NUM:-189}"
NL=scripts/nested-live.sh
SB="target/nested-live-$DISPLAY_NUM"   # nested-live's sandbox for this display
APP=./target/release/jetty
SHOTBIN=./target/release/jetty-shot
LOG="$SB/jetty.log"                    # the nested jetty's stdout + stderr
PNGDIR="$SB/verify"
OCC_SECONDS="${OCC_SECONDS:-6}"   # pidstat window for the occluded/hidden states
CPU_SOFT_MAX="${CPU_SOFT_MAX:-5.0}"   # soft %CPU ceiling for occluded-with-output
                                       # (draining a `yes` flood is not literally 0;
                                       # the HARD signal is the frozen frame counter)
FAILS=0

note()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pass()  { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
fail()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAILS=$((FAILS+1)); }
info()  { printf '       %s\n' "$*"; }

# ---- preflight ----
for t in Xvfb xdotool kwin_x11 pidstat; do
  command -v "$t" >/dev/null || { echo "MISSING TOOL: $t"; exit 2; }
done
[ -x "$APP" ] || { echo "build first: cargo build --release --bin jetty"; exit 2; }
if [ -e "/tmp/.X11-unix/X$DISPLAY_NUM" ]; then
  echo "display :$DISPLAY_NUM is in use — pick a free one with DISPLAY_NUM"; exit 2
fi

# ---- the nested session (never the real display, never your JeTTY) ----
# Default settings (the shortcuts below are the defaults) but no auto-hide on
# focus loss — a minimized window would hide itself, and step 3 is about a
# minimized one — plus the frame counter, zsh. nested-live.sh keeps its own
# config dir, socket and bus, and its `stop` kills only what its `start`
# launched.
mkdir -p "$SB/config/jetty" "$PNGDIR"
rm -f "$PNGDIR"/*.png "$SB/config/jetty/config.toml"
echo 'focus_autohide = false' >"$SB/verify-config.toml"
: >"$LOG"
trap '"$NL" stop >/dev/null' EXIT
JETTY_FRAME_LOG=1 SHELL=/usr/bin/zsh NESTED_WM=kwin NESTED_CONFIG="$SB/verify-config.toml" \
  "$NL" start || { echo "ERROR: the nested session did not start"; tail -12 "$LOG"; exit 1; }
PID=$(cat "$SB/jetty.pid")
sleep 3   # shell init
# Catch a genuine launch failure (crash / GPU init error).
if ! kill -0 "$PID" 2>/dev/null; then
  echo "ERROR: the test instance exited immediately (crash or GPU init failure). Log:"
  tail -12 "$LOG"; exit 1
fi
if ! grep -q 'JETTY_FRAME' "$LOG" 2>/dev/null; then
  echo "WARNING: no frames logged yet after 3s — the frame counter may not be active."
  tail -6 "$LOG"
fi

# xdotool on the nested display only. Windows are found by the test
# instance's pid — never by name, which a real JeTTY's windows share.
xdo() { "$NL" x "$@"; }
WID=$(timeout 15 "$NL" x search --sync --all --pid "$PID" --name JeTTY 2>/dev/null | tail -1)
if [ -z "$WID" ]; then echo "ERROR: JeTTY window not found"; tail -8 "$LOG"; exit 1; fi

# ---- helpers ----
# `grep -c` PRINTS "0" AND exits 1 on no match, so `grep -c … || echo 0` emits
# "0\n0" — which breaks `[ "$f" -eq … ]` ("integer expression expected").
# Capture via command substitution so each helper yields exactly ONE integer.
frames() { local c; c=$(grep -c 'JETTY_FRAME' "$LOG" 2>/dev/null); echo "${c:-0}"; }
frames_main() { local c; c=$(grep -c 'JETTY_FRAME .* main' "$LOG" 2>/dev/null); echo "${c:-0}"; }
# Per-surface counts so an occluded-target test can't false-FAIL on a stray
# present by ANOTHER surface (e.g. a visible main window blinking its caret while
# the DETACHED window is the one under test). Frame log = `JETTY_FRAME <n> <surface>`.
frames_detached() { local c; c=$(grep -c 'JETTY_FRAME .* detached' "$LOG" 2>/dev/null); echo "${c:-0}"; }
focus() { xdo windowactivate --sync "$WID" 2>/dev/null; sleep 0.3; }
is_focused() { [ "$(xdo getactivewindow 2>/dev/null)" = "$WID" ]; }
typek() { xdo type --delay 40 -- "$1"; }
keyk()  { xdo key --clearmodifiers "$1"; }
enter() { xdo key Return; }
shot() {  # shot <name> — grab ONLY the jetty window
  local WINDOW X Y WIDTH HEIGHT SCREEN
  eval "$(xdo getwindowgeometry --shell "$WID" 2>/dev/null)"
  "$NL" shot "$PNGDIR/$1.png" "$X" "$Y" "$WIDTH" "$HEIGHT" >/dev/null 2>&1
}
png_same() { # png_same a b -> 0 if visually identical
  if command -v compare >/dev/null; then
    local ae; ae=$(compare -metric AE "$PNGDIR/$1.png" "$PNGDIR/$2.png" null: 2>&1)
    [ "${ae%%.*}" -lt 30 ] 2>/dev/null
  else
    cmp -s "$PNGDIR/$1.png" "$PNGDIR/$2.png"
  fi
}
# max %CPU of the process over DUR seconds. The column is found by its header
# name: the LAST numeric field of a data line is the CPU core number (pidstat
# prints `… %CPU CPU Command`), and LC_ALL=C keeps an AM/PM column out.
cpu_max() { LC_ALL=C pidstat -u -p "$PID" 1 "$1" 2>/dev/null \
  | awk '/%CPU/ {for(i=1;i<=NF;i++) if($i=="%CPU") c=i; next} c && /^[0-9]/ {print $c}' \
  | sort -rn | head -1; }

focus
is_focused || { echo "ERROR: could not focus JeTTY (WID=$WID)"; exit 1; }

########################################################################
note "1. REPAINT-TRIGGER MATRIX (visible focused main window)"
# Each trigger must advance the frame counter AND change the pixels.
trigger() { # trigger <name> <command…>
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
ls_color() { typek "ls --color"; enter; }
shrink() {
  local WINDOW X Y WIDTH HEIGHT SCREEN
  eval "$(xdo getwindowgeometry --shell "$WID" 2>/dev/null)"
  xdo windowsize "$WID" $((WIDTH-40)) $((HEIGHT-40))
}
trigger keystroke   typek "echo hi"
trigger pty_output  enter                       # runs `echo hi`
trigger ls_output   ls_color
trigger resize      shrink
trigger overlay     keyk ctrl+shift+p           # command palette open
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
xdo windowminimize "$WID"; sleep 1.2          # -> Occluded(true)/iconify path
f0=$(frames_main)
info "sampling CPU for ${OCC_SECONDS}s while minimized + flooding..."
cmax=$(cpu_max "$OCC_SECONDS")
f1=$(frames_main)
if [ "${f1:-0}" -eq "${f0:-0}" ]; then
  pass "occluded main presented ZERO frames while flooding ($f0 == $f1)"
else
  fail "occluded main SELF-DROVE $((f1-f0)) frames while flooding — 0%-idle regression"
fi
info "occluded-with-output max CPU = ${cmax:-?}% (soft ceiling ${CPU_SOFT_MAX}%)"
awk -v c="${cmax:-0}" -v m="$CPU_SOFT_MAX" 'BEGIN{exit !(c+0>m+0)}' \
  && fail "occluded CPU ${cmax}% exceeds ${CPU_SOFT_MAX}% (investigate drain cost)" \
  || pass "occluded CPU within soft ceiling"
xdo windowmap "$WID" 2>/dev/null; focus
keyk ctrl+c; sleep 0.2; typek "kill %1 2>/dev/null"; enter; keyk ctrl+c; sleep 0.3

########################################################################
note "4. HIDDEN-WITH-OUTPUT (summon toggled off) — BLOCKING-2 catch"
# Hidden over the test instance's own socket (`jetty --hide`): the window is
# unmapped exactly as by the summon hotkey.
focus
typek "yes"; enter; sleep 1.0
"$NL" ctl --hide; sleep 1.2
f0=$(frames_main)
info "sampling CPU for ${OCC_SECONDS}s while hidden + flooding..."
cmax=$(cpu_max "$OCC_SECONDS")
f1=$(frames_main)
if [ "${f1:-0}" -eq "${f0:-0}" ]; then
  pass "hidden main presented ZERO frames while flooding ($f0 == $f1)"
else
  fail "hidden main SELF-DROVE $((f1-f0)) frames while flooding — 0%-idle regression"
fi
info "hidden-with-output max CPU = ${cmax:-?}%"
awk -v c="${cmax:-0}" -v m="$CPU_SOFT_MAX" 'BEGIN{exit !(c+0>m+0)}' \
  && fail "hidden CPU ${cmax}% exceeds ${CPU_SOFT_MAX}%" \
  || pass "hidden CPU within soft ceiling"
"$NL" ctl --show; sleep 0.8                   # re-summon
focus; keyk ctrl+c; sleep 0.3

########################################################################
note "5. DETACHED OCCLUDED-WITH-OUTPUT — BLOCKING-2 catch (per-surface)"
focus
keyk ctrl+shift+t; sleep 1.5                    # detach needs >= 2 tabs
keyk ctrl+shift+d; sleep 1.5                    # detach the active tab
DWID=$(xdo search --onlyvisible --pid "$PID" 2>/dev/null | grep -vx "$WID" | tail -1)
if [ -z "$DWID" ]; then
  info "SKIP: could not identify a detached window."
else
  xdo windowactivate --sync "$DWID" 2>/dev/null; sleep 0.4
  xdo type --delay 40 -- "yes"; xdo key Return; sleep 1.0
  xdo windowminimize "$DWID"; sleep 1.2
  f0=$(frames_detached)
  info "sampling CPU for ${OCC_SECONDS}s while detached window minimized + flooding..."
  cmax=$(cpu_max "$OCC_SECONDS")
  f1=$(frames_detached)
  if [ "${f1:-0}" -eq "${f0:-0}" ]; then
    pass "occluded DETACHED window presented ZERO frames while flooding ($f0 == $f1)"
  else
    fail "occluded detached window SELF-DROVE $((f1-f0)) frames — 0%-idle regression"
  fi
  info "detached-occluded-with-output max CPU = ${cmax:-?}%"
  xdo windowmap "$DWID" 2>/dev/null; xdo windowactivate --sync "$DWID" 2>/dev/null
  xdo key ctrl+c 2>/dev/null; sleep 0.3
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
