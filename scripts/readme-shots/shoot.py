#!/usr/bin/env python3
"""Regenerate the README screenshots in assets/screenshots/.

Every image is a real frame from the GPU renderer: `jetty-shot` (the headless
renderer in crates/jetty-app/src/bin/jetty-shot.rs) draws a scripted session
(content.py) with default settings, then this script rounds the corners like
the window's own mask, adds name badges to sheets, and writes 256-color PNGs
(the summon animation is a WebP).

    cargo build --release --bin jetty-shot
    python3 scripts/readme-shots/shoot.py            # every job
    python3 scripts/readme-shots/shoot.py looks tabs # some of them

Jobs: hero, looks, themes, backdrops, glyphs, tabs, settings, summon.
Needs Python 3 with Pillow (libimagequant + WebP support) and the default font,
MesloLGS NF. Renders at 2x so the images stay sharp on HiDPI screens.
"""
import math
import os
import re
import subprocess
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import content  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(HERE))
SHOT = os.path.join(ROOT, "target/release/jetty-shot")
WORK = os.path.join(ROOT, "target/readme-shots")
RAW, CFG = os.path.join(WORK, "raw"), os.path.join(WORK, "cfg")  # cfg: no config.toml → defaults
OUT = os.path.join(ROOT, "assets/screenshots")
for d in (RAW, CFG, OUT):
    os.makedirs(d, exist_ok=True)

PERF_IDLE = "⚡ idle · 0% CPU · 0 MB/s"  # the HUD at rest (app.rs PERF_IDLE_TEXT)
S2 = {"JETTY_SHOT_SCALE": "2"}
TABS = {"JETTY_SHOT_TABBAR": "1", "JETTY_SHOT_TABBAR_N": "3", "JETTY_SHOT_TAB_TITLES": ",nvim,cargo watch",
        "JETTY_SHOT_TABBAR_ACTIVITY": "none,done,output", "JETTY_SHOT_TAB_PROGRESS": "-,-,60"}

# The one-click Looks (settings_ui.rs LOOKS): theme, effects preset over the
# defaults, backdrop, cursor.
LOOKS = {
    "neon-night": {"JETTY_THEME": "synthwave_84", "JETTY_SHOT_PRESET": "neon",
                   "JETTY_SHOT_BACKDROP": "synthwave", "JETTY_SHOT_BACKDROP_STRENGTH": "0.5"},
    "aurora": {"JETTY_THEME": "tokyo_night_storm", "JETTY_SHOT_PRESET": "clean",
               "JETTY_SHOT_BACKDROP": "aurora", "JETTY_SHOT_BACKDROP_STRENGTH": "0.5"},
    "trinitron": {"JETTY_THEME": "tokyo_night", "JETTY_SHOT_PRESET": "retro_crt", "JETTY_SHOT_BACKDROP": "theme"},
    "amber-vt": {"JETTY_THEME": "phosphor_amber", "JETTY_SHOT_PRESET": "amber", "JETTY_SHOT_CURSOR": "shape=block"},
    "p1-green": {"JETTY_THEME": "phosphor_green", "JETTY_SHOT_PRESET": "green_phosphor",
                 "JETTY_SHOT_CURSOR": "shape=block"},
    "paper": {"JETTY_THEME": "flexoki_light", "JETTY_SHOT_PRESET": "paper"},
}

THEMES = [
    ("catppuccin_mocha", "Catppuccin Mocha"), ("tokyo_night", "Tokyo Night"), ("dracula", "Dracula"),
    ("gruvbox_dark", "Gruvbox Dark"), ("nord", "Nord"), ("rose_pine", "Rosé Pine"), ("kanagawa", "Kanagawa"),
    ("one_dark", "One Dark"), ("everforest_dark", "Everforest Dark"), ("onyx", "Onyx"),
    ("night_owl", "Night Owl"), ("poimandres", "Poimandres"), ("catppuccin_latte", "Catppuccin Latte"),
    ("github_light", "GitHub Light"), ("solarized_light", "Solarized Light"), ("rose_pine_dawn", "Rosé Pine Dawn"),
]


# --- rendering ---------------------------------------------------------------

