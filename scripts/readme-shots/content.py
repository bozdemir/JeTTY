"""Scripted terminal sessions (ANSI bytes) for the README screenshots.

Every color is one of the theme's 16 palette colors (or reverse video), so the
same session re-skins with each theme, the way a real shell's output does.
The user and host are neutral (`dev@jetty`, `~/src/jetty`).
"""

import math
import os
import re

E = "\x1b"


def sgr(*codes):
    return f"{E}[{';'.join(str(c) for c in codes)}m"


R = sgr(0)
DIM = sgr(90)
HIDE_CURSOR = f"{E}[?25l"
TITLE = f"{E}]2;~/src/jetty\x07"

# Nerd Font icons (the default font, MesloLGS NF, has them) and the Powerline
# separators JeTTY draws itself.
FOLDER, FOLDER_OPEN = "", ""
BRANCH = ""
SEP, RSEP = "", ""
CHECK, CROSS = "", "✘"
CLOCK, HOURGLASS = "", ""
CHEV = "❯"


def prompt(cols, path="~/src/jetty", branch="main", took=None, clock="09:41", ok=True):
    """A powerlevel10k-style two-line rainbow prompt → (line 1, line 2's prefix).
    Segments are palette colors in reverse video (the theme's background as
    the text), so they recolor with every theme."""
    left_plain = f" {FOLDER_OPEN} {path} {SEP} {BRANCH} {branch} {SEP}"
    left = (sgr(34, 7) + f" {FOLDER_OPEN} {path} " + R + sgr(34, 42) + SEP + R
            + sgr(32, 7) + f" {BRANCH} {branch} " + R + sgr(32) + SEP + R)
    status_col = 32 if ok else 31
    status = f" {CHECK} " if ok else f" {CROSS} 1 "
    right = sgr(status_col) + RSEP + R + sgr(status_col, 7) + status + R
    right_plain = RSEP + status
    prev = status_col
    if took:
        right += sgr(33, prev + 10) + RSEP + R + sgr(33, 7) + f" {HOURGLASS} {took} " + R
        right_plain += RSEP + f" {HOURGLASS} {took} "
        prev = 33
    right += sgr(36, prev + 10) + RSEP + R + sgr(36, 7) + f" {CLOCK} {clock} " + R
    right_plain += RSEP + f" {CLOCK} {clock} "
    # "╭─" + left + " " + rule + " " + right + one spare column (p10k's indent)
    n = cols - 2 - len(left_plain) - 2 - len(right_plain) - 1
    line1 = DIM + "╭─" + R + left + " " + DIM + "─" * max(n, 1) + R + " " + right
    line2 = DIM + "╰─" + R + sgr(1, 32 if ok else 31) + CHEV + R + " "
    return line1, line2


def block(cols, cmd, out_lines, **kw):
    """A prompt, the command typed at it, and its output."""
    l1, l2 = prompt(cols, **kw)
    return [l1, l2 + cmd] + out_lines


