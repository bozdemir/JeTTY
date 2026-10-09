#!/usr/bin/env bash
#
# nested-live.sh — run the REAL jetty as a live window on an invisible nested X
# server (or a nested headless Wayland compositor) and drive it with real input.
# Safe next to the JeTTY you are using: its own display, config dir, socket,
# autostart dir, session bus and shell history; software Vulkan (lavapipe) only
# — no Intel/NVIDIA device is opened.
#
# Hardware Vulkan can't present on Xvfb (no DRI3); lavapipe can. Optionally a
# real window manager (kwin_x11, NESTED_WM=kwin) runs on the nested display, with
# a private bus that activates no services and no session-manager link, so WM
# policy (focus-stealing prevention, raise, minimize) is real too. That is how
# v0.26.0's F9-raise bug under KWin was found. NESTED_WM=kwin-wayland runs a
# headless kwin_wayland instead (same isolation) and jetty as a native Wayland
# client: input goes through KWin's fake-input protocol, `shot` through its
# ScreenShot2 API, `state` through a KWin script — all on the private bus, with
# KWin's permission checks off for that nested instance only.
#
# Requires: Xvfb, xdotool, setxkbmap, python3 with python-xlib + Pillow,
# dbus-run-session, the lavapipe ICD (mesa-vulkan-drivers); kwin_x11 optional;
# kwin_wayland + python3-dbus for NESTED_WM=kwin-wayland.
#
# Usage (DISPLAY_NUM defaults to 187; never the real display):
#   scripts/nested-live.sh start [layout]   # Xvfb (+ WM) + jetty; layout e.g. tr, us
#   scripts/nested-live.sh key ctrl+shift+t ...    # key chords (xdotool names)
#   scripts/nested-live.sh type 'echo hi'          # type text
#   scripts/nested-live.sh click X Y [right]       # pointer click at X,Y
#   scripts/nested-live.sh x key ctrl+shift+t      # X11: raw xdotool, pinned to the display
#   scripts/nested-live.sh raw-key 75              # X11: XTEST keycode (xdotool adds Alt to F9)
#   scripts/nested-live.sh shot out.png [x y w h [scale]]
#   scripts/nested-live.sh state                   # active window, stacking / minimized
#   scripts/nested-live.sh ctl --toggle            # jetty --toggle/--show/--hide, to THIS jetty
#   scripts/nested-live.sh restart                 # relaunch jetty (after a rebuild)
#   scripts/nested-live.sh stop                    # kills only what start launched
#
# Env: DISPLAY_NUM, NESTED_WM=kwin|none|kwin-wayland (default: kwin when installed),
#      NESTED_CONFIG=<config.toml to copy> (default: ~/.config/jetty/config.toml if
#      present — copied, never written), NESTED_BIN=<binary or a wrapper `jetty` script,
#      e.g. one exec-ing a release AppImage> (default target/release/jetty),
#      NESTED_SCALE=<output scale> (kwin-wayland only; default 1).