def run(env_extra, out, inp):
    env = {k: v for k, v in os.environ.items() if not k.startswith("JETTY")}
    env.update({"JETTY_CONFIG_DIR": CFG, "JETTY_SHOT_OUT": out, "JETTY_SHOT_INPUT": inp}, **env_extra)
    r = subprocess.run([SHOT], env=env, capture_output=True, text=True, timeout=180)
    if r.returncode != 0:
        sys.exit(f"jetty-shot failed for {out}:\n{r.stderr[-2000:]}")
    return r.stderr


def render(name, env, session=lambda cols, rows: ""):
    """Probe the grid size for `env`, then render `session(cols, rows)`."""
    m = re.search(r"grid = (\d+)x(\d+) cells", run(env, os.path.join(RAW, "_probe.png"), ""))
    cols, rows = (int(m.group(1)), int(m.group(2))) if m else (0, 0)
    out = os.path.join(RAW, f"{name}.png")
    run(env, out, session(cols, rows))
    return out


# --- post-processing ---------------------------------------------------------

def rounded(im, radius, ss=4):
    """The window's shape: corners cut at the corner radius (10 px at 1x)."""
    w, h = im.size
    mask = Image.new("L", (w * ss, h * ss), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, w * ss - 1, h * ss - 1), radius * ss, fill=255)
    im = im.convert("RGBA")
    im.putalpha(ImageChops.multiply(im.getchannel("A"), mask.resize((w, h), Image.LANCZOS)))
    return im


def save(im, name):
    path = os.path.join(OUT, name)
    im.quantize(colors=256, method=Image.Quantize.LIBIMAGEQUANT, dither=Image.Dither.FLOYDSTEINBERG).save(
        path, optimize=True)
    print(f"  {name}: {os.path.getsize(path) // 1024} KiB {im.size[0]}x{im.size[1]}")


def window(src, width=None, radius=20):
    im = Image.open(src).convert("RGBA")
    if width and im.width != width:
        radius = radius * width / im.width
        im = im.resize((width, round(im.height * width / im.width)), Image.LANCZOS)
    return rounded(im, radius)


def font(size):
    for f in ("/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
              "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"):
        if os.path.exists(f):
            return ImageFont.truetype(f, size)
    return ImageFont.load_default(size)


