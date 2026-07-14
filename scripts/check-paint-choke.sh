#!/usr/bin/env bash
#
# check-paint-choke.sh — CI guard rail for the v0.23 central paint chokepoint.
#
# Asserts that NO raw `.request_redraw()` call survives in the MIGRATEABLE
# A/B/C/E producer sites (input / PTY output / resize / overlay+chrome / lifecycle
# paints). Every such paint MUST route through one of the auditable per-surface
# chokes instead:
#
#     App::request_main_paint(&self)        -> main window
#     App::request_settings_paint(&self)    -> settings window
#     DetachedWindow::request_paint(&self)  -> a detached window
#     App::mark_dirty_all(&self)            -> fan-out over all of the above
#
# SCOPE / HONESTY (amendment BLOCKING 4): this grep covers ONLY the migrateable
# producer sites. It does NOT — and CANNOT — make the category-D animation
# invariant un-regressable: the animation/lifecycle Poll-drive is intentionally
# raw `request_redraw` and is EXPLICITLY WHITELISTED below. Routing that drive
# through any wrapper risks a dropped frame across the macOS Wait/Poll seam.
#
# EXPLICIT WHITELIST — the only raw `.request_redraw()` calls allowed to remain
# (each verified by CONTEXT, not by line number, so the check survives edits):
#
#   app.rs
#     1. fn request_main_paint     — the main choke DEFINITION
#     2. fn request_settings_paint — the settings choke DEFINITION
#     3. fn about_to_wait          — ALL animation/lifecycle self-drive sites
#          (main_pending / detached_pending / settings_pending Poll re-requests,
#           reflow/deadline services). Must stay raw: the macOS Poll/Wait seam.
#     4. main render-tail self-drive — the block guarded by
#          `summon_anim || slide_anim || hint_live || crt_anim_live || caret_anim`
#     5. dock re-assert  — guarded by `pending_dock_frames > 0`
#     6. center re-assert — guarded by `pending_center_frames > 0`
#     7. main-window-open first-frame nudge — a bare local `window` binding
#   detached.rs
#     8. fn request_paint          — the detached choke DEFINITION
#     9. detached render-tail self-drive — guarded by
#          `!occluded && (caret_anim || crt_anim_live || shift_hint_show)`
#    10. DetachedWindow::new first-frame nudge — a bare local `window` binding
#
# Exit 0 = clean. Exit 1 = a raw producer redraw leaked in (prints the sites).

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
import re, sys, os
root = sys.argv[1]
app = os.path.join(root, "crates/jetty-app/src/app.rs")
det = os.path.join(root, "crates/jetty-app/src/detached.rs")

def fn_range(lines, sig):
    start = next(i for i,l in enumerate(lines) if l.startswith(sig))
    end = next((i for i in range(start+1, len(lines)) if lines[i].startswith("    fn ")), len(lines))
    return start, end

CALL = re.compile(r"\.request_redraw\(\s*\)")
offenders = []

# ---------- app.rs ----------
lines = open(app).read().split("\n")
main_s, main_e   = fn_range(lines, "    fn request_main_paint(")
set_s,  set_e    = fn_range(lines, "    fn request_settings_paint(")
atw_s,  atw_e    = fn_range(lines, "    fn about_to_wait(")

def allowed_app(i):
    l = lines[i]; s = l.strip()
    prev = lines[i-1].strip() if i>0 else ""
    # whitelist 1/2: choke definitions
    if main_s <= i < main_e or set_s <= i < set_e:
        return True
    # whitelist 3: entire about_to_wait
    if atw_s <= i < atw_e:
        return True
    # whitelist 7 / 10-style: bare local `window` binding (window-open nudge)
    if re.match(r"^window\.request_redraw\(\);$", s):
        return True
    # whitelist 5/6: dock / center re-assert
    if "pending_dock_frames > 0" in prev or "pending_center_frames > 0" in prev:
        return True
    # whitelist 4: main render-tail self-drive
    if prev == "if let Some(w) = &self.window {":
        pred = "\n".join(lines[max(0,i-12):i])
        if ("self.summon_anim.is_some()" in pred and
            "self.caret_anim.is_some()" in pred and "crt_anim_live" in pred):
            return True
    # whitelist 9: detached render-tail self-drive (render_detached_window lives
    # in app.rs); guarded by `!occluded && (caret_anim || crt_anim_live || shift_hint_show)`
    if "shift_hint_show" in prev:
        return True
    return False

for i,l in enumerate(lines):
    if not CALL.search(l): continue
    s = l.strip()
    if s.startswith("//") or s.startswith("///") or s.startswith("*"): continue  # comment mention
    if not allowed_app(i):
        offenders.append(f"app.rs:{i+1}: {s}")

# ---------- detached.rs ----------
dlines = open(det).read().split("\n")
rp_s, rp_e = fn_range(dlines, "    pub(crate) fn request_paint(")

def allowed_det(i):
    l = dlines[i]; s = l.strip()
    prev = dlines[i-1].strip() if i>0 else ""
    # whitelist 8: choke definition
    if rp_s <= i < rp_e:
        return True
    # whitelist 10: bare local `window` nudge in the constructor
    if re.match(r"^window\.request_redraw\(\);$", s):
        return True
    # whitelist 9: detached render-tail self-drive
    if "shift_hint_show" in prev:
        return True
    return False

for i,l in enumerate(dlines):
    if not CALL.search(l): continue
    s = l.strip()
    if s.startswith("//") or s.startswith("///") or s.startswith("*"): continue
    if not allowed_det(i):
        offenders.append(f"detached.rs:{i+1}: {s}")

if offenders:
    print("FAIL: raw .request_redraw() in a migrateable producer site — route it")
    print("      through a per-surface paint choke (request_main_paint /")
    print("      request_settings_paint / dw.request_paint / mark_dirty_all).")
    print("      If it is genuinely an animation/lifecycle self-drive, add it to")
    print("      the explicit whitelist in scripts/check-paint-choke.sh.\n")
    for o in offenders:
        print("  " + o)
    sys.exit(1)

print("OK: no raw request_redraw in migrateable producer sites (paint choke intact).")
PY