set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
N="${DISPLAY_NUM:-187}"
SB="$ROOT/target/nested-live-$N"
# The runtime dir holds jetty's IPC socket (`ctl`) — and kwin_wayland's socket —
# and a Unix socket path must fit in sun_path (108 bytes): deep in a worktree,
# `$SB/run/jetty-<hash>.sock` does not, and jetty then runs without IPC. Use a
# short private dir instead.
RUN="$SB/run"
if [ $(( ${#RUN} + 28 )) -gt 100 ]; then
    RUN="${XDG_RUNTIME_DIR:-/tmp}/jetty-nested-$N"
fi
# NOT $JETTY_BIN: inside JeTTY that names the INSTALLED binary (JeTTY sets it
# for its shells), which would silently test the wrong build.
BIN="${NESTED_BIN:-$ROOT/target/release/jetty}"
D=":$N"
WL="$RUN/wm/wayland-jetty"
# The session kind start chose (x11 | wayland), for every later command.
if [ "${1:-}" = start ]; then
    [ "${NESTED_WM:-}" = kwin-wayland ] && MODE=wayland || MODE=x11
else
    MODE=$(cat "$SB/mode" 2>/dev/null || echo x11)
fi

die() { echo "nested-live: $*" >&2; exit 1; }
[ "$N" != "0" ] && [ "$D" != "${DISPLAY:-}" ] || die "refusing to use the real display ($D)"

py() { python3 -I - "$@"; }

# jetty's sandbox environment: the nested display, the sandbox config/cache/
# data/state/runtime dirs — so a `jetty --toggle` run in it (`ctl`) can only
# ever reach the sandbox's own socket — and software Vulkan.
if [ "$MODE" = wayland ]; then
    DISP=(-u DISPLAY WAYLAND_DISPLAY="$WL")
else
    DISP=(-u WAYLAND_DISPLAY DISPLAY="$D")
fi
JENV=(env -u JETTY -u JETTY_BIN -u TERM_PROGRAM -u TERM_PROGRAM_VERSION -u SESSION_MANAGER
    "${DISP[@]}"
    XDG_CONFIG_HOME="$SB/config" XDG_CACHE_HOME="$SB/cache" XDG_DATA_HOME="$SB/data"
    XDG_STATE_HOME="$SB/state" XDG_RUNTIME_DIR="$RUN" JETTY_CONFIG_DIR="$SB/config/jetty"
    PATH="$SB/bin:$PATH" HISTFILE="$SB/zsh_history" GITSTATUS_CACHE_DIR="$HOME/.cache/gitstatus"
    DISABLE_AUTO_UPDATE=true
    VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
    VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json)

# A private session bus with NO service activation, used by jetty and the WM:
# nothing on it can D-Bus-activate a real desktop service (a portal backend, a
# secret daemon …) that would outlive the bus.
write_bus_conf() {
    cat >"$SB/bus.conf" <<'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
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
}

launch_jetty() {
    [ -x "$BIN" ] || die "no binary at $BIN (cargo build --release --bin jetty)"
    [ -f "$SB/bus.conf" ] || write_bus_conf
    ln -sfn "$BIN" "$SB/bin/jetty"
    mkdir -p "$RUN" && chmod 700 "$RUN"
    (cd "$SB" && setsid -f "${JENV[@]}" \
        dbus-run-session --config-file="$SB/bus.conf" -- "$SB/bin/jetty" >>"$SB/jetty.log" 2>&1)
    for _ in $(seq 50); do
        # An AppImage (NESTED_BIN wrapping one) runs jetty as `AppRun`.
        pid=$(for p in $(pgrep -x 'jetty|AppRun'); do
            tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -qx "JETTY_CONFIG_DIR=$SB/config/jetty" && echo "$p"
        done | head -1)
        [ -n "$pid" ] && break
        sleep 0.1
    done
    [ -n "${pid:-}" ] || die "jetty did not start — see $SB/jetty.log"
    echo "$pid" >"$SB/jetty.pid"
    sleep 0.5
    echo "jetty pid $pid: $(readlink "/proc/$pid/exe") — $(grep '^jetty [0-9]' "$SB/jetty.log" | tail -1) (log $SB/jetty.log)"
}

stop_pid() { # only a pid we recorded, and only if it is still the process we started
    local f="$SB/$1.pid" want="$2" p
    [ -f "$f" ] || return 0
    p=$(cat "$f")
    if [ -n "$p" ] && [ -d "/proc/$p" ] && [[ "$(cat /proc/$p/comm)" =~ ^($want)$ ]]; then kill "$p"; fi
    rm -f "$f"
}

# Run a KWin script (JS on stdin) in the nested kwin_wayland, over its private
# bus; prints what the script print()s with a "NL:" prefix.
kwin_js() {
    local bus js="$SB/query.js" name="nl$RANDOM" n id
    bus=$(cat "$SB/wm.bus" 2>/dev/null) || die "no nested kwin_wayland bus"
    cat >"$js"
    n=$(wc -l <"$SB/wm.log")
    id=$(dbus-send --bus="$bus" --dest=org.kde.KWin --print-reply=literal /Scripting \
        org.kde.kwin.Scripting.loadScript string:"$js" string:"$name" | awk '{print $2}')
    dbus-send --bus="$bus" --dest=org.kde.KWin --print-reply /Scripting/Script"$id" org.kde.kwin.Script.run >/dev/null
    sleep 0.3
    dbus-send --bus="$bus" --dest=org.kde.KWin --print-reply /Scripting \
        org.kde.kwin.Scripting.unloadScript string:"$name" >/dev/null
    tail -n +"$((n + 1))" "$SB/wm.log" | grep -a "NL:" | sed 's/.*NL://'
}

# Fake input into the NESTED kwin_wayland (org_kde_kwin_fake_input over a
# minimal Wayland wire client — no pywayland needed). Args: the compositor
# socket, then "key CHORD" / "type TEXT" / "click X Y BUTTON" commands.
wl_input() {
    py "$WL" "$@" <<'EOF'
import socket, struct, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(sys.argv[1])
ids = [2]
def new_id():
    ids[0] += 1; return ids[0] - 1
def send(obj, op, payload=b''):
    s.sendall(struct.pack('<II', obj, ((8 + len(payload)) << 16) | op) + payload)
u32 = lambda v: struct.pack('<I', v)
fixed = lambda v: struct.pack('<i', int(round(v * 256)))
def string(t):
    b = t.encode() + b'\0'; return u32(len(b)) + b + b'\0' * (-len(b) % 4)
buf = bytearray()
def roundtrip(registry=None):
    cb = new_id(); send(1, 0, u32(cb)); found = []
    while True:
        while len(buf) < 8 or len(buf) < (struct.unpack_from('<I', buf, 4)[0] >> 16):
            chunk = s.recv(65536)
            if not chunk: sys.exit('nested-live: compositor closed the connection')
            buf.extend(chunk)
        obj, so = struct.unpack_from('<II', buf); size, op = so >> 16, so & 0xffff
        msg = bytes(buf[8:size]); del buf[:size]
        if obj == 1 and op == 0:
            sys.exit('nested-live: wayland error %r' % msg)
        if obj == registry and op == 0:
            name, ln = struct.unpack_from('<II', msg)
            ver, = struct.unpack_from('<I', msg, 8 + ((ln + 3) & ~3))
            found.append((name, msg[8:8 + ln - 1].decode(), ver))
        if obj == cb and op == 0:
            return found
registry = new_id(); send(1, 1, u32(registry))
fi = [g for g in roundtrip(registry) if g[1] == 'org_kde_kwin_fake_input']
if not fi: sys.exit('nested-live: no org_kde_kwin_fake_input (is this the nested kwin_wayland?)')
name, iface, ver = fi[0]
fake = new_id(); send(registry, 0, u32(name) + string(iface) + u32(min(ver, 4)) + u32(fake))
send(fake, 0, string('nested-live') + string('test input'))  # authenticate
roundtrip()
def step():
    roundtrip(); time.sleep(0.02)
NAMED = {'escape': 1, 'esc': 1, 'backspace': 14, 'tab': 15, 'return': 28, 'enter': 28,
         'ctrl': 29, 'control': 29, 'shift': 42, 'alt': 56, 'space': 57, 'super': 125,
         'up': 103, 'down': 108, 'left': 105, 'right': 106, 'prior': 104, 'page_up': 104,
         'next': 109, 'page_down': 109, 'home': 102, 'end': 107, 'delete': 111,
         'f11': 87, 'f12': 88, **{'f%d' % i: 58 + i for i in range(1, 11)}}
ROWS = [('qwertyuiop', 16), ('asdfghjkl', 30), ('zxcvbnm', 44)]
PLAIN = {'-': 12, '=': 13, '[': 26, ']': 27, ';': 39, "'": 40, '`': 41, '\\': 43,
         ',': 51, '.': 52, '/': 53, ' ': 57, '0': 11, **{str(d): 1 + d for d in range(1, 10)}}
SHIFTED = {'_': '-', '+': '=', '{': '[', '}': ']', ':': ';', '"': "'", '~': '`', '|': '\\',
           '<': ',', '>': '.', '?': '/', '!': '1', '@': '2', '#': '3', '$': '4', '%': '5',
           '^': '6', '&': '7', '*': '8', '(': '9', ')': '0'}
def code(k):
    """evdev keycode + shift for a key name (xdotool-style) or a character (US layout)."""
    if k.lower() in NAMED: return NAMED[k.lower()], False
    if len(k) == 1:
        for row, base in ROWS:
            if k.lower() in row: return base + row.index(k.lower()), k.isupper()
        if k in PLAIN: return PLAIN[k], False
        if k in SHIFTED: return PLAIN[SHIFTED[k]], True
    sys.exit('nested-live: unknown key %r' % k)
args = sys.argv[2:]
while args:
    cmd = args.pop(0)
    if cmd == 'key':
        keys = [code(k)[0] for k in args.pop(0).split('+')]
        for c in keys: send(fake, 10, u32(c) + u32(1)); step()
        for c in reversed(keys): send(fake, 10, u32(c) + u32(0)); step()
    elif cmd == 'type':
        for ch in args.pop(0):
            c, shift = code(ch)
            if shift: send(fake, 10, u32(42) + u32(1))
            send(fake, 10, u32(c) + u32(1)); send(fake, 10, u32(c) + u32(0))
            if shift: send(fake, 10, u32(42) + u32(0))
            step()
    elif cmd == 'click':
        x, y, b = float(args.pop(0)), float(args.pop(0)), args.pop(0)
        send(fake, 9, fixed(x) + fixed(y)); step()
        btn = 0x111 if b == 'right' else 0x110
        send(fake, 2, u32(btn) + u32(1)); step(); send(fake, 2, u32(btn) + u32(0)); step()
EOF
}

# Start the nested headless kwin_wayland (see the header) and wait for its socket.
start_kwin_wayland() {
    rm -f "$SB/wm.bus" "$SB/wm.pid"
    # The wrapper records kwin's pid ($$ survives the exec) and its private bus:
    # kwin_wayland has file capabilities, so its /proc environ is unreadable.
    (cd "$SB/wm" && setsid -f env -u SESSION_MANAGER -u KDE_FULL_SESSION -u KDE_SESSION_UID \
        -u KDE_SESSION_VERSION -u KDE_APPLICATIONS_AS_SCOPE -u QT_IM_MODULE -u XMODIFIERS \
        -u WAYLAND_DISPLAY -u DISPLAY -u XDG_SESSION_TYPE -u XDG_CURRENT_DESKTOP \
        XDG_CONFIG_DIRS=/etc/xdg XDG_CONFIG_HOME="$SB/wm/config" XDG_CACHE_HOME="$SB/wm/cache" \
        XDG_DATA_HOME="$SB/wm/data" XDG_STATE_HOME="$SB/wm/state" XDG_RUNTIME_DIR="$RUN/wm" \
        KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1 \
        QT_LOGGING_RULES="js.debug=true" \
        dbus-run-session --config-file="$SB/bus.conf" -- \
        sh -c 'echo "$DBUS_SESSION_BUS_ADDRESS" >"$1"; echo $$ >"$2"; shift 2; exec kwin_wayland "$@"' \
            sh "$SB/wm.bus" "$SB/wm.pid" --virtual --no-lockscreen --socket "${WL##*/}" \
            --width "${1:-1920}" --height "${2:-1080}" >>"$SB/wm.log" 2>&1)
    for _ in $(seq 100); do [ -S "$WL" ] && [ -s "$SB/wm.bus" ] && break; sleep 0.1; done
    [ -S "$WL" ] || die "kwin_wayland did not start — see $SB/wm.log"
}

case "${1:-}" in
start)
    if [ "$MODE" = wayland ]; then
        [ -S "$WL" ] && die "a nested compositor already runs at $WL (stop it first, or set DISPLAY_NUM)"
    else
        [ -e "/tmp/.X11-unix/X$N" ] && die "display $D is in use (stop it first, or set DISPLAY_NUM)"
    fi
    mkdir -p "$SB"/{config/jetty,cache,data,state,run,bin,wm/config,wm/cache,wm/data,wm/state,wm/run}
    chmod 700 "$SB/run" "$SB/wm/run"
    echo "$MODE" >"$SB/mode"
    cfg="${NESTED_CONFIG:-$HOME/.config/jetty/config.toml}"
    [ -f "$SB/config/jetty/config.toml" ] || { [ -f "$cfg" ] && cp "$cfg" "$SB/config/jetty/config.toml"; }
    write_bus_conf
    if [ "$MODE" = wayland ]; then
        command -v kwin_wayland >/dev/null || die "kwin_wayland is not installed"
        mkdir -p "$RUN/wm" && chmod 700 "$RUN" "$RUN/wm"
        # Each start sets the output scale afresh (KWin would restore the last one).
        rm -f "$SB/wm/config/kwinoutputconfig.json"
        : >"$SB/wm.log"
        start_kwin_wayland
        if [ "${NESTED_SCALE:-1}" != 1 ]; then
            # The virtual backend's --scale only multiplies the mode; the output
            # SCALE is what KWin's own config says. Patch the file it just wrote
            # (1920×1080 logical at NESTED_SCALE) and start it once more.
            for _ in $(seq 50); do [ -s "$SB/wm/config/kwinoutputconfig.json" ] && break; sleep 0.1; done
            stop_pid wm kwin_wayland
            for _ in $(seq 50); do [ -S "$WL" ] || break; sleep 0.1; done
            rm -f "$WL" "$WL.lock"
            py "$SB/wm/config/kwinoutputconfig.json" "$NESTED_SCALE" <<'EOF' || die "could not set the output scale"
import json, sys
path, scale = sys.argv[1], float(sys.argv[2])
cfg = json.load(open(path))
outs = [o for sec in cfg if sec.get('name') == 'outputs' for o in sec['data']]
assert outs, 'no outputs in the KWin config'
for o in outs:
    o['scale'] = scale
    o['mode']['width'], o['mode']['height'] = round(1920 * scale), round(1080 * scale)
json.dump(cfg, open(path, 'w'), indent=4)
EOF
            start_kwin_wayland "$(py <<<"print(round(1920 * $NESTED_SCALE))")" "$(py <<<"print(round(1080 * $NESTED_SCALE))")"
        fi
        launch_jetty
        echo "nested Wayland compositor $WL ready (kwin_wayland). Drive it with: $0 key …"
        exit 0
    fi
    Xvfb "$D" -screen 0 1920x1080x24 -nolisten tcp -noreset >"$SB/xvfb.log" 2>&1 &
    echo $! >"$SB/xvfb.pid"
    for _ in $(seq 50); do [ -S "/tmp/.X11-unix/X$N" ] && break; sleep 0.1; done
    DISPLAY="$D" setxkbmap -layout "${2:-us}"
    wm="${NESTED_WM:-$(command -v kwin_x11 >/dev/null && echo kwin || echo none)}"
    if [ "$wm" = kwin ]; then
        (cd "$SB/wm" && setsid -f env -u SESSION_MANAGER -u KDE_FULL_SESSION -u KDE_SESSION_UID \
            -u KDE_SESSION_VERSION -u KDE_APPLICATIONS_AS_SCOPE -u QT_IM_MODULE -u XMODIFIERS \
            -u WAYLAND_DISPLAY DISPLAY="$D" XDG_CONFIG_HOME="$SB/wm/config" XDG_CACHE_HOME="$SB/wm/cache" \
            XDG_DATA_HOME="$SB/wm/data" XDG_STATE_HOME="$SB/wm/state" XDG_RUNTIME_DIR="$SB/wm/run" \
            KWIN_COMPOSE=N QT_QPA_PLATFORM=xcb \
            dbus-run-session --config-file="$SB/bus.conf" -- kwin_x11 >"$SB/wm.log" 2>&1)
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
    stop_pid jetty 'jetty|AppRun'
    sleep 1
    launch_jetty
    ;;
key | type | click)
    cmd=$1
    shift
    if [ "$MODE" = wayland ]; then
        case "$cmd" in
        key) set -- $(printf 'key %s ' "$@") ;;
        type) set -- type "$*" ;;
        click) set -- click "$1" "$2" "${3:-left}" ;;
        esac
        wl_input "$@"
    else
        case "$cmd" in
        key) exec env DISPLAY="$D" xdotool key "$@" ;;
        type) exec env DISPLAY="$D" xdotool type -- "$*" ;;
        click) exec env DISPLAY="$D" xdotool mousemove "$1" "$2" click "$([ "${3:-}" = right ] && echo 3 || echo 1)" ;;
        esac
    fi
    ;;