def badge(im, text, size=30, pad=14, corner="br"):
    """A name pill: dark glass and white text, readable on any theme and on
    GitHub's light and dark pages alike."""
    d = ImageDraw.Draw(im, "RGBA")
    f = font(size)
    l, t, r, b = d.textbbox((0, 0), text, font=f)
    w, h = r - l + 2 * pad, b - t + pad
    x = im.width - w - 14 if corner.endswith("r") else 14
    y = im.height - h - 14 if corner.startswith("b") else 14
    d.rounded_rectangle((x, y, x + w, y + h), h // 2, fill=(12, 12, 16, 200))
    d.text((x + pad - l, y + pad // 2 - t), text, font=f, fill=(255, 255, 255, 255))
    return im


def sheet(tiles, cols, tile_w, gap=18):
    """Tiles on a transparent sheet."""
    tiles = [t.resize((tile_w, round(t.height * tile_w / t.width)), Image.LANCZOS) for t in tiles]
    th = max(t.height for t in tiles)
    rows = -(-len(tiles) // cols)
    out = Image.new("RGBA", (cols * tile_w + (cols - 1) * gap, rows * th + (rows - 1) * gap), (0, 0, 0, 0))
    for i, t in enumerate(tiles):
        out.alpha_composite(t, ((i % cols) * (tile_w + gap), (i // cols) * (th + gap)))
    return out


# --- jobs --------------------------------------------------------------------

def job_hero():
    env = {**S2, **TABS, "JETTY_THEME": "catppuccin_mocha", "JETTY_SHOT_WIDTH": "1800",
           "JETTY_SHOT_HEIGHT": "1150", "JETTY_SHOT_PERF": PERF_IDLE}
    save(window(render("hero", env, content.hero_session)), "hero.png")


def job_looks():
    for key, look in LOOKS.items():
        env = {**S2, **TABS, "JETTY_SHOT_WIDTH": "1520", "JETTY_SHOT_HEIGHT": "900", **look}
        save(window(render(f"look-{key}", env, content.look_session), width=1000), f"look-{key}.png")


def job_themes():
    tiles = []
    for tid, label in THEMES:
        env = {**S2, **TABS, "JETTY_THEME": tid, "JETTY_SHOT_WIDTH": "1120", "JETTY_SHOT_HEIGHT": "640"}
        tiles.append(badge(window(render(f"theme-{tid}", env, content.theme_session)), label))
    save(sheet(tiles, 4, 560), "themes.png")


def sunset():
    """A stand-in wallpaper photo for the image backdrop: sky, sun, hills."""
    path = os.path.join(WORK, "sunset.png")
    w, h = 2400, 1500
    im = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(im)
    for y in range(h):
        t = y / (h - 1)
        if t < 0.62:
            k = t / 0.62
            c = (int(40 + 215 * k ** 1.4), int(20 + 110 * k ** 2.2), int(90 - 40 * k))
        else:
            k = (t - 0.62) / 0.38
            c = (int(60 - 45 * k), int(30 - 20 * k), int(50 - 30 * k))
        d.line([(0, y), (w, y)], fill=c)
    d.ellipse((w * 0.5 - 220, h * 0.62 - 300, w * 0.5 + 220, h * 0.62 + 140), fill=(255, 210, 120))
    for layer, (base, amp, col) in enumerate([(0.66, 120, (70, 30, 70)), (0.74, 90, (45, 20, 50)),
                                              (0.84, 60, (25, 12, 30))]):
        pts = [(0, h)] + [(x, h * base - amp * (0.6 * math.sin(x / 210.0 + layer) + 0.4 * math.sin(x / 77.0 + 2 * layer)))
                          for x in range(0, w + 40, 40)] + [(w, h)]
        d.polygon(pts, fill=col)
    im.save(path)
    return path


def job_backdrops():
    sets = [
        ("Stars", {"JETTY_SHOT_BACKDROP": "stars"}),
        ("Grid", {"JETTY_SHOT_BACKDROP": "grid"}),
        ("Gradient", {"JETTY_SHOT_BACKDROP": "gradient", "JETTY_SHOT_BACKDROP_COLORS": "#2b1055,#7597de",
                      "JETTY_SHOT_BACKDROP_ANGLE": "160", "JETTY_SHOT_BACKDROP_STRENGTH": "0.8",
                      "JETTY_SHOT_BACKDROP_VIGNETTE": "0.5"}),
        ("Frosted image", {"JETTY_SHOT_BACKDROP": "image", "JETTY_SHOT_BACKDROP_IMAGE": sunset(),
                           "JETTY_SHOT_BACKDROP_BLUR": "0.35"}),
    ]
    tiles = []
    for label, bd in sets:
        env = {**S2, **TABS, "JETTY_THEME": "tokyo_night", "JETTY_SHOT_WIDTH": "1120", "JETTY_SHOT_HEIGHT": "640", **bd}
        tiles.append(badge(window(render(f"backdrop-{label.split()[0].lower()}", env, content.theme_session)), label))
    save(sheet(tiles, 2, 800), "backdrops.png")


def job_glyphs():
    env = {**S2, "JETTY_THEME": "catppuccin_mocha", "JETTY_SHOT_WIDTH": "1800", "JETTY_SHOT_HEIGHT": "500"}
    save(window(render("glyphs", env, content.glyph_session)), "glyphs.png")


def job_tabs():
    strips = []
    for style in ("pill", "underline", "slant", "powerline", "compact"):
        env = {**S2, "JETTY_THEME": "catppuccin_mocha", "JETTY_SHOT_WIDTH": "1800", "JETTY_SHOT_HEIGHT": "160",
               "JETTY_SHOT_TABBAR": "1", "JETTY_SHOT_TABBAR_N": "5", "JETTY_SHOT_TAB_STYLE": style,
               "JETTY_SHOT_TAB_TITLES": ",nvim,cargo watch,ssh prod,htop",
               "JETTY_SHOT_TABBAR_ACTIVITY": "none,done,output,failed,bell",
               "JETTY_SHOT_TAB_PROGRESS": "-,-,60,-,-", "JETTY_SHOT_TAB_COLORS": "-,-,-,1,-"}
        im = Image.open(render(f"tabs-{style}", env, lambda c, r: content.TITLE + content.HIDE_CURSOR))
        strips.append((style, rounded(im.convert("RGBA").crop((0, 0, im.width, 76)), 16)))
    gap, gutter = 14, 230
    sw, sh = strips[0][1].size
    out = Image.new("RGBA", (gutter + sw, len(strips) * sh + gap * (len(strips) - 1)), (0, 0, 0, 0))
    for i, (style, strip) in enumerate(strips):
        y = i * (sh + gap)
        out.alpha_composite(strip, (gutter, y))
        tag = badge(Image.new("RGBA", (gutter - 16, sh), (0, 0, 0, 0)), style, size=28, corner="tr")
        bb = tag.getbbox()
        out.alpha_composite(tag, (0, y + (sh - (bb[3] - bb[1])) // 2 - bb[1]))
    save(out, "tabs.png")


def job_settings():
    common = {**S2, "JETTY_SHOT_PANEL": "1", "JETTY_THEME": "catppuccin_mocha", "JETTY_SHOT_HEIGHT": "1500"}
    look = {**common, "JETTY_SHOT_PANEL_TAB": "0",
            "JETTY_SHOT_PANEL_COLLAPSE": "look.window,look.backdrop,look.chrome,look.appearance"}
    save(window(render("settings-look", look)), "settings.png")
    fx = {**common, "JETTY_SHOT_PANEL_TAB": "4", "JETTY_SHOT_PANEL_PRESET": "retro_crt"}
    save(window(render("settings-effects", fx)), "settings-effects.png")


def job_summon():
    """Phosphor Ignition (0.25 s) over a stand-in desktop, at 1/3 speed."""
    W, H, WW, WH, fps, slow, secs = 1800, 1080, 1440, 830, 30, 3.0, 0.25
    bg = Image.new("RGB", (W, H))
    d = ImageDraw.Draw(bg)
    for y in range(H):
        t = y / (H - 1)
        d.line([(0, y), (W, y)], fill=(int(18 + 10 * t), int(20 + 6 * t), int(38 + 22 * t)))
    blobs = Image.new("RGB", (W, H))
    b = ImageDraw.Draw(blobs)
    for cx, cy, r, col in [(290, 240, 400, (90, 60, 160)), (1550, 320, 460, (40, 90, 170)),
                           (1000, 980, 520, (150, 60, 120)), (130, 980, 330, (30, 110, 120))]:
        b.ellipse((cx - r, cy - r, cx + r, cy + r), fill=col)
    blobs = ImageChops.multiply(blobs.filter(ImageFilter.GaussianBlur(160)), Image.new("RGB", (W, H), (150,) * 3))
    bg = ImageChops.add(bg, blobs)

    def frame(t):
        env = {**S2, **TABS, "JETTY_THEME": "catppuccin_mocha", "JETTY_SHOT_WIDTH": str(WW),
               "JETTY_SHOT_HEIGHT": str(WH), "JETTY_SHOT_UNDERLAY": "none", "JETTY_CORNER_RADIUS": "20"}
        if t is not None:
            env["JETTY_SHOT_PHOSPHOR_T"] = f"{t:.4f}"
        win = Image.open(render("summon", env, content.look_session)).convert("RGBA")
        # Centered, with a compositor-style soft shadow that fades in with it.
        x, y = (W - WW) // 2, (H - WH) // 2 - 10
        shadow = Image.new("L", (W, H), 0)
        shadow.paste(win.getchannel("A"), (x, y + 22))
        shadow = shadow.filter(ImageFilter.GaussianBlur(28)).point(lambda v: int(v * 0.6))
        canvas = Image.composite(Image.new("RGBA", (W, H), (0, 0, 0, 255)), bg.convert("RGBA"), shadow)
        canvas.alpha_composite(win, (x, y))
        return canvas.convert("RGB")

    n = round(secs * slow * fps)
    frames = [bg] + [frame(i / n) for i in range(1, n + 1)] + [frame(None)]
    durations = [800] + [round(1000 / fps)] * n + [2600]
    frames = [f.resize((1000, 600), Image.LANCZOS) for f in frames]
    path = os.path.join(OUT, "summon.webp")
    frames[0].save(path, save_all=True, append_images=frames[1:], duration=durations, loop=0, quality=82, method=6)
    print(f"  summon.webp: {os.path.getsize(path) // 1024} KiB, {len(frames)} frames")


JOBS = {k[4:]: v for k, v in dict(globals()).items() if k.startswith("job_")}

if __name__ == "__main__":
    if not os.access(SHOT, os.X_OK):
        sys.exit("build the renderer first: cargo build --release --bin jetty-shot")
    for job in sys.argv[1:] or list(JOBS):
        print(job)
        JOBS[job]()
