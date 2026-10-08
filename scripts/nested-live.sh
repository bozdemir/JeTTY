#!/usr/bin/env bash
#
# nested-live.sh — run the REAL jetty as a live window on an invisible nested X
# server and drive it with real X input. Safe next to the JeTTY you are using:
# its own display, config dir, socket, autostart dir, session bus and shell
# history; software Vulkan (lavapipe) only — no Intel/NVIDIA device is opened.
#
# Hardware Vulkan can't present on Xvfb (no DRI3); lavapipe can. Optionally a
# real window manager (kwin_x11, NESTED_WM=kwin) runs on the nested display, with
# a private bus that activates no services and no session-manager link, so WM
# policy (focus-stealing prevention, raise, minimize) is real too. That is how
# v0.26.0's F9-raise bug under KWin was found.
#
# Requires: Xvfb, xdotool, setxkbmap, python3 with python-xlib + Pillow,
# dbus-run-session, the lavapipe ICD (mesa-vulkan-drivers); kwin_x11 optional.
#
# Usage (DISPLAY_NUM defaults to 187; never the real display):
#   scripts/nested-live.sh start [layout]   # Xvfb (+ WM) + jetty; layout e.g. tr, us
#   scripts/nested-live.sh x key ctrl+shift+t      # xdotool, pinned to the nested display
#   scripts/nested-live.sh raw-key 75               # XTEST keycode (xdotool adds Alt to F9)
#   scripts/nested-live.sh shot out.png [x y w h [scale]]
#   scripts/nested-live.sh state                   # map state, active window, stacking
#   scripts/nested-live.sh restart                 # relaunch jetty (after a rebuild)
#   scripts/nested-live.sh stop                    # kills only what start launched
#
# Env: DISPLAY_NUM, NESTED_WM=kwin|none (default: kwin when installed),
#      NESTED_CONFIG=<config.toml to copy> (default: ~/.config/jetty/config.toml if
#      present — copied, never written), JETTY_BIN=<binary> (default target/release/jetty).

set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
N="${DISPLAY_NUM:-187}"
SB="$ROOT/target/nested-live-$N"
BIN="${JETTY_BIN:-$ROOT/target/release/jetty}"
D=":$N"

die() { echo "nested-live: $*" >&2; exit 1; }
[ "$N" != "0" ] && [ "$D" != "${DISPLAY:-}" ] || die "refusing to use the real display ($D)"

py() { python3 -I - "$@"; }

launch_jetty() {
    [ -x "$BIN" ] || die "no binary at $BIN (cargo build --release --bin jetty)"
    ln -sfn "$BIN" "$SB/bin/jetty"
    (cd "$SB" && setsid -f env -u JETTY -u JETTY_BIN -u TERM_PROGRAM -u TERM_PROGRAM_VERSION \
        -u WAYLAND_DISPLAY -u SESSION_MANAGER DISPLAY="$D" \
        XDG_CONFIG_HOME="$SB/config" XDG_CACHE_HOME="$SB/cache" XDG_DATA_HOME="$SB/data" \
        XDG_STATE_HOME="$SB/state" XDG_RUNTIME_DIR="$SB/run" JETTY_CONFIG_DIR="$SB/config/jetty" \
        PATH="$SB/bin:$PATH" HISTFILE="$SB/zsh_history" GITSTATUS_CACHE_DIR="$HOME/.cache/gitstatus" \
        VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
        VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json \
        dbus-run-session -- "$SB/bin/jetty" >>"$SB/jetty.log" 2>&1)
    for _ in $(seq 50); do
        pid=$(for p in $(pgrep -x jetty); do
            tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qx "JETTY_CONFIG_DIR=$SB/config/jetty" && echo "$p"
        done | head -1)
        [ -n "$pid" ] && break
        sleep 0.1
    done
    [ -n "${pid:-}" ] || die "jetty did not start — see $SB/jetty.log"
    echo "$pid" >"$SB/jetty.pid"
    echo "jetty pid $pid (log $SB/jetty.log)"
}

stop_pid() { # only a pid we recorded, and only if it is still the process we started
    local f="$SB/$1.pid" want="$2" p
    [ -f "$f" ] || return 0
    p=$(cat "$f")
    if [ -n "$p" ] && [ -d "/proc/$p" ] && [ "$(cat /proc/$p/comm)" = "$want" ]; then kill "$p"; fi
    rm -f "$f"
}