x)
    [ "$MODE" = x11 ] || die "x: X11 sessions only (use key/type/click)"
    shift
    exec env DISPLAY="$D" xdotool "$@"
    ;;
ctl)
    # `jetty --toggle` & co. in the sandbox env: the socket is the sandbox's, and
    # only while its jetty runs — without one, `jetty --toggle` would launch a
    # second, untracked jetty instead of toggling.
    shift
    p=$(cat "$SB/jetty.pid" 2>/dev/null)
    [ -n "$p" ] && [ -d "/proc/$p" ] || die "the sandbox jetty is not running"
    exec "${JENV[@]}" "$SB/bin/jetty" "$@"
    ;;
raw-key)
    [ "$MODE" = x11 ] || die "raw-key: X11 sessions only (Wayland has no global hotkey: use ctl --toggle)"
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
    if [ "$MODE" = wayland ]; then
        py "$(cat "$SB/wm.bus")" "${@:2}" <<'EOF'
import os, sys, threading
import dbus
from PIL import Image
bus = dbus.bus.BusConnection(sys.argv[1]); out = sys.argv[2]
shot = dbus.Interface(bus.get_object('org.kde.KWin', '/org/kde/KWin/ScreenShot2'), 'org.kde.KWin.ScreenShot2')
r, w = os.pipe(); chunks = []
def drain():
    while (b := os.read(r, 1 << 20)):
        chunks.append(b)