def eza(cols):
    """`eza --icons` in its grid layout: as many columns as fit the width."""
    items = [
        (sgr(1, 34), FOLDER, "assets"), (sgr(1, 34), FOLDER, "crates"), (sgr(1, 34), FOLDER, "docs"),
        (sgr(1, 34), FOLDER, "scripts"), ("", "", "Cargo.lock"), (sgr(33), "", "Cargo.toml"),
        ("", "", "CHANGELOG.md"), (sgr(1, 32), "", "install.sh"), ("", "", "LICENSE"),
        ("", "", "README.md"),
    ]
    cells = [(f"{c}{i} {n}{R}", 2 + len(n)) for c, i, n in items]
    for per_row in range(len(cells), 0, -1):
        nrows = -(-len(cells) // per_row)
        # column-major, like eza: item k sits in column k // nrows
        colw = [max(w for _, w in cells[c * nrows:(c + 1) * nrows]) for c in range(-(-len(cells) // nrows))]
        if sum(colw) + 2 * (len(colw) - 1) <= cols - 1:
            break
    lines = []
    for r in range(nrows):
        parts = []
        for c in range(len(colw)):
            k = c * nrows + r
            if k < len(cells):
                s, w = cells[k]
                last = c == len(colw) - 1 or (c + 1) * nrows + r >= len(cells)
                parts.append(s + ("" if last else " " * (colw[c] - w + 2)))
        lines.append("".join(parts))
    return lines


def git_log():
    y = sgr(33)
    return [
        f"* {y}e910ef2{R} {y}({R}{sgr(1, 36)}HEAD -> {R}{sgr(1, 32)}main{R}{y},{R} {sgr(1, 31)}origin/main{R}{y}){R}"
        " docs: README for visuals v2",
        f"* {y}9908f54{R} feat(settings): one-click Looks",
        f"* {y}1a7be72{R} fix(motion): reduce motion meets the backdrop",
    ]


def bat(cols, path="crates/jetty-app/src/motion.rs"):
    """`bat` with its default grid: a header and line numbers."""
    kw, fn, ty, num, com, me = sgr(35), sgr(34), sgr(33), sgr(36), sgr(3, 90), sgr(31)
    code = [
        f"{com}/// Summon: slide in from the top edge and ease out.{R}",
        f"{kw}pub fn{R} {fn}summon{R}(&{kw}mut{R} {me}self{R}, now: {ty}Instant{R}) -> {ty}bool{R} {{",
        f"    {kw}let{R} t = (now - {me}self{R}.started).{fn}as_secs_f32{R}() / {num}0.18{R};",
        f"    {me}self{R}.offset = {fn}ease_out_cubic{R}(t.{fn}min{R}({num}1.0{R})) * {me}self{R}.height;",
        f"    t < {num}1.0{R} {com}// keep animating{R}",
        "}",
    ]
    rule = "─" * (cols - 8)
    out = [
        DIM + "─" * 7 + "┬" + rule + R,
        DIM + " " * 7 + "│ " + R + f"File: {sgr(1)}{path}{R}",
        DIM + "─" * 7 + "┼" + rule + R,
    ]
    out += [DIM + f"{41 + i:>5}  │ " + R + line for i, line in enumerate(code)]
    out.append(DIM + "─" * 7 + "┴" + rule + R)
    return out


def jetty_version():
    """The version in the repo's Cargo.toml, so the sample build log stays current."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "Cargo.toml")
    with open(path) as f:
        return re.search(r'^version = "([^"]+)"', f.read(), re.M).group(1)


def cargo_build():
    gb, v = sgr(1, 32), jetty_version()
    return [
        f"{gb}   Compiling{R} jetty-app v{v} (~/src/jetty/crates/jetty-app)",
        f"{gb}   Compiling{R} jetty v{v} (~/src/jetty)",
        f"{gb}    Finished{R} `release` profile [optimized] target(s) in 41.87s",
    ]


def snippet():
    kw, fn, ty, num, com = sgr(35), sgr(34), sgr(33), sgr(36), sgr(3, 90)
    return [
        f"{com}/// Ease the summon in.{R}",
        f"{kw}fn{R} {fn}summon{R}(&{kw}mut{R} {sgr(31)}self{R}, t: {ty}f32{R}) -> {ty}f32{R} {{",
        f"    {kw}let{R} k = (t / {num}0.18{R}).{fn}min{R}({num}1.0{R});",
        f"    {num}1.0{R} - ({num}1.0{R} - k).{fn}powi{R}({num}3{R}) {com}// ease out{R}",
        "}",
    ]


def swatches():
    """The 16-color palette: normal over bright."""
    return ["".join(f"{sgr(base + i)}███{R} " for i in range(8)) for base in (30, 90)]


def hero_session(cols, rows):
    """eza, bat, cargo build, an idle prompt."""
    lines = block(cols, "eza --icons", eza(cols))
    lines += block(cols, "bat crates/jetty-app/src/motion.rs", bat(cols))
    lines += block(cols, "cargo build --release", cargo_build(), clock="09:42")
    lines += list(prompt(cols, took="42s", clock="09:42"))
    return TITLE + "\r\n".join(lines[-rows:])


def look_session(cols, rows):
    """eza, git log, cargo build, an idle prompt."""
    lines = block(cols, "eza --icons", eza(cols))
    lines += block(cols, "git log --oneline --graph -3", git_log())
    lines += block(cols, "cargo build --release", cargo_build(), clock="09:42")
    lines += list(prompt(cols, took="42s", clock="09:42"))
    return TITLE + "\r\n".join(lines[-rows:])


def theme_session(cols, rows):
    """Code in syntax colors, the 16-color palette, an idle prompt."""
    lines = block(cols, "bat -p motion.rs", snippet())
    lines += block(cols, "colortest", swatches())
    lines += list(prompt(cols, clock="09:42"))
    return TITLE + "\r\n".join(lines[-rows:])


def glyph_session(cols, rows):
    """btop / tmux-style chrome: box drawing, a braille graph, block bars,
    shades, sextants, Powerline, emoji and every underline style — the glyphs
    JeTTY draws itself, cell-exact."""
    g, c, gr, y, m, r = DIM, sgr(36), sgr(32), sgr(33), sgr(35), sgr(31)
    lw = cols * 3 // 5
    rw = cols - lw
    inner_l, inner_r = lw - 2, rw - 2

    def top(w, name, col):
        return g + "╭─ " + col + name + R + g + " " + "─" * (w - 5 - len(name)) + "╮" + R

    def bottom(w):
        return g + "╰" + "─" * (w - 2) + "╯" + R

    # A braille area graph, 4 rows tall, 2 samples per cell.
    samples = [min(0.97, max(0.04, 0.42 + 0.26 * math.sin(i / 9.0) + 0.14 * math.sin(i / 3.3 + 1.0)
                             + 0.08 * math.sin(i / 1.7 + 2.0))) for i in range(inner_l * 2)]
    graph = []
    for row in range(4):
        cells = []
        for cx in range(inner_l - 2):
            bits = 0
            for sub in range(2):
                filled = int(samples[cx * 2 + sub] * 16)
                for dy in range(4):  # dy 0 = the cell's top dot
                    if (3 - row) * 4 + (3 - dy) < filled:
                        bits |= (0x01, 0x02, 0x04, 0x40)[dy] if sub == 0 else (0x08, 0x10, 0x20, 0x80)[dy]
            cells.append(chr(0x2800 + bits))
        graph.append((r, y, gr, gr)[row] + "".join(cells) + R)

    bw = inner_r - 13

    def bar(label, frac, col):
        whole = int(frac * bw)
        part = " ▏▎▍▌▋▊▉"[int((frac * bw - whole) * 8)]
        s = "█" * whole + (part if whole < bw else "")
        return f"{label:<6}{col}{s}{R}{' ' * (bw - len(s))} {int(frac * 100):>3}%"

    mem = [bar("used", 0.61, m), bar("cache", 0.34, c), bar("swap", 0.08, y), bar("disk", 0.77, gr)]
    lines = [top(lw, "cpu", c) + top(rw, "mem", m)]
    for i in range(4):
        lines.append(g + "│" + R + " " + graph[i] + " " + g + "│" + R
                     + g + "│" + R + " " + mem[i] + " " + g + "│" + R)
    lines.append(bottom(lw) + bottom(rw))
    ramp = sgr(34) + "▁▂▃▄▅▆▇█▇▆▅▄▃▂▁" + R
    shades = sgr(36) + "░░▒▒▓▓██" + R
    sextants = sgr(35) + "".join(chr(0x1FB00 + i) for i in range(0, 40, 3)) + R
    lines += [
        "",
        f" {ramp}  {shades}  {sextants}  {sgr(1)}✔ bold{R} {sgr(3)}italic{R} {sgr(2)}faint{R}",
        f" {sgr(58, 5, 9)}{E}[4:3mundercurl{R}  {E}[4:2mdouble{R}  {E}[4:4mdotted{R}  {E}[4:5mdashed{R}"
        f"  {sgr(9)}strike{R}   ✨ \U0001F680 ✅ \U0001F389 \U0001F980",
        "",
    ]
    # A tmux status line with Powerline separators.
    left = (sgr(30, 42) + " 0 zsh " + R + sgr(32, 44) + SEP + R + sgr(30, 44) + " 1 nvim " + R
            + sgr(34, 45) + SEP + R + sgr(30, 45) + " 2 cargo " + R + sgr(35) + SEP + R)
    right = (sgr(36) + RSEP + R + sgr(30, 46) + f" {CLOCK} 09:41 " + R + sgr(33, 46) + RSEP + R
             + sgr(30, 43) + " dev@jetty " + R)
    lines.append(left + " " * (cols - 27 - 22 - 1) + right)
    return "\r\n".join(lines[-rows:]) + HIDE_CURSOR