case "${1:-}" in
start)
    [ -e "/tmp/.X11-unix/X$N" ] && die "display $D is in use (stop it first, or set DISPLAY_NUM)"
    mkdir -p "$SB"/{config/jetty,cache,data,state,run,bin,wm/config,wm/cache,wm/data,wm/state,wm/run}
    chmod 700 "$SB/run" "$SB/wm/run"
    cfg="${NESTED_CONFIG:-$HOME/.config/jetty/config.toml}"
    [ -f "$SB/config/jetty/config.toml" ] || { [ -f "$cfg" ] && cp "$cfg" "$SB/config/jetty/config.toml"; }
    Xvfb "$D" -screen 0 1920x1080x24 -nolisten tcp -noreset >"$SB/xvfb.log" 2>&1 &
    echo $! >"$SB/xvfb.pid"
    for _ in $(seq 50); do [ -S "/tmp/.X11-unix/X$N" ] && break; sleep 0.1; done
    DISPLAY="$D" setxkbmap -layout "${2:-us}"
    wm="${NESTED_WM:-$(command -v kwin_x11 >/dev/null && echo kwin || echo none)}"
    if [ "$wm" = kwin ]; then
        cat >"$SB/wm/bus.conf" <<'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <!-- private bus with no service activation: nothing else gets started -->
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
        (cd "$SB/wm" && setsid -f env -u SESSION_MANAGER -u KDE_FULL_SESSION -u KDE_SESSION_UID \
            -u KDE_SESSION_VERSION -u KDE_APPLICATIONS_AS_SCOPE -u QT_IM_MODULE -u XMODIFIERS \
            -u WAYLAND_DISPLAY DISPLAY="$D" XDG_CONFIG_HOME="$SB/wm/config" XDG_CACHE_HOME="$SB/wm/cache" \
            XDG_DATA_HOME="$SB/wm/data" XDG_STATE_HOME="$SB/wm/state" XDG_RUNTIME_DIR="$SB/wm/run" \
            KWIN_COMPOSE=N QT_QPA_PLATFORM=xcb \
            dbus-run-session --config-file="$SB/wm/bus.conf" -- kwin_x11 >"$SB/wm.log" 2>&1)
        for _ in $(seq 50); do
            p=$(for q in $(pgrep -x kwin_x11); do
                tr '\0' '\n' <"/proc/$q/environ" 2>/dev/null | grep -qx "DISPLAY=$D" && echo "$q"
            done | head -1)
            [ -n "$p" ] && { echo "$p" >"$SB/wm.pid"; break; }
            sleep 0.1
        done
        sleep 1
    fi
    launch_jetty
    echo "nested display $D ready (wm: $wm). Drive it with: $0 x …"
    ;;
restart)
    stop_pid jetty jetty
    sleep 1
    launch_jetty
    ;;
x)
    shift
    exec env DISPLAY="$D" xdotool "$@"
    ;;
raw-key)
    py "$D" "$2" <<'EOF'
import sys, time
from Xlib import display, X
from Xlib.ext import xtest
d = display.Display(sys.argv[1]); kc = int(sys.argv[2])
xtest.fake_input(d, X.KeyPress, kc); d.sync(); time.sleep(0.03)
xtest.fake_input(d, X.KeyRelease, kc); d.sync()
EOF
    ;;
shot)
    py "$D" "${@:2}" <<'EOF'
import sys
from Xlib import display, X
from PIL import Image
d = display.Display(sys.argv[1]); root = d.screen().root; g = root.get_geometry()
out = sys.argv[2]
x, y, w, h = (int(v) for v in sys.argv[3:7]) if len(sys.argv) >= 7 else (0, 0, g.width, g.height)
img = Image.frombytes('RGB', (w, h), root.get_image(x, y, w, h, X.ZPixmap, 0xffffffff).data, 'raw', 'BGRX')
if len(sys.argv) >= 8:
    s = float(sys.argv[7]); img = img.resize((int(w * s), int(h * s)), Image.LANCZOS)
img.save(out); print(out, img.size)
EOF
    ;;
state)
    py "$D" <<'EOF'
import sys
from Xlib import display, Xatom
d = display.Display(sys.argv[1]); root = d.screen().root
A = d.intern_atom
def name(w):
    try:
        p = d.create_resource_object('window', w).get_full_property(A('_NET_WM_NAME'), A('UTF8_STRING'))
        n = p.value.decode('utf-8', 'replace') if p else hex(w)
    except Exception:
        n = hex(w)
    return f'{n[:40]} (0x{w:x})'
act = root.get_full_property(A('_NET_ACTIVE_WINDOW'), Xatom.WINDOW)
st = root.get_full_property(A('_NET_CLIENT_LIST_STACKING'), Xatom.WINDOW)
print('active:', name(act.value[0]) if act and act.value[0] else 'none')
for w in (st.value if st else []):
    ms = d.create_resource_object('window', w).get_attributes().map_state
    print('  ', {0: 'unmapped', 1: 'unviewable', 2: 'viewable'}[ms], name(w))
EOF
    ;;
stop)
    stop_pid jetty jetty
    stop_pid wm kwin_x11
    stop_pid xvfb Xvfb
    echo "stopped (sandbox kept at $SB)"
    ;;
*)
    sed -n '2,32p' "$0"
    exit 1
    ;;
esac