t = threading.Thread(target=drain); t.start()
res = shot.CaptureWorkspace({'native-resolution': True, 'include-cursor': False}, dbus.types.UnixFd(w))
os.close(w); t.join()
W, H, stride = int(res['width']), int(res['height']), int(res['stride'])
img = Image.frombuffer('RGBA', (W, H), b''.join(chunks), 'raw', 'BGRA', stride, 1).convert('RGB')
if len(sys.argv) >= 7:
    x, y, w, h = (int(v) for v in sys.argv[3:7]); img = img.crop((x, y, x + w, y + h))
if len(sys.argv) >= 8:
    s = float(sys.argv[7]); img = img.resize((int(img.width * s), int(img.height * s)), Image.LANCZOS)
img.save(out); print(out, img.size)
EOF
        exit $?
    fi
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
    if [ "$MODE" = wayland ]; then
        kwin_js <<'EOF'
const act = workspace.activeWindow;
print("NL:active: " + (act ? act.caption : "none"));
for (const w of workspace.stackingOrder) {
    if (!w.normalWindow && !w.dialog) continue;
    const g = w.frameGeometry;
    print("NL:   " + (w.minimized ? "minimized" : "shown") + (w.fullScreen ? " fullscreen" : "")
        + " " + g.x + "," + g.y + " " + g.width + "x" + g.height + " " + w.caption);
}
EOF
        exit 0
    fi
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
    stop_pid jetty 'jetty|AppRun'
    if [ "$MODE" = wayland ]; then stop_pid wm kwin_wayland; else stop_pid wm kwin_x11; fi
    stop_pid xvfb Xvfb
    # The runtime dirs outside the sandbox: jetty's socket + lock and its shells'
    # integration snippets, the compositor's, and the dconf cache a program in
    # the session may have left.
    sleep 0.3
    rm -f "$RUN"/jetty*.sock "$RUN"/jetty*.sock.lock "$WL" "$WL.lock" "$RUN/dconf/user" "$RUN/wm/dconf/user"
    rm -rf "$RUN"/jetty-shell-*
    rmdir "$RUN/dconf" "$RUN/wm/dconf" 2>/dev/null
    rmdir "$RUN/wm" 2>/dev/null
    [ "$RUN" != "$SB/run" ] && rmdir "$RUN" 2>/dev/null
    rm -f "$SB/mode"
    echo "stopped (sandbox kept at $SB)"
    ;;
*)
    sed -n '2,41p' "$0"
    exit 1
    ;;
esac
