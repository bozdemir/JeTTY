/// Headless offscreen screenshot tool — renders one terminal frame to a PNG
/// with NO window, surface, or display.
///
/// Config via env:
///   JETTY_SHOT_OUT   — output path (default: /tmp/jetty-shot.png)
///   JETTY_SHOT_INPUT — ANSI bytes to feed the terminal (default: built-in
///                    sample). With JETTY_SHOT_TABBAR, an OSC 0/2 title in the
///                    input (`\e]2;my title\a`) retitles the first tab.
///   JETTY_SHOT_ATTRS — "1" feeds a built-in TEXT-ATTRIBUTE self-test sample
///                    (used only when JETTY_SHOT_INPUT is unset): a plain
///                    reference row + the same text bold+italic (column-alignment
///                    check), then every underline style (single / \e[4:2m double
///                    / \e[4:3m undercurl / \e[4:4m dotted / \e[4:5m dashed), a
///                    colored undercurl (SGR 58), strikethrough, and combos — so
///                    a single screenshot verifies bold/italic/underline/strike +
///                    the monospace invariant.
///   JETTY_SHOT_CURSOR_UNFOCUSED — "1" draws the cursor as if the window is
///                    UNFOCUSED (a Block cursor becomes a hollow box). The cursor
///                    SHAPE itself comes from the snapshot, so append a DECSCUSR
///                    to JETTY_SHOT_INPUT (`\e[1 q` block, `\e[3 q` underline,
///                    `\e[5 q` beam) to screenshot each shape.
///   JETTY_THEME      — theme name (picked up automatically via Terminal::new)
///   JETTY_OPACITY    — opacity 0.0..1.0 (picked up automatically via Terminal::new)
///   JETTY_SHOT_MIN_CONTRAST — `minimum_contrast` ratio (1 = off .. 21).
///   JETTY_SHOT_UI_FONT_SIZE — UI (chrome) font size in logical pt (10..28,
///                    default 16). Drives ALL chrome (tab bar/status/menu/panel/
///                    help/confirm/welcome) and the panel's live "Aa" specimen.
///   JETTY_SHOT_UI_FONT — UI (chrome) font family (default "" = platform sans).
///   JETTY_SHOT_SCALE — simulated display DPI scale (0.5..4, default 1; 2 =
///                    Retina / 4K@200%). Fonts rasterize at size × scale and the
///                    chrome follows ChromeMetrics, exactly like the live app.
///                    Pass PHYSICAL JETTY_SHOT_WIDTH/HEIGHT (e.g. 2000×1280).
///   JETTY_SHOT_PADDING="x,y" — the grid's inner padding in LOGICAL px (the
///                    `padding_x`/`padding_y` config keys; default = their
///                    defaults, "0,0" = the unpadded grid). Scaled by
///                    JETTY_SHOT_SCALE like the app; every grid-anchored layer
///                    (cells, glyphs, cursor, decorations, images, hint chips,
///                    preedit, failed-command bars) moves with it.
///   JETTY_SHOT_LINE_HEIGHT — the grid line height as a multiple of the font
///                    size (the `line_height` key, 1.0..2.0, default 1.3).
///   JETTY_SHOT_SCROLLBAR=always|auto|never — the `scrollbar` key (default
///                    always): "never" drops the thumb AND the gutter (more
///                    columns); "auto" shows the thumb only while scrolled back
///                    (JETTY_SHOT_SCROLL) or with JETTY_SHOT_SCROLLBAR_HOVER=1
///                    (the pointer over the gutter).
///   JETTY_SHOT_TABBAR_N — number of sample tabs for JETTY_SHOT_TABBAR (default 3).
///   JETTY_SHOT_TAB_TITLES — comma list of the sample tabs' titles (default
///                    "Tab N"; an OSC 0/2 title in the input still wins for tab 1).
///                    The first two also name the palette's "Switch to tab"
///                    rows, and the second the close-tab confirmation's tab.
///   JETTY_SHOT_HELP_SCROLL — first help row for JETTY_SHOT_HELP when its rows
///                    overflow the window (large UI font / short window).
///   JETTY_SHOT_PILL="text" — draw the app's toast pill (run-selection status /
///                    Shift-drag hint surface) above the status strip.
///   JETTY_SHOT_TABBAR_ACTIVITY — comma list aligned with the sample tabs
///                    (`none|output|bell|done|failed`, unknown → none), e.g.
///                    `none,output,bell` — draws the activity / bell / finished
///                    / failed badges on the inactive tabs.
///   JETTY_SHOT_TAB_STYLE — `pill|underline|slant|powerline|compact` (the
///                    `tab_style` key; default pill). Applies to the tab bar and
///                    the detached bar.
///   JETTY_SHOT_TAB_CLOSE — `always|hover|active` (`tab_close_button`).
///   JETTY_SHOT_TAB_HOVER — index of the tab under the pointer (hover lift).
///   JETTY_SHOT_TAB_BAR_OPACITY — "1": the bar follows the window opacity
///                    (`tab_bar_opacity`; combine with JETTY_OPACITY).
///   JETTY_SHOT_TAB_PROGRESS — comma list aligned with the tabs of OSC 9;4
///                    states: `40` (40 %), `e30` / `e` (error, with / without a
///                    value), `i` (indeterminate), `p50` (paused), `-` (none).
///                    An OSC 9;4 inside JETTY_SHOT_INPUT (`\e]9;4;1;40\e\\`)
///                    sets tab 1's through the real parser.
///                    JETTY_SHOT_PROGRESS_BAR=0 hides progress (`progress_bar`).
///   JETTY_SHOT_TAB_COLORS — comma list of per-tab colors (palette 1–6, `-`
///                    for none), aligned with the tabs.
///   JETTY_SHOT_TAB_MENU — "1" draws the tab context menu under tab 1;
///                    "colors" its Color ▸ list with swatches.
///                    JETTY_SHOT_TAB_MENU_HOVER=n highlights row n.
///   JETTY_SHOT_MENU / JETTY_SHOT_DMENU — "1" draws the terminal / detached
///                    window's context menu (_DISABLED=1: the no-selection
///                    grayed rows). JETTY_SHOT_MENU_AT=cursor opens them where
///                    the Menu key does: below the text cursor's line, or
///                    above it when the card does not fit below.
///   JETTY_SHOT_MENU_KEYS="down,down,end" — the menus (all three) the keyboard
///                    way: open on the first enabled row, then each key (up /
///                    down / home / end) moves the highlight through the app's
///                    own `menunav` (wrapping, grayed rows skipped).
///   JETTY_SHOT_WINDOW_BORDER — `focus|always` draws the window ring (the real
///                    GPU pass, before the corner mask) in the accent or tab 1's
///                    color; JETTY_SHOT_CURSOR_UNFOCUSED=1 shows the unfocused
///                    state (none / the muted border).
///   JETTY_SHOT_DETACHED — "1" renders the DETACHED-window chrome: top bar
///                    (title + ✕), grid offset below it, bottom status strip.
///                    JETTY_SHOT_DETACHED_TITLE / _HOVER tweak title and the
///                    ✕ hover state.
///   JETTY_SHOT_LINK_HOVER — "row,col" (0-based viewport cell): print
///                    `link_at(row,col)` to stderr (URL under that cell, OSC 8
///                    or plain text) and draw the app's themed Ctrl+hover
///                    underline for the hit — plus the target pill when it is
///                    an OSC 8 link whose text is not its target.
///   JETTY_SHOT_OSC133 — "1" feeds a scripted OSC 133 A/C/D;<exit> sequence (a
///                    FAILED command, a passing one, then another failed one)
///                    after the input, so the PNG shows the themed left-edge
///                    failed-command marker on each failed prompt row. Combine
///                    with JETTY_SHOT_SCROLL to verify the marker tracks the
///                    correct viewport row after the mark scrolls into history.
///   JETTY_SHOT_SIXEL — inline-image self-test: feed a sixel DCS so the PNG shows
///                    a real bitmap drawn over the grid (scanner → decode → cell
///                    reservation → placement → ImageLayer). "1" = a built-in
///                    30×24 red/green/blue-stripe pattern; any other value is
///                    treated as a raw sixel BODY (bytes after `q`, before the
///                    ST). Prints decoded WxH / footprint / anchor row to stderr;
///                    combine with JETTY_SHOT_SCROLL to verify it tracks into
///                    history. e.g. `img2sixel img.png` yields a body you can pass
///                    (strip the leading `\ePq` and trailing `\e\\`).
///   JETTY_SHOT_PALETTE — command-palette self-test: render the fuzzy command
///                    palette overlay via the SHARED registry + filter path.
///                    JETTY_SHOT_PALETTE_QUERY sets the typed query (drives the
///                    matched-char highlight), JETTY_SHOT_PALETTE_SEL the selected
///                    row index (auto-scrolled into view).
///   JETTY_SHOT_SEARCH — scrollback-search self-test: set this query on the
///                    terminal after the input (and any JETTY_SHOT_SCROLL) is
///                    applied, print the "cur/total" counter to stderr, draw
///                    the match highlights (current match tinted stronger) and
///                    the themed search bar at the top-right of the grid.
///   JETTY_SHOT_HINTS — hint-mode self-test: overlay the home-row label chips on
///                    every visible URL / file-path / git-hash / IPv4 (real
///                    Terminal::hint_tokens + assign_labels + build_hint_overlay);
///                    JETTY_SHOT_HINTS_TYPED="s" narrows to matching labels (typed
///                    prefix dimmed). Feed a token-rich line via JETTY_SHOT_INPUT.
///   JETTY_SHOT_PREEDIT="text" — an IME composition at the cursor, drawn by the
///                    app's own builder (build_preedit_overlay): terminal font,
///                    theme bg backdrop, underline; shifts left at the edge.
///   JETTY_SHOT_GRAPHEMES="row,col,cluster;…" — extra grapheme-cluster overrides
///                    for the renderer (combining marks / VS16 / ZWJ drawn from
///                    the whole cluster instead of the cell's base char), on top
///                    of the ones the snapshot carries for the fed input.
///   JETTY_SHOT_BUILTIN_GLYPHS=0 — draw box drawing / blocks / Powerline / braille
///                    / sextants from the font instead of the built-in
///                    cell-exact glyphs (config `builtin_glyphs`, default on).
///   JETTY_SHOT_COLOR_EMOJI=0 — no color emoji (config `color_emoji`, default on).
///   JETTY_SHOT_BOLD_BRIGHT=1 — bold text in the 8 normal ANSI colors renders
///                    bright (config `bold_is_bright`, default off).
///   JETTY_SHOT_PANEL=1 — render the Settings window instead of a terminal: the
///                    frame defaults to the Settings window's physical size and
///                    the panel content comes from the config in
///                    JETTY_CONFIG_DIR (defaults without one), so every
///                    non-default setting shows; env knobs override single keys
///                    (JETTY_THEME, JETTY_OPACITY, JETTY_CORNER_RADIUS,
///                    JETTY_SHOT_UI_FONT[_SIZE], JETTY_SHOT_PANEL_WINMODE/EFFECT/
///                    DH/DW/AUTOHIDE/LAUNCH/SHELL/FULLSCREEN). Panel state:
///                    JETTY_SHOT_PANEL_TAB=0..4, JETTY_SHOT_PANEL_SCROLL=<px>|<n>%|max
///                    (alias _FX_SCROLL), JETTY_SHOT_PANEL_FILTER=all|dark|light|mine,
///                    JETTY_SHOT_PANEL_HOVER=<theme name>|reset|filter:<f>|section:<id>,
///                    JETTY_SHOT_PANEL_COLLAPSE=<section id,...>,
///                    JETTY_SHOT_PANEL_FOCUS=<control id> (a deep link: its tab,
///                    scrolled to it, highlighted, its first part keyboard-
///                    focused and ringed; a control the config hides lands
///                    on the row that reveals it), JETTY_SHOT_PANEL_RESET=
///                    armed|ready|disabled, JETTY_SHOT_PANEL_BACKDROP=<mode>, JETTY_SHOT_PANEL_PRESET=<id> (an effects
///                    preset on the panel config only), JETTY_SHOT_PANEL_SESSION=1 (a gallery
///                    browsing session's footer hint).
///   JETTY_SHOT_COPYMODE="row,col" — copy-mode self-test: draw the keyboard cursor
///                    (hollow box) + the "COPY" pill at (row,col). With
///                    JETTY_SHOT_COPYMODE_ANCHOR="row,col" also drive a live
///                    selection anchor→cursor (JETTY_SHOT_COPYMODE_LINE=1 = whole
///                    lines, JETTY_SHOT_COPYMODE_BLOCK=1 = a rectangle),
///                    rendering the real selection tint.
///   JETTY_SHOT_CRT   — run the REAL CRT post pass through the app's own settings
///                    path (`effects::crt_settings` → `CrtParams::build`). "1" =
///                    the harness look (curvature .24, scanline .55, mask .32,
///                    bloom .45, chromatic .22, vignette .45); "config" = the
///                    `[effects]` table of $JETTY_CONFIG_DIR/config.toml.
///                    Per-key overrides: JETTY_SHOT_CRT_{CURVATURE, SCANLINE,
///                    MASK, BLOOM, BLOOM_RADIUS, CHROMATIC, VIGNETTE, PHOSPHOR
///                    (off|amber|green|white|blue|paper|custom), PHOSPHOR_HUE,
///                    PHOSPHOR_COLOR (#rrggbb), GRAIN, DITHER, ROLL, FLICKER,
///                    JITTER (0/1), TIME (seconds), RADIUS (corner px)}.
///   JETTY_SHOT_PRESET=<id|name> — apply an effects preset (`effects::
///                    effect_presets`: clean, retro_crt, amber, green_phosphor,
///                    neon, paper, e_ink) on top of the base above (CRT on
///                    unless Clean); the per-key overrides still apply after it.
///   JETTY_SHOT_GLITCH=<0..1> — one event-glitch frame at that intensity (with
///                    CRT off: the glitch-only pass). JETTY_SHOT_CRT_TIME picks
///                    the tear pattern.
///   JETTY_SHOT_CRT_BENCH=<n> — time the post pass: pipeline build once, then n
///                    passes (GPU-synchronized) → ms/pass on stderr.
///   JETTY_SHOT_BACKDROP — draw the `[backdrop]` layer (unset = none, today's
///                    shots unchanged): `config` (the `[backdrop]` table of the
///                    config in JETTY_CONFIG_DIR), a mode (`theme`, `gradient`,
///                    `image`, `pattern`, `none`) or a pattern name (`stars`,
///                    `aurora`, `grid`, `synthwave`). Overrides, each optional:
///                    JETTY_SHOT_BACKDROP_{COLORS="#a,#b",ANGLE,SHAPE,STRENGTH,
///                    VIGNETTE,GRAIN,IMAGE=path,FIT,DIM,BLUR,PATTERN}; _TIME=s
///                    animates to that phase; _SCROLL=px turns on parallax at that
///                    scrollback position; _SLIDE=px offsets it like the dropdown
///                    slide. The image is decoded synchronously (the app uses a
///                    worker thread) and scaled to cover the shot.
///   JETTY_SHOT_CARET_T=t — a caret-flash frame at progress t (0..1; the peak
///                    is t≈0.29) with JETTY_SHOT_CARET_COLOR="r,g,b" (0..1,
///                    default white), through the app's contrast-safe path.
///   JETTY_SHOT_CURSOR="key=value,…" — a `[cursor]` table in compact form
///                    (shape, thickness, unfocused, color, guide — e.g.
///                    "shape=double_underline,thickness=0.2,guide=always"),
///                    parsed by the app's own parser: the shape becomes the
///                    terminal's default (DECSCUSR in JETTY_SHOT_INPUT still
///                    wins), the rest is the render look + the row guide.
///   JETTY_SHOT_TRAIL="row,col,ms" — the cursor trail `ms` milliseconds after
///                    the cursor jumped from cell (row,col) to where the input
///                    left it (trail_ms 200, threshold 2 unless
///                    JETTY_SHOT_TRAIL_MS / _THRESHOLD say otherwise) — the
///                    app's model and its 6-vertex pass inside the grid pass.
///   JETTY_SHOT_TRANSFORM_T=t — the Pop / Glide / Fade summon effect
///                    (JETTY_SHOT_TRANSFORM=pop|glide|fade, default pop) at
///                    progress t: the real Tier-B pass sampling the frame.
///   JETTY_SHOT_SLIDE_T=t — a Dropdown slide-in frame at progress t (0..1):
///                    the content moved up by the slide's ease-out offset and
///                    the window SHAPE cut at the moving bottom edge (square
///                    top, JETTY_CORNER_RADIUS bottom corners — the app's mask).
///   JETTY_SHOT_BELL=flash|rim — the visual bell at JETTY_SHOT_BELL_T=t (0..1
///                    of its 150 ms, default 0.15 = the peak).
///   JETTY_SHOT_PULSE=failure|success — the command status pulse at
///                    JETTY_SHOT_PULSE_T=t (0..1 of its 0.4 s, default 0.15).
///                    Both use the app's colors (UiPalette warn / danger /
///                    accent), its pass (the focus ring, soft) and the window radius
///                    (JETTY_CORNER_RADIUS).
///   JETTY_SHOT_GLOW_T=t — the caret glow/ripple pass at progress t around the
///                    cursor (additive on a dark theme, multiply on a light
///                    one; same color source as JETTY_SHOT_CARET_COLOR).
///
/// If the terminal bg alpha < 255, the rendered image is composited over a
/// checkerboard (alternating 16px squares of [40,40,40] and [90,90,90]) so
/// transparency is visible in the output PNG. JETTY_SHOT_UNDERLAY=none keeps
/// the real alpha instead (a straight-alpha RGBA PNG).
use std::fs::File;
use std::io::BufWriter;

use jetty_render::{QuadLayer, TextLayer};

/// The `JETTY_SHOT_BACKDROP` layer, built and prepared for one frame (see the
/// header doc), or `None` when the hook is unset / "none".
#[allow(clippy::too_many_arguments)]
fn shot_backdrop(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    dpi: f32,
    theme: &jetty_core::Theme,
) -> Option<jetty_render::Backdrop> {
    use jetty_render::{BackdropFit, BackdropMode, BackdropPattern, BackdropSettings, BackdropShape};
    let spec = std::env::var("JETTY_SHOT_BACKDROP").ok().filter(|v| !v.is_empty())?;
    let env = |k: &str| std::env::var(format!("JETTY_SHOT_BACKDROP_{k}")).ok().filter(|v| !v.is_empty());
    let envf = |k: &str| env(k).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
    let (mut s, mut image) = if spec == "config" {
        jetty_app::configured_backdrop()
    } else {
        (BackdropSettings::default(), None)
    };
    match spec.as_str() {
        "config" => {}
        "stars" | "aurora" | "grid" | "synthwave" => {
            s.mode = BackdropMode::Pattern;
            s.pattern = BackdropPattern::parse(&spec);
        }
        other => s.mode = BackdropMode::parse(other),
    }
    if let Some(v) = env("COLORS") {
        s.colors = v.split(',').filter_map(jetty_render::parse_hex_color).collect();
    }
    if let Some(v) = env("SHAPE") {
        s.shape = BackdropShape::parse(&v);
    }
    if let Some(v) = env("FIT") {
        s.fit = BackdropFit::parse(&v);
    }
    if let Some(v) = env("PATTERN") {
        s.pattern = BackdropPattern::parse(&v);
    }
    if let Some(v) = env("IMAGE") {
        image = Some(v.into());
    }
    let c01 = |v: f32| v.clamp(0.0, 1.0);
    s.angle = envf("ANGLE").unwrap_or(s.angle);
    s.strength = envf("STRENGTH").map(c01).unwrap_or(s.strength);
    s.vignette = envf("VIGNETTE").map(c01).unwrap_or(s.vignette);
    s.grain = envf("GRAIN").map(c01).unwrap_or(s.grain);
    s.dim = envf("DIM").map(c01).unwrap_or(s.dim);
    s.blur = envf("BLUR").map(c01).unwrap_or(s.blur);
    let time = envf("TIME");
    s.animate |= time.is_some();
    let scroll = envf("SCROLL");
    s.parallax |= scroll.is_some();
    if s.is_off() {
        return None;
    }
    eprintln!("jetty-shot: backdrop {s:?}");
    let gpu_image = match (s.mode, image) {
        (BackdropMode::Image, Some(path)) => {
            let t = std::time::Instant::now();
            match jetty_render::backdrop_image::load(&path, width, height, s.blur) {
                Ok(img) => {
                    eprintln!(
                        "jetty-shot: backdrop image {} decoded in {:.1} ms: {img:?}",
                        path.display(),
                        t.elapsed().as_secs_f64() * 1000.0
                    );
                    jetty_render::GpuImage::upload(device, queue, &img).map(std::sync::Arc::new)
                }
                Err(e) => {
                    eprintln!("jetty-shot: backdrop image {}: {e} — showing the gradient", path.display());
                    None
                }
            }
        }
        _ => None,
    };
    let mut bd = jetty_render::Backdrop::new(device, format);
    let frame = jetty_render::BackdropFrame {
        width,
        height,
        slide_y: envf("SLIDE").unwrap_or(0.0),
        scroll_px: scroll.unwrap_or(0.0),
        dpi,
        // The shot clears premultiplied (the harness composites itself).
        premultiply: true,
        time: time.unwrap_or(0.0),
        image: gpu_image.as_ref(),
    };
    bd.prepare(device, queue, &s, theme, &frame).then_some(bd)
}

/// A boolean shot flag is ON only when set to a non-empty value other than "0",
/// matching the JETTY_SHOT_DETACHED / JETTY_SHOT_PANEL_* semantics so a harness
/// can turn a mode off with `=0` instead of the mode staying on for any value.
fn env_flag(k: &str) -> bool {
    std::env::var(k).map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
}

/// Sample tab `n`'s title (1-based): JETTY_SHOT_TAB_TITLES' n-th entry, else
/// "Tab n".
fn sample_tab_title(n: usize) -> String {
    let titles = std::env::var("JETTY_SHOT_TAB_TITLES").unwrap_or_default();
    let named = titles.split(',').map(str::trim).nth(n - 1).filter(|t| !t.is_empty());
    named.map_or_else(|| format!("Tab {n}"), str::to_string)
}

/// The highlighted row of a shot menu of `rows` rows (`disabled` grayed).
/// JETTY_SHOT_MENU_KEYS="down,down,end" drives it the keyboard way: the menu
/// opens on its first enabled row (as the Menu key opens it), then each key
/// (up / down / home / end) moves the highlight through the app's own
/// `menunav` — the logic its key handler runs. Without it: `fallback`.
fn shot_menu_hover(rows: usize, disabled: &[usize], fallback: Option<usize>) -> Option<usize> {
    use jetty_app::menunav::{self, MenuPress, MenuStep};
    use winit::keyboard::{Key, NamedKey};
    let Ok(keys) = std::env::var("JETTY_SHOT_MENU_KEYS") else { return fallback };
    let mut hover = menunav::first_enabled(rows, disabled);
    for k in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        let named = match k.to_ascii_lowercase().as_str() {
            "up" => NamedKey::ArrowUp,
            "down" => NamedKey::ArrowDown,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            other => {
                eprintln!("jetty-shot: JETTY_SHOT_MENU_KEYS: skipping {other:?} (up/down/home/end)");
                continue;
            }
        };
        let MenuPress::Key(key) = menunav::classify(&Key::Named(named), jetty_app::keymap::Mods::default()) else {
            continue;
        };
        if let Some(MenuStep::Highlight(h)) = menunav::step(key, hover, rows, disabled) {
            hover = h;
        }
    }
    eprintln!("jetty-shot: JETTY_SHOT_MENU_KEYS={keys:?} -> highlighted row {hover:?}");
    hover
}

/// The `[effects]` the shot's post pass renders with (see the JETTY_SHOT_CRT /
/// JETTY_SHOT_PRESET / JETTY_SHOT_GLITCH docs) and the glitch intensity; `None`
/// when no post pass was asked for. Base (harness look, config, or defaults) →
/// preset → per-key overrides → the app's own sanitizing.
fn shot_effects() -> Option<(jetty_app::effects::EffectsConfig, f32)> {
    use jetty_app::effects::{self, EffectsConfig, PhosphorMode};
    let crt = std::env::var("JETTY_SHOT_CRT").unwrap_or_default();
    let preset = std::env::var("JETTY_SHOT_PRESET").ok().filter(|s| !s.is_empty());
    let glitch = std::env::var("JETTY_SHOT_GLITCH")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|g| g.is_finite())
        .map(|g| g.clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let crt_on = !crt.is_empty() && crt != "0";
    if !crt_on && preset.is_none() && glitch <= 0.0 {
        return None;
    }
    let mut fx = if crt == "config" {
        effects::configured_effects()
    } else if crt_on {
        EffectsConfig {
            crt_enabled: true,
            crt_curvature: 0.24,
            crt_scanline: 0.55,
            crt_mask: 0.32,
            crt_bloom: 0.45,
            crt_chromatic: 0.22,
            crt_vignette: 0.45,
            ..EffectsConfig::default()
        }
    } else {
        EffectsConfig::default()
    };
    if let Some(name) = preset {
        match effects::find_preset(&name) {
            Some(p) => {
                p.patch.apply_to(&mut fx);
                eprintln!("jetty-shot: effects preset {:?}", p.name);
            }
            None => eprintln!("jetty-shot: unknown effects preset {name:?} (ignored)"),
        }
    }
    let f = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok());
    let b = |k: &str| std::env::var(k).ok().map(|v| v != "0" && !v.is_empty());
    macro_rules! set_f {
        ($($key:literal => $field:ident),* $(,)?) => { $( if let Some(v) = f($key) { fx.$field = v; } )* };
    }
    set_f!(
        "JETTY_SHOT_CRT_CURVATURE" => crt_curvature,
        "JETTY_SHOT_CRT_SCANLINE" => crt_scanline,
        "JETTY_SHOT_CRT_MASK" => crt_mask,
        "JETTY_SHOT_CRT_BLOOM" => crt_bloom,
        "JETTY_SHOT_CRT_BLOOM_RADIUS" => crt_bloom_radius,
        "JETTY_SHOT_CRT_CHROMATIC" => crt_chromatic,
        "JETTY_SHOT_CRT_VIGNETTE" => crt_vignette,
        "JETTY_SHOT_CRT_PHOSPHOR_HUE" => crt_phosphor_hue,
        "JETTY_SHOT_CRT_GRAIN" => crt_grain,
    );
    if let Ok(m) = std::env::var("JETTY_SHOT_CRT_PHOSPHOR") {
        match PhosphorMode::from_name(&m) {
            Some(m) => fx.crt_phosphor = m,
            None => eprintln!("jetty-shot: unknown JETTY_SHOT_CRT_PHOSPHOR {m:?} (ignored)"),
        }
    }
    if let Ok(hex) = std::env::var("JETTY_SHOT_CRT_PHOSPHOR_COLOR") {
        let h = hex.trim().trim_start_matches('#');
        match u32::from_str_radix(h, 16) {
            Ok(c) if h.len() == 6 => {
                fx.crt_phosphor_color =
                    [(c >> 16) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0];
            }
            _ => eprintln!("jetty-shot: JETTY_SHOT_CRT_PHOSPHOR_COLOR wants #rrggbb, got {hex:?}"),
        }
    }
    if let Some(v) = b("JETTY_SHOT_CRT_DITHER") {
        fx.crt_dither = v;
    }
    if let Some(v) = b("JETTY_SHOT_CRT_ROLL") {
        fx.crt_animate_roll = v;
    }
    if let Some(v) = b("JETTY_SHOT_CRT_FLICKER") {
        fx.crt_flicker = v;
    }
    if let Some(v) = b("JETTY_SHOT_CRT_JITTER") {
        fx.crt_jitter = v;
    }
    if glitch > 0.0 {
        // The burst needs its trigger on (that is what compiles the glitch in).
        fx.glitch_on_error = true;
    }
    Some((fx.clamped(), glitch))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Seed the theme registry (built-ins + user themes) so a JETTY_THEME naming a
    // user-imported theme resolves in Terminal::new / the panel — same as the app.
    jetty_app::themes::rebuild_registry();

    let out_path =
        std::env::var("JETTY_SHOT_OUT").unwrap_or_else(|_| "/tmp/jetty-shot.png".to_string());

    // Window size is overridable so the harness can reproduce narrow-window
    // layouts (e.g. help/panel fit) — JETTY_SHOT_WIDTH / JETTY_SHOT_HEIGHT.
    // Clamp to 1..=8192 (the wgpu::Limits::default() max_texture_dimension_2d
    // requested below): 0 or an oversized value would trip a wgpu validation
    // panic deep inside create_texture instead of a usable render.
    let width_env: Option<u32> = std::env::var("JETTY_SHOT_WIDTH").ok().and_then(|s| s.parse().ok()).map(|v: u32| v.clamp(1, 8192));
    let height_env: Option<u32> = std::env::var("JETTY_SHOT_HEIGHT").ok().and_then(|s| s.parse().ok()).map(|v: u32| v.clamp(1, 8192));
    // JETTY_SHOT_PANEL=1 renders the Settings window: its content comes from the
    // config in JETTY_CONFIG_DIR (never the user's real config dir — without the
    // override the defaults are used), with single keys overridable from env.
    let shot_cfg: Option<jetty_app::config::Config> = env_flag("JETTY_SHOT_PANEL").then(|| {
        let mut cfg = if std::env::var_os("JETTY_CONFIG_DIR").is_some() {
            jetty_app::config::Config::load().cfg
        } else {
            jetty_app::config::Config::default()
        };
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let num = |k: &str| var(k).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
        if let Some(t) = var("JETTY_THEME") {
            cfg.theme = t;
        }
        if let Some(v) = num("JETTY_OPACITY") {
            cfg.opacity = v.clamp(0.1, 1.0);
        }
        if let Some(v) = num("JETTY_CORNER_RADIUS") {
            cfg.corner_radius = v.clamp(0.0, 24.0);
        }
        if let Some(v) = var("JETTY_SHOT_PANEL_EFFECT") {
            cfg.summon_effect = v.to_lowercase();
        }
        if let Some(v) = var("JETTY_SHOT_PANEL_WINMODE") {
            cfg.window_mode = v.to_lowercase();
        }
        if let Some(v) = var("JETTY_TAB_BAR") {
            cfg.tab_bar_position = v;
        }
        if let Some(v) = num("JETTY_SHOT_PANEL_DH") {
            cfg.dropdown_height_pct = v.clamp(0.25, 1.0);
        }
        if let Some(v) = num("JETTY_SHOT_PANEL_DW") {
            cfg.dropdown_width_pct = v.clamp(0.2, 1.0);
        }
        if let Some(v) = var("JETTY_SHOT_PANEL_AUTOHIDE") {
            cfg.focus_autohide = v != "0";
        }
        if let Some(v) = var("JETTY_SHOT_PANEL_LAUNCH") {
            cfg.launch_at_login = v != "0";
        }
        if let Some(v) = var("JETTY_SHOT_PANEL_SHELL") {
            cfg.shell = v;
        }
        if let Some(v) = num("JETTY_SHOT_UI_FONT_SIZE") {
            cfg.ui_font_size = v.clamp(10.0, 28.0);
        }
        if let Ok(v) = std::env::var("JETTY_SHOT_UI_FONT") {
            cfg.ui_font_family = v;
        }
        // An effects preset applied to the PANEL's config only (it lights its
        // chip) — unlike JETTY_SHOT_PRESET, no CRT pass runs over the shot.
        if let Some(p) = var("JETTY_SHOT_PANEL_PRESET").and_then(|n| jetty_app::effects::find_preset(&n)) {
            p.patch.apply_to(&mut cfg.effects);
        }
        if let Some(m) = var("JETTY_SHOT_PANEL_BACKDROP") {
            cfg.backdrop.mode = m;
        }
        cfg
    });
    // Allow headless renders at different font sizes so the test harness can
    // verify that font-size changes produce a different cell grid.
    let font_size: f32 = std::env::var("JETTY_FONT_SIZE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(6.0, 48.0))
        .unwrap_or(16.0);
    // JETTY_SHOT_SCALE — simulated display DPI scale (default 1; e.g. 2 for a
    // Retina / 4K@200% screen). Like the live app, every font is rasterized at
    // logical × scale and the chrome geometry follows `ChromeMetrics` (DPI × UI
    // font). Pass a matching JETTY_SHOT_WIDTH/HEIGHT (PHYSICAL px).
    let dpi: f32 = std::env::var("JETTY_SHOT_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.5, 4.0))
        .unwrap_or(1.0);

    let default_input = "\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/jetty\x1b[0m$ ls --color\r\n\x1b[1;34msrc\x1b[0m  \x1b[33mCargo.toml\x1b[0m  \x1b[31mREADME.md\x1b[0m\r\n\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/jetty\x1b[0m$ \r\n";
    // JETTY_SHOT_ATTRS=1: built-in text-attribute self-test sample (used only when
    // JETTY_SHOT_INPUT is not set). Row 1 is a PLAIN reference; row 2 is the same
    // text in bold+italic so columns can be checked for alignment (the monospace
    // invariant). The remaining rows exercise every underline style (single,
    // \e[4:2m double, \e[4:3m undercurl, \e[4:4m dotted, \e[4:5m dashed), a colored
    // undercurl (SGR 58), strikethrough, and combinations. Append a DECSCUSR
    // (e.g. `\e[5 q`) via JETTY_SHOT_INPUT to also pick a cursor shape.
    let attrs_sample = "The quick brown fox 0123456789 |\r\n\
        \x1b[1;3mThe quick brown fox 0123456789 |\x1b[0m\r\n\
        \x1b[1mBold\x1b[0m \x1b[3mItalic\x1b[0m \x1b[1;3mBoldItalic\x1b[0m plain\r\n\
        \x1b[4mSingle underline\x1b[0m\r\n\
        \x1b[4:2mDouble underline\x1b[0m\r\n\
        \x1b[4:3mUndercurl underline\x1b[0m\r\n\
        \x1b[4:4mDotted underline\x1b[0m\r\n\
        \x1b[4:5mDashed underline\x1b[0m\r\n\
        \x1b[58;2;255;80;80m\x1b[4:3mColored undercurl (LSP red)\x1b[0m\r\n\
        \x1b[9mStrikethrough text\x1b[0m\r\n\
        \x1b[1m\x1b[4mBold+underline\x1b[0m  \x1b[3m\x1b[9mItalic+strike\x1b[0m\r\n";
    let input_bytes: Vec<u8> = match std::env::var("JETTY_SHOT_INPUT") {
        Ok(s) => s.into_bytes(),
        Err(_) if env_flag("JETTY_SHOT_ATTRS") => attrs_sample.as_bytes().to_vec(),
        Err(_) => default_input.as_bytes().to_vec(),
    };

    // --- wgpu offscreen setup (no surface) ---
    // Match the live app: Vulkan-only instance (skips GLES enumeration), with an
    // all-backends fallback if no Vulkan adapter is present.
    let mut instance = wgpu::Instance::new(jetty_render::instance_descriptor(wgpu::Backends::VULKAN));
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            instance = wgpu::Instance::new(jetty_render::instance_descriptor(wgpu::Backends::all()));
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))?
        }
    };

    let info = adapter.get_info();
    let driver = if info.driver_info.is_empty() { String::new() } else { format!(", {}", info.driver_info) };
    let backend = jetty_render::backend_display_name(info.backend);
    eprintln!("jetty-shot: GPU adapter = {} ({backend}{driver})", info.name);

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jetty-shot-device"),
            required_features: wgpu::Features::empty(),
            // What the live app requests (`GpuContext::new`): a downlevel GL / GLES
            // 3.0 adapter sits below `Limits::default()` and refused the device.
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            ..Default::default()
        }))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // Allow rendering with a specific font family for visual comparison.
    let font_family = std::env::var("JETTY_FONT_FAMILY")
        .unwrap_or_else(|_| "MesloLGS NF".to_string());
    if std::env::var("JETTY_FONT_FAMILY").is_ok() {
        eprintln!("jetty-shot: JETTY_FONT_FAMILY={font_family:?}");
    }

    // UI (chrome) font: size (10..28, default 16) + family ("" = platform sans).
    // Mirrors the live app's separate UI font — drives ALL chrome and the panel
    // "Aa" specimen, independent of the terminal grid font (JETTY_FONT_SIZE).
    let ui_font_size: f32 = match &shot_cfg {
        Some(cfg) => cfg.ui_font_size.clamp(10.0, 28.0),
        None => std::env::var("JETTY_SHOT_UI_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|v| v.clamp(10.0, 28.0))
            .unwrap_or(16.0),
    };
    let ui_font_family = match &shot_cfg {
        Some(cfg) => cfg.ui_font_family.clone(),
        None => std::env::var("JETTY_SHOT_UI_FONT").unwrap_or_default(),
    };
    // The frame: JETTY_SHOT_WIDTH/HEIGHT, else the Settings window's physical
    // size in panel mode (the app's `desired_settings_logical_size`: the design
    // size scaled by the panel's chrome unit, never below it, +2 px each side),
    // else 1000×640.
    let (width, height) = {
        let panel = shot_cfg.as_ref().map(|_| {
            let u = jetty_render::ChromeMetrics::new(dpi, ui_font_size.clamp(13.0, 17.0)).overlay_u();
            let f = (u / dpi).max(1.0);
            let w = (jetty_render::PANEL_W * f).ceil() + 4.0;
            let h = (jetty_render::PANEL_H * f).ceil() + 4.0;
            (((w * dpi).round() as u32).clamp(1, 8192), ((h * dpi).round() as u32).clamp(1, 8192))
        });
        (
            width_env.or(panel.map(|p| p.0)).unwrap_or(1000),
            height_env.or(panel.map(|p| p.1)).unwrap_or(640),
        )
    };
    if std::env::var("JETTY_SHOT_UI_FONT_SIZE").is_ok() || std::env::var("JETTY_SHOT_UI_FONT").is_ok() {
        eprintln!("jetty-shot: UI font size={ui_font_size}, family={ui_font_family:?}");
    }

    // --- Build TextLayer ---
    let mut text = TextLayer::new_with_family(&device, &queue, format, font_size * dpi, &font_family);
    // JETTY_SHOT_LINE_HEIGHT — the grid's row spacing, like the app's
    // `line_height` key (clamped to 1.0..2.0 by the layer).
    if let Some(lh) = std::env::var("JETTY_SHOT_LINE_HEIGHT").ok().and_then(|s| s.parse::<f32>().ok()) {
        text.set_line_height(lh);
    }
    // The grid's glyph options (config `builtin_glyphs` / `color_emoji`, both on
    // by default): `=0` turns one off.
    let env_off = |k: &str| std::env::var(k).is_ok_and(|v| v == "0");
    text.set_builtin_glyphs(!env_off("JETTY_SHOT_BUILTIN_GLYPHS"));
    text.set_color_emoji(!env_off("JETTY_SHOT_COLOR_EMOJI"));
    // Chrome layer at the UI font size, mirroring the live app: ALL window chrome
    // (tab bar, status bar, context menu, settings panel, help, confirm, palette,
    // …) renders through this in the chosen UI family, independent of
    // JETTY_FONT_SIZE. The terminal grid renders through `text` (which scales with
    // the font). Built from the grid layer's font database, like the app (no
    // second font scan).
    let mut chrome_text = TextLayer::new_with_family_and_fonts(
        &device, &queue, format, ui_font_size * dpi, &font_family, text.clone_font_system(),
    );
    chrome_text.set_ui_family(if ui_font_family.is_empty() { None } else { Some(ui_font_family.as_str()) });
    // The chrome geometry (bar/strip/pill heights, paddings) — the SAME metrics
    // the app derives from its window's DPI × UI font.
    let cm = jetty_render::ChromeMetrics::new(dpi, ui_font_size);
    // The Settings panel draws with its OWN layer at the CAPPED body size, like
    // the app's settings window (the panel body font is clamped to [13, 17] pt).
    let panel_font = ui_font_size.clamp(13.0, 17.0);
    let panel_cm = jetty_render::ChromeMetrics::new(dpi, panel_font);
    let mut panel_text =
        TextLayer::new_with_family(&device, &queue, format, panel_font * dpi, &font_family);
    panel_text.set_ui_family(if ui_font_family.is_empty() { None } else { Some(ui_font_family.as_str()) });
    let (cell_w, cell_h) = text.cell_size();
    let mono_families = text.monospace_families();
    // UI-font candidates with the synthetic "System Sans (default)" row at index 0.
    let ui_families: Vec<String> = std::iter::once("System Sans (default)".to_string())
        .chain(chrome_text.proportional_families())
        .collect();
    eprintln!("jetty-shot: {} monospace families found (e.g. {:?})", mono_families.len(), mono_families.first());

    // JETTY_SHOT_DETACHED=1 — render the DETACHED-window chrome: the top bar
    // (title pill + close ✕), the grid offset below it, and the bottom status
    // strip, mirroring App::render_detached_window for visual verification.
    let detached_shot = std::env::var("JETTY_SHOT_DETACHED").map(|v| v != "0").unwrap_or(false);
    // JETTY_SHOT_TABBAR lays the grid out like the live main window: below the
    // bar (or above it with JETTY_TAB_BAR=bottom), above the status strip when a
    // perf HUD is shown — so a bar that overflows its band is visible in the PNG.
    let tabbar_shot = env_flag("JETTY_SHOT_TABBAR");
    let tab_bar_bottom = std::env::var("JETTY_TAB_BAR").map(|v| v == "bottom").unwrap_or(false);
    let perf_shot = std::env::var("JETTY_SHOT_PERF").map(|v| !v.is_empty()).unwrap_or(false);
    let shot_grid_top: f32 =
        if detached_shot || (tabbar_shot && !tab_bar_bottom) { cm.bar_h() } else { 0.0 };
    let shot_status_h: f32 =
        if detached_shot || (tabbar_shot && perf_shot) { cm.status_h() } else { 0.0 };
    // The bar's height when it sits at the BOTTOM (the grid ends above it).
    let shot_bottom_bar_h: f32 = if tabbar_shot && tab_bar_bottom { cm.bar_h() } else { 0.0 };

    // The grid's inner padding (JETTY_SHOT_PADDING="x,y", logical px; default
    // = the config defaults), scaled to whole physical px like the app.
    let (pad_lx, pad_ly) = std::env::var("JETTY_SHOT_PADDING")
        .ok()
        .and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        })
        .unwrap_or_else(jetty_app::default_grid_padding);
    let (pad_x, pad_y) = (jetty_render::padding_px(pad_lx, dpi), jetty_render::padding_px(pad_ly, dpi));
    // Cell (0, 0): the left padding, and the band top plus the top padding.
    let shot_origin = jetty_render::GridOrigin::new(pad_x, shot_grid_top + pad_y);

    // Reserve the scrollbar gutter EXACTLY like the live windows
    // (app.rs main_grid_dims_at / detached.rs), so a screenshot builds the same
    // column count the user sees and never lays text UNDER the drawn scrollbar
    // (F22): DPI-scaled, none under JETTY_SHOT_SCROLLBAR=never.
    let scrollbar_mode =
        jetty_app::ScrollbarMode::parse(&std::env::var("JETTY_SHOT_SCROLLBAR").unwrap_or_default());
    let scrollbar_gutter =
        if scrollbar_mode.has_gutter() { jetty_render::scrollbar_gutter_px(dpi) } else { 0.0 };
    let band_h = height as f32 - shot_grid_top - shot_status_h - shot_bottom_bar_h;
    let (cols, rows) = jetty_render::grid_dims(width as f32, band_h, cell_w, cell_h, scrollbar_gutter, pad_x, pad_y);

    eprintln!(
        "jetty-shot: grid = {cols}x{rows} cells (cell {cell_w:.2}x{cell_h:.1}px, origin {:.0},{:.0}, padding {pad_x}x{pad_y}px)",
        shot_origin.left, shot_origin.top,
    );

    // --- Build terminal snapshot ---
    // Terminal::new picks up JETTY_THEME and JETTY_OPACITY from the environment.
    let mut terminal = jetty_core::Terminal::new(cols, rows);
    // JETTY_SHOT_CURSOR — the `[cursor]` table (see the header).
    let shot_cursor = jetty_app::motion::parse_cursor_spec(&std::env::var("JETTY_SHOT_CURSOR").unwrap_or_default());
    terminal.set_default_cursor_shape(shot_cursor.shape.terminal_shape());
    // Push the real cell metrics so a fed sixel (JETTY_SHOT_SIXEL) reserves the
    // correct row footprint — the shot's analogue of App::reflow's set_cell_px.
    terminal.set_cell_px(cell_w, cell_h);
    // JETTY_SHOT_MIN_CONTRAST=<ratio> — the `minimum_contrast` config key.
    if let Some(r) = std::env::var("JETTY_SHOT_MIN_CONTRAST").ok().and_then(|v| v.parse::<f32>().ok()) {
        terminal.set_minimum_contrast(r);
        eprintln!("jetty-shot: minimum_contrast {:.2}", terminal.minimum_contrast());
    }
    // Config `bold_is_bright` (default off), like the app applies at tab spawn.
    terminal.set_bold_is_bright(env_flag("JETTY_SHOT_BOLD_BRIGHT"));

    if env_flag("JETTY_SHOT_PTY") {
        // Drive a REAL shell offscreen so we can see the live startup prompt
        // (e.g. zsh+p10k) settle exactly as in the running app. This feeds the
        // shell's output into the terminal and writes the terminal's query
        // replies (DSR/DA/etc.) back to the PTY, which is what clears the
        // startup red "x".
        use std::io::Write;
        let pty = jetty_core::PtySession::spawn(
            cols as u16,
            rows as u16,
            (cols as f32 * cell_w).min(65535.0) as u16,
            (rows as f32 * cell_h).min(65535.0) as u16,
            None,
            None,
            || {},
        )?;
        let mut w = pty.writer();

        // ~3.5s startup settle: 700 iterations of 5ms.
        // Short sleep = replies go out within ~5ms of each query, well inside
        // p10k's capability-probe timeouts (mirrors the fixed live-app latency).
        for _ in 0..700 {
            while let Some(chunk) = pty.try_recv_output() {
                terminal.feed(&chunk);
            }
            let replies = terminal.drain_pty_writes();
            if !replies.is_empty() {
                w.write_all(&replies).ok();
                w.flush().ok();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Final drain to capture anything emitted during the last sleep.
        while let Some(chunk) = pty.try_recv_output() {
            terminal.feed(&chunk);
        }
        eprintln!("jetty-shot: JETTY_SHOT_PTY mode drove a real shell for ~3.5s");

        // Optional: inject a command into the live shell after startup settles.
        // Writes the command + newline to the PTY, then runs the SAME tight
        // drain/respond loop for another ~3.5s so the command executes and the
        // prompt fully redraws (including p10k's post-command queries) before we
        // snapshot.
        if let Ok(cmd) = std::env::var("JETTY_SHOT_PTY_CMD") {
            w.write_all(cmd.as_bytes()).ok();
            w.write_all(b"\n").ok();
            w.flush().ok();
            eprintln!("jetty-shot: injected JETTY_SHOT_PTY_CMD={cmd:?}");

            // ~3.5s: 700 iterations of 5ms — tight loop so replies are prompt.
            for _ in 0..700 {
                while let Some(chunk) = pty.try_recv_output() {
                    terminal.feed(&chunk);
                }
                let replies = terminal.drain_pty_writes();
                if !replies.is_empty() {
                    w.write_all(&replies).ok();
                    w.flush().ok();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Final drain after the command loop.
            while let Some(chunk) = pty.try_recv_output() {
                terminal.feed(&chunk);
            }
            eprintln!("jetty-shot: ran injected command for ~3.5s");
        }
    } else {
        terminal.feed(&input_bytes);
    }

    // JETTY_SHOT_OSC133 — headless self-test of the OSC 133 pipeline: feed a
    // scripted A/C/D;<exit> sequence (a FAILED command then a passing one) so the
    // rendered PNG shows the themed left-edge failed-command marker on the failed
    // prompt's row. Combine with JETTY_SHOT_SCROLL to verify the marker tracks
    // the correct viewport row after the mark scrolls into history.
    if env_flag("JETTY_SHOT_OSC133") {
        // Failed command on the current prompt row → renders the marker.
        terminal.feed(b"\x1b]133;A\x07$ false\x1b]133;C\x07\r\n\x1b]133;D;1\x07");
        // A passing command below it (D;0) → no marker (contrast check).
        terminal.feed(b"\x1b]133;A\x07$ echo ok\x1b]133;C\x07\r\nok\r\n\x1b]133;D;0\x07");
        // A third, failed again, so the shot shows two states cleanly.
        terminal.feed(b"\x1b]133;A\x07$ grep nope\x1b]133;C\x07\r\ngrep: nope: no match\r\n\x1b]133;D;2\x07");
        eprintln!("jetty-shot: JETTY_SHOT_OSC133 fed a scripted A/C/D failed+ok+failed sequence");
    }

    // JETTY_SHOT_SIXEL — headless self-test of the inline-image pipeline: feed a
    // sixel DCS, so the rendered PNG shows a real bitmap drawn over the grid
    // (scanner → decode → cell reservation → placement → ImageLayer). Set to "1"
    // for a built-in RGB-stripe pattern, or to a raw sixel BODY (the bytes AFTER
    // `q`, BEFORE the ST) to render a custom image. Combine with JETTY_SHOT_SCROLL
    // to verify the image tracks the correct viewport row into history.
    if let Ok(val) = std::env::var("JETTY_SHOT_SIXEL") {
        if !val.is_empty() && val != "0" {
            // A 30×24 image: four 6-px bands of red/green/blue vertical stripes.
            const BUILTIN_BAND: &str =
                "#0;2;100;0;0#0!10~#1;2;0;100;0#1!10~#2;2;0;0;100#2!10~";
            let body = if val == "1" {
                format!("{BUILTIN_BAND}-{BUILTIN_BAND}-{BUILTIN_BAND}-{BUILTIN_BAND}")
            } else {
                val
            };
            let mut bytes = b"\x1bPq".to_vec();
            bytes.extend_from_slice(body.as_bytes());
            bytes.extend_from_slice(b"\x1b\\"); // 7-bit ST
            terminal.feed(&bytes);
            // Report each decoded image: native WxH, cell footprint, anchor row —
            // so a harness can assert without pixel diffing.
            let imgs = terminal.visible_images();
            eprintln!("jetty-shot: JETTY_SHOT_SIXEL fed a sixel → {} image(s)", imgs.len());
            for vi in &imgs {
                eprintln!(
                    "jetty-shot:   image {}x{}px, footprint {}x{} cells, top viewport row {}",
                    vi.px_w, vi.px_h, vi.cols, vi.rows, vi.top_row
                );
            }
        }
    }

    // JETTY_SHOT_KITTY — headless self-test of the Kitty graphics pipeline (scanner
    // → base64 → decode → cell reservation → placement → ImageLayer). Mirrors
    // JETTY_SHOT_SIXEL. Set to:
    //   "1"   → a built-in 16×16 four-quadrant RGBA image (f=32, a=T).
    //   "png" → a built-in 2×2 RGBA PNG fixture (exercises the f=100 path).
    //   <raw> → a raw APC BODY (bytes AFTER `ESC _ G`, before ST) for custom cases.
    if let Ok(val) = std::env::var("JETTY_SHOT_KITTY") {
        if !val.is_empty() && val != "0" {
            // Standard base64 encoder (no external dep).
            fn b64(data: &[u8]) -> String {
                const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut s = String::new();
                for chunk in data.chunks(3) {
                    let b0 = chunk[0];
                    let b1 = *chunk.get(1).unwrap_or(&0);
                    let b2 = *chunk.get(2).unwrap_or(&0);
                    let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
                    s.push(A[((n >> 18) & 63) as usize] as char);
                    s.push(A[((n >> 12) & 63) as usize] as char);
                    s.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
                    s.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
                }
                s
            }

            let body = if val == "1" {
                // 16×16, four opaque color quadrants (red / green / blue / yellow).
                let (w, h) = (16u32, 16u32);
                let mut px = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        let c = match (x < w / 2, y < h / 2) {
                            (true, true) => [220, 40, 40],
                            (false, true) => [40, 200, 40],
                            (true, false) => [40, 80, 220],
                            (false, false) => [220, 200, 40],
                        };
                        px.extend_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                format!("a=T,f=32,s={w},v={h};{}", b64(&px))
            } else if val == "png" {
                // A 2×2 RGBA PNG fixture encoded on the fly (f=100 path).
                let mut png_bytes = Vec::new();
                {
                    let mut enc = png::Encoder::new(&mut png_bytes, 2, 2);
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    let mut w = enc.write_header().unwrap();
                    let data = [
                        220, 40, 40, 255, 40, 200, 40, 255, 40, 80, 220, 255, 220, 200, 40, 255,
                    ];
                    w.write_image_data(&data).unwrap();
                }
                format!("a=T,f=100;{}", b64(&png_bytes))
            } else {
                val
            };

            let mut bytes = b"\x1b_G".to_vec();
            bytes.extend_from_slice(body.as_bytes());
            bytes.extend_from_slice(b"\x1b\\"); // 7-bit ST
            terminal.feed(&bytes);
            let imgs = terminal.visible_images();
            eprintln!("jetty-shot: JETTY_SHOT_KITTY fed a Kitty APC → {} image(s)", imgs.len());
            for vi in &imgs {
                eprintln!(
                    "jetty-shot:   image {}x{}px, footprint {}x{} cells, col {}, top viewport row {}",
                    vi.px_w, vi.px_h, vi.cols, vi.rows, vi.col, vi.top_row
                );
            }
        }
    }

    // Optional: scroll the view before snapshotting (JETTY_SHOT_SCROLL, i32, positive = up).
    if let Ok(scroll_str) = std::env::var("JETTY_SHOT_SCROLL") {
        if let Ok(n) = scroll_str.parse::<i32>() {
            if n != 0 {
                terminal.scroll_lines(n);
                eprintln!("jetty-shot: scrolled {} lines (positive=up into history)", n);
            }
        }
    }

    // JETTY_SHOT_SEARCH — scrollback-search self-test: set the query on the
    // terminal (incremental smart-case literal search; auto-scrolls to the
    // current match), mirroring what Ctrl+Shift+F + typing does in the app.
    let search_query = std::env::var("JETTY_SHOT_SEARCH").ok().filter(|s| !s.is_empty());
    if let Some(q) = &search_query {
        let (cur, total) = terminal.search_set_query(q);
        eprintln!("jetty-shot: JETTY_SHOT_SEARCH={q:?} → match {cur}/{total}");
    }

    // JETTY_SHOT_LINK_HOVER="row,col" — Ctrl+hover link self-test: report the
    // link under that 0-based viewport cell on stderr (so a harness can assert
    // the matched URL) and draw the SAME themed underline quads the app draws.
    let link_hit: Option<jetty_core::LinkHit> =
        std::env::var("JETTY_SHOT_LINK_HOVER").ok().and_then(|s| {
            let mut it = s.splitn(2, ',');
            let row = it.next()?.trim().parse::<usize>().ok()?;
            let col = it.next()?.trim().parse::<usize>().ok()?;
            let hit = terminal.link_at(row, col);
            eprintln!(
                "jetty-shot: link_at({row},{col}) = {:?}",
                hit.as_ref().map(|h| &h.uri)
            );
            hit
        });

    // JETTY_SHOT_COPYMODE="row,col" — keyboard copy-mode self-test: place the
    // copy-mode cursor at (row,col). When JETTY_SHOT_COPYMODE_ANCHOR="row,col" is
    // also set, drive a live selection from the anchor to the cursor (char mode,
    // or whole-line when JETTY_SHOT_COPYMODE_LINE=1) using the DERIVED sub-cell
    // sides (reading-order start=Left, end=Right) — so the selection tint renders
    // through the REAL cell_bg_rects path (snapshot below), exactly like the app.
    let parse_rc = |s: &str| -> Option<(usize, usize)> {
        let mut it = s.splitn(2, ',');
        let r = it.next()?.trim().parse().ok()?;
        let c = it.next()?.trim().parse().ok()?;
        Some((r, c))
    };
    let copymode_cursor: Option<(usize, usize, jetty_render::CopySelect)> = std::env::var("JETTY_SHOT_COPYMODE")
        .ok()
        .and_then(|s| parse_rc(&s))
        .map(|(r, c)| {
            let line_mode = env_flag("JETTY_SHOT_COPYMODE_LINE");
            let block_mode = env_flag("JETTY_SHOT_COPYMODE_BLOCK");
            let mut select = jetty_render::CopySelect::None;
            if let Some(a) = std::env::var("JETTY_SHOT_COPYMODE_ANCHOR").ok().and_then(|s| parse_rc(&s)) {
                let cursor = (r, c);
                if line_mode {
                    select = jetty_render::CopySelect::Lines;
                    let (sr, er) = if cursor >= a { (a.0, cursor.0) } else { (cursor.0, a.0) };
                    terminal.selection_start_lines(sr);
                    terminal.selection_update(er, c, false);
                } else if block_mode {
                    // The app's own block path (copymode::block_sides).
                    select = jetty_render::CopySelect::Block;
                    let (a_left, c_left) = jetty_app::copy_mode_block_sides(a.1, c);
                    let top = terminal.viewport_line_to_buffer(0);
                    terminal.selection_start_block_abs(top + a.0 as i32, a.1, a_left);
                    terminal.selection_update_abs(top + r as i32, c, c_left);
                } else {
                    select = jetty_render::CopySelect::Chars;
                    let (start, end) = if cursor >= a { (a, cursor) } else { (cursor, a) };
                    terminal.selection_start(start.0, start.1, true); // Left
                    terminal.selection_update(end.0, end.1, false); // Right
                }
            }
            eprintln!("jetty-shot: JETTY_SHOT_COPYMODE cursor=({r},{c}) select={select:?}");
            (r, c, select)
        });

    // JETTY_SHOT_CLICK_WORD="row,col" / JETTY_SHOT_CLICK_LINE="row" — the
    // selections a double / triple click makes (the app's gridmouse press calls
    // these same Terminal APIs), rendered through the real selection paint.
    if let Some((r, c)) = std::env::var("JETTY_SHOT_CLICK_WORD").ok().and_then(|s| parse_rc(&s)) {
        terminal.selection_start_semantic(r, c);
        eprintln!("jetty-shot: double-click word at ({r},{c}) -> {:?}", terminal.selection_text());
    }
    if let Some(r) = std::env::var("JETTY_SHOT_CLICK_LINE").ok().and_then(|s| s.trim().parse::<usize>().ok()) {
        terminal.selection_start_lines(r);
        eprintln!("jetty-shot: triple-click line {r} -> {:?}", terminal.selection_text());
    }

    let snap = terminal.snapshot();

    let bg_alpha = snap.bg_rgba[3];
    eprintln!(
        "jetty-shot: theme={} bg_rgba={:?} compositing={}",
        terminal.theme().name,
        snap.bg_rgba,
        bg_alpha < 255
    );

    // --- Create offscreen texture ---
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jetty-shot-tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // TEXTURE_BINDING so the Tier-B summon effects (Liquid/Focus) can SAMPLE
        // this rendered frame as their input texture.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut quad = QuadLayer::new(&device, format);
    let mut image_layer = jetty_render::ImageLayer::new(&device, format);
    // The backdrop (JETTY_SHOT_BACKDROP), drawn first in Pass 1 like the app.
    let backdrop = shot_backdrop(&device, &queue, format, width, height, dpi, terminal.theme());

    // --- Pass 1: clear to theme bg + paint per-cell background quads UNDER text ---
    let (cell_w, cell_h) = text.cell_size();
    let selection = jetty_render::selection_paint(terminal.theme());
    // The shell cursor split exactly like the app's render core: the SOLID block
    // goes under the glyphs (its glyph recolored in Pass 2), the thin shapes over
    // them. Focused unless JETTY_SHOT_CURSOR_UNFOCUSED is set, so the harness can
    // screenshot the unfocused-hollow cursor. The shape itself comes from the
    // snapshot (DECSCUSR in the input, e.g. `\e[5 q` for a beam). Copy-mode
    // suppresses the shell cursor (only the keyboard cursor shows).
    let cursor_focused = !env_flag("JETTY_SHOT_CURSOR_UNFOCUSED");
    // JETTY_SHOT_CARET_T=t (0..1) — a caret-flash frame at progress t with the
    // configured flash color (JETTY_SHOT_CARET_COLOR="r,g,b" in 0..1, default
    // white), through the same contrast-safe path as the app.
    let shot_caret_flash: Option<(f32, [f32; 3])> =
        std::env::var("JETTY_SHOT_CARET_T").ok().and_then(|s| s.parse::<f32>().ok()).map(|t| {
            let color = std::env::var("JETTY_SHOT_CARET_COLOR")
                .ok()
                .and_then(|s| {
                    let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    (v.len() == 3).then(|| [v[0], v[1], v[2]])
                })
                .unwrap_or([1.0; 3]);
            (t.clamp(0.0, 1.0), color)
        });
    let shot_cursor_style = shot_cursor.style.placed(Some(text.underline_geom()));
    let cursor = if copymode_cursor.is_none() {
        jetty_render::cursor_draw(
            &snap,
            terminal.theme(),
            cell_w,
            cell_h,
            shot_origin.left,
            shot_origin.top,
            cursor_focused,
            shot_caret_flash,
            &shot_cursor_style,
        )
    } else {
        jetty_render::CursorDraw::default()
    };
    // Grid-space builders (x from the grid's left edge), each moved onto the
    // origin right where it is built — the app's `render_grid_scene` order.
    // The `[cursor] guide` band first (cells, selection and block cover it).
    let mut bg_rects: Vec<jetty_render::Rect> = Vec::new();
    if copymode_cursor.is_none() && shot_cursor.guide.shows(terminal.alt_screen()) {
        bg_rects.extend(jetty_render::cursor_guide_rect(
            &snap,
            terminal.theme(),
            cell_w,
            cell_h,
            0.0,
            shot_origin.top,
            backdrop.is_some(),
        ));
    }
    bg_rects.extend(jetty_render::cell_bg_rects(&snap, cell_w, cell_h, shot_origin.top, selection.bg));
    // The current match's glyph recolor (Pass 2), like the app's render core.
    let mut search_recolor: Vec<(usize, usize, usize, [u8; 3])> = Vec::new();
    if search_query.is_some() {
        // Same pass-1 placement as the app: match tints under the glyphs,
        // appended after the selection rects so they win where overlapping.
        let hits = terminal.search_viewport_hits();
        bg_rects.extend(jetty_render::search_hit_rects(
            &hits,
            cell_w,
            cell_h,
            shot_origin.top,
            terminal.theme(),
        ));
        search_recolor = jetty_render::search_recolor_spans(&hits, terminal.theme());
    }
    jetty_render::shift_x(&mut bg_rects, shot_origin.left);
    bg_rects.extend(cursor.under);

    // --- Pass 2: the grid text on top of the painted background ---
    // The cells carrying combining marks / VS16 / ZWJ, as the app hands them to
    // the renderer (`render_grid_scene`), plus any JETTY_SHOT_GRAPHEMES
    // ="row,col,cluster;…" overrides (e.g. "0,0,e\u{301}" for an NFD é).
    let grapheme_spec = std::env::var("JETTY_SHOT_GRAPHEMES").unwrap_or_default();
    let graphemes: Vec<(usize, usize, &str)> = snap
        .graphemes
        .iter()
        .map(|g| (g.row, g.col, g.text.as_str()))
        .chain(grapheme_spec.split(';').filter_map(|e| {
            let mut it = e.splitn(3, ',');
            Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?, it.next()?))
        }))
        .collect();
    let paint = jetty_render::GridPaint {
        cursor_glyph: cursor.glyph,
        selection: Some(selection),
        graphemes: &graphemes,
        recolor: &search_recolor,
    };
    // Passes 1 + 2 in ONE render pass + submit, exactly like the app's
    // `render_grid_scene`. The clear is the historical premultiplied value: the
    // harness CPU-composites over its own checkerboard, independent of any surface.
    let bg_count = quad.upload(&device, &queue, width, height, &bg_rects);
    text.prepare_grid(&device, &queue, width, height, &snap, shot_origin, &paint)?;
    // JETTY_SHOT_TRAIL="row,col,ms" — the cursor trail (see the header): the
    // app's model simulated from the jump, drawn by the app's pass between the
    // cell backgrounds and the glyphs.
    let shot_trail = std::env::var("JETTY_SHOT_TRAIL").ok().and_then(|spec| {
        let v: Vec<f32> = spec.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        let (row, col, ms) = (*v.first()? as usize, *v.get(1)? as usize, *v.get(2)?);
        let getu = |k: &str, d: u32| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
        let params = jetty_render::TrailParams::new(
            getu("JETTY_SHOT_TRAIL_MS", 200),
            getu("JETTY_SHOT_TRAIL_THRESHOLD", 2),
        );
        let to_rect =
            jetty_render::cursor_trail_rect(&snap, cell_w, cell_h, shot_origin.left, shot_origin.top, cursor_focused, &shot_cursor_style)?;
        let key = [0, 0, 0, 0];
        let from_rect = [shot_origin.col_x(col, cell_w), shot_origin.row_y(row, cell_h), to_rect[2], to_rect[3]];
        let from = jetty_render::TrailPos { key, cell: (row, col), rect: from_rect };
        let to = jetty_render::TrailPos { key, cell: (snap.cursor_row, snap.cursor_col), rect: to_rect };
        let corners = jetty_render::simulate_trail(from, to, std::time::Duration::from_secs_f32(ms / 1000.0), &params);
        eprintln!("jetty-shot: JETTY_SHOT_TRAIL {spec:?} -> corners {corners:?}");
        let color = jetty_render::cursor_colors(&snap, terminal.theme(), shot_cursor_style.color).block;
        corners.map(|c| {
            let layer = jetty_render::CursorTrailLayer::new(&device, format);
            layer.upload(&queue, &jetty_render::TrailUniform::new(width, height, c, to_rect, color));
            layer
        })
    });
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shot-grid") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shot-grid-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(jetty_render::default_bg_clear(&snap, true)),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(bd) = &backdrop {
                bd.draw(&mut pass);
            }
            quad.draw_uploaded(&mut pass, bg_count);
            if let Some(layer) = &shot_trail {
                layer.draw(&mut pass);
            }
            text.draw_grid(&mut pass);
        }
        queue.submit(Some(encoder.finish()));
        text.end_grid_frame();
    }

    // --- Pass 2b: inline (sixel) images over the grid, at native pixel size,
    // scissored to the grid area — the same ImageLayer the live app runs, so the
    // headless PNG exercises the real decode → upload → draw path. ---
    {
        let img_data: Vec<(jetty_core::VisibleImage, std::sync::Arc<jetty_core::SixelImage>)> = terminal
            .visible_images()
            .into_iter()
            .filter_map(|vi| terminal.image_rgba(vi.id).map(|img| (vi, img)))
            .collect();
        let draws: Vec<jetty_render::ImageDraw> = img_data
            .iter()
            .map(|(vi, img)| jetty_render::ImageDraw {
                id: vi.id,
                w: img.width,
                h: img.height,
                rgba: &img.rgba,
                dst: [
                    shot_origin.col_x(vi.col as usize, cell_w),
                    shot_origin.top + vi.top_row * cell_h,
                    vi.px_w as f32,
                    vi.px_h as f32,
                ],
                opacity: 1.0,
            })
            .collect();
        let grid_bottom_px = (height as f32 - shot_status_h - shot_bottom_bar_h).max(0.0);
        let sc_y = shot_origin.top.clamp(0.0, height as f32) as u32;
        let sc_h = (grid_bottom_px.clamp(0.0, height as f32) as u32).saturating_sub(sc_y);
        image_layer.render(&device, &queue, &view, width, height, &draws, [0, sc_y, width, sc_h]);
    }

    // --- Draw scrollbar quad (and optionally the settings panel) over the text ---
    {
        let mut rects: Vec<jetty_render::Rect> = Vec::new();
        // The app's thumb color (one shared definition).
        let sb_thumb = jetty_render::scrollbar_thumb_color(terminal.theme());
        // The grid band: below a top bar, above a bottom bar and the strip.
        let band_bottom = (height as f32 - shot_status_h - shot_bottom_bar_h).max(shot_grid_top);
        let track = jetty_render::ScrollbarTrack::new(width as f32, shot_grid_top, band_bottom, dpi);
        let hover = env_flag("JETTY_SHOT_SCROLLBAR_HOVER");
        if scrollbar_mode.shows_thumb(snap.scroll_offset > 0, false, hover) {
            if let Some(r) = jetty_render::scrollbar_rect(&snap, &track, sb_thumb) {
                rects.push(r);
            }
        }

        // OSC 133 failed-command marker (JETTY_SHOT_OSC133): the same themed
        // left-edge accent bar the app draws, so the headless PNG verifies the
        // whole pipeline (scanner → marks → visible-row mapping → render).
        let failed_rows = terminal.failed_prompt_rows();
        if !failed_rows.is_empty() {
            let bar_w = (3.0 * dpi).round().max(2.0);
            rects.extend(jetty_render::failed_marker_rects(
                &failed_rows,
                cell_h,
                shot_origin.top,
                jetty_render::failed_marker_x(shot_origin.left, bar_w),
                bar_w,
                terminal.theme().failed_marker_color(),
            ));
        }

        // SGR text decorations (underline styles + strike), same as the app's
        // Pass 4 — makes the headless self-test exercise every underline/strike.
        rects.extend_from_slice(text.decoration_rects());

        // JETTY_SHOT_LINK_HOVER underline: the shared link-underline geometry
        // (theme bright blue), identical to the app's Pass 4.
        if let Some(hit) = &link_hit {
            let p12 = terminal.theme().palette[12];
            let mut link = jetty_render::link_underline_rects_at(
                &hit.spans,
                [p12[0], p12[1], p12[2], 255],
                cell_w,
                cell_h,
                text.underline_geom(),
                shot_origin.top,
            );
            jetty_render::shift_x(&mut link, shot_origin.left);
            rects.extend(link);
        }

        // The thin cursor shapes over the glyphs + decorations (the solid block
        // was painted under the text in Pass 1).
        rects.extend(cursor.over);

        // Baseline + color of the live "Aa" UI-font specimen, set when the panel
        // is built.
        let mut ui_specimen_pos: Option<(f32, f32)> = None;
        let mut specimen_rgb = [255u8; 3];
        // The panel's scrolled content — drawn after the chrome, scissored /
        // clipped to its viewport exactly like the app's Settings window.
        let mut panel_content: Option<(Vec<jetty_render::Rect>, Vec<jetty_render::Label>, [u32; 4])> = None;
        let panel_labels = if let Some(cfg) = &shot_cfg {
            use jetty_app::settings_ui as sui;
            let theme_idx = jetty_core::theme_index(&cfg.theme).unwrap_or(0);
            let theme = jetty_core::theme_at(theme_idx);
            let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let mut tab = var("JETTY_SHOT_PANEL_TAB")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0)
                .min(jetty_render::N_TABS - 1);
            let collapsed: Vec<&'static str> = var("JETTY_SHOT_PANEL_COLLAPSE")
                .map(|v| v.split(',').filter_map(|id| sui::section(id.trim()).map(|s| s.id)).collect())
                .unwrap_or_default();
            // A deep link: the control's tab, scrolled to it, highlighted (its
            // section for a master switch or the gallery) and keyboard-focused
            // (ringed) — like App::reveal_setting.
            let linked = var("JETTY_SHOT_PANEL_FOCUS").and_then(|id| sui::find(&id));
            let focus: Option<&'static str> = linked.and_then(|d| sui::link_target(d.id, cfg)).map(|(t, target)| {
                tab = t;
                target
            });
            let filter = match var("JETTY_SHOT_PANEL_FILTER").as_deref() {
                Some("dark") => jetty_render::ThemeFilter::Dark,
                Some("light") => jetty_render::ThemeFilter::Light,
                Some("mine") => jetty_render::ThemeFilter::Mine,
                _ => jetty_render::ThemeFilter::All,
            };
            let hover = var("JETTY_SHOT_PANEL_HOVER").and_then(|h| {
                if h == "reset" {
                    Some(jetty_render::PanelHit::ResetTab)
                } else if let Some(f) = h.strip_prefix("filter:") {
                    jetty_render::ThemeFilter::ALL
                        .into_iter()
                        .find(|x| x.label().eq_ignore_ascii_case(f))
                        .map(jetty_render::PanelHit::GalleryFilter)
                } else if let Some(id) = h.strip_prefix("section:") {
                    sui::section(id).map(|s| jetty_render::PanelHit::Section(s.id))
                } else {
                    jetty_core::theme_index(&h).map(jetty_render::PanelHit::GalleryCard)
                }
            });
            let reset = match var("JETTY_SHOT_PANEL_RESET").as_deref() {
                Some("armed") => jetty_render::ResetState::Armed,
                Some("ready") => jetty_render::ResetState::Ready,
                Some("disabled") => jetty_render::ResetState::Disabled,
                _ if sui::tab_at_defaults(cfg, tab) => jetty_render::ResetState::Disabled,
                _ => jetty_render::ResetState::Ready,
            };
            let footer = match reset {
                jetty_render::ResetState::Armed => "Press again to reset",
                _ if env_flag("JETTY_SHOT_PANEL_SESSION") => "Enter keeps · Esc restores",
                _ => "",
            };
            let backdrop_images = sui::backdrop_images();
            // The font lists open at the configured family, as in the app.
            let font_pos = mono_families.iter().position(|f| *f == cfg.font_family);
            let ui_pos = if cfg.ui_font_family.is_empty() {
                Some(0)
            } else {
                ui_families.iter().position(|f| *f == cfg.ui_font_family)
            };
            let ctx = sui::Ctx {
                main_fullscreen: env_flag("JETTY_SHOT_PANEL_FULLSCREEN"),
                mono_families: &mono_families,
                ui_families: &ui_families,
                font_shown: &cfg.font_family,
                ui_font_shown: &cfg.ui_font_family,
                backdrop_images: &backdrop_images,
                font_offset: sui::list_offset_showing(mono_families.len(), font_pos, sui::list_rows("font_family")),
                ui_font_offset: sui::list_offset_showing(ui_families.len(), ui_pos, sui::list_rows("ui_font_family")),
                collapsed: &collapsed,
                ..sui::Ctx::empty()
            };
            let items = sui::tab_items(tab, cfg, &ctx);
            let mut inp = jetty_render::PanelInput::new(width, height, &theme, panel_cm, &items);
            inp.active_tab = tab;
            // px, "max", or "<n>%" of the tab's scroll range.
            let scroll_env = var("JETTY_SHOT_PANEL_SCROLL").or_else(|| var("JETTY_SHOT_PANEL_FX_SCROLL"));
            let scroll_pct = scroll_env
                .as_deref()
                .and_then(|v| v.strip_suffix('%'))
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 100.0) / 100.0);
            inp.scroll = match scroll_env.as_deref() {
                Some("max") => 1.0e9,
                Some(v) => v.parse::<f32>().unwrap_or(0.0),
                None => 0.0,
            };
            inp.theme_idx = theme_idx;
            inp.filter = filter;
            inp.hover = hover;
            inp.focus = focus;
            inp.focus_part = linked
                .and_then(|d| {
                    let st = sui::stops(&items);
                    sui::stop_of(&st, d.id).or_else(|| focus.and_then(|t| sui::stop_of(&st, t)))
                })
                .and_then(|s| sui::focus_ring(s, &items, theme_idx));
            inp.ui_font_size = ui_font_size;
            inp.reset = reset;
            inp.footer_hint = footer;
            let mut pv = jetty_render::build_panel(&inp, &mut panel_text);
            if let Some(f) = scroll_pct {
                inp.scroll = f * pv.geom.max_scroll;
                pv = jetty_render::build_panel(&inp, &mut panel_text);
            }
            if let Some((top, _)) = focus.and_then(|f| pv.geom.anchor(f)) {
                inp.scroll = (top - 12.0 * panel_cm.overlay_u()).clamp(0.0, pv.geom.max_scroll);
                pv = jetty_render::build_panel(&inp, &mut panel_text);
            }
            rects.extend(pv.quads);
            if let Some(vp) = pv.content_viewport {
                panel_content = Some((pv.content_quads, pv.content_labels, vp));
            }
            // The live "Aa" specimen is drawn at the TRUE UI size via chrome_text
            // (here chrome_text IS at the UI size), so capture its baseline.
            ui_specimen_pos = Some(pv.ui_specimen_pos);
            specimen_rgb = pv.specimen_rgb;
            eprintln!(
                "jetty-shot: settings panel (tab={tab}, theme={}, ui_font={ui_font_size}, scroll={:.0}/{:.0}, {}×{})",
                cfg.theme, pv.geom.scroll, pv.geom.max_scroll, width, height
            );
            pv.labels
        } else {
            Vec::new()
        };

        // Every non-panel chrome label renders through `chrome_text` (the UI-size
        // layer), like the app's main window; the panel's own labels above use
        // the capped `panel_text` layer, like the app's settings window.
        let mut chrome_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
        // Tab titles render in the proportional sans (Family::SansSerif); collect
        // them separately so the harness renders them like the live app does.
        let mut panel_title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
        // Grid-anchored labels (welcome splash, hint chips) render with the
        // TERMINAL (monospace) layer, not chrome_text — they live on the grid.
        let mut welcome_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

        // JETTY_SHOT_SEARCH — the themed search bar (top-right of the grid),
        // built with the SAME builder + args as the app's draw call.
        if let Some(q) = &search_query {
            let (cur, total) = terminal.search_counter();
            let sb = jetty_render::build_search_bar(
                width, shot_grid_top, terminal.theme(), &mut chrome_text, cm, q, cur, total,
            );
            rects.extend(sb.quads);
            chrome_labels.extend(sb.labels);
        }

        // JETTY_SHOT_HINTS — hint-mode label chips over every visible URL / path /
        // git-hash / IPv4, via the SAME scan (Terminal::hint_tokens) + label
        // (assign_labels) + overlay (build_hint_overlay) path the app uses.
        // JETTY_SHOT_HINTS_TYPED sets a partial prefix to verify narrowing (only
        // matching labels are shown, with the typed prefix dimmed).
        if env_flag("JETTY_SHOT_HINTS") {
            let tokens = terminal.hint_tokens();
            let labels = jetty_core::hints::assign_labels(tokens.len());
            let typed = std::env::var("JETTY_SHOT_HINTS_TYPED").unwrap_or_default();
            let labeled: Vec<(String, usize, usize)> = labels
                .iter()
                .zip(tokens.iter())
                .filter(|(l, _)| typed.is_empty() || l.starts_with(&typed))
                .filter_map(|(l, t)| t.spans.first().map(|(r, c, _)| (l.clone(), *r, *c)))
                .collect();
            let refs: Vec<(&str, usize, usize)> =
                labeled.iter().map(|(l, r, c)| (l.as_str(), *r, *c)).collect();
            // Chips are one grid row tall: labels in the TERMINAL font (grid
            // layer + grid-font metrics), exactly like the app.
            let grid_cm = jetty_render::ChromeMetrics::new(dpi, font_size);
            let mut ov = jetty_render::build_hint_overlay(
                &refs, cell_w, cell_h, shot_origin.top, terminal.theme(), &mut text, grid_cm, &typed,
                width.saturating_sub(shot_origin.left as u32),
            );
            jetty_render::shift_x(&mut ov.quads, shot_origin.left);
            jetty_render::shift_labels_x(&mut ov.labels, shot_origin.left);
            rects.extend(ov.quads);
            welcome_labels.extend(ov.labels);
            eprintln!(
                "jetty-shot: JETTY_SHOT_HINTS tokens={} labels_shown={}",
                tokens.len(),
                refs.len()
            );
        }

        // JETTY_SHOT_PREEDIT — an IME composition at the terminal cursor, via the
        // SAME builder the app uses (terminal font through the grid layer).
        if let Ok(p) = std::env::var("JETTY_SHOT_PREEDIT") {
            if let Some(mut ov) = jetty_render::build_preedit_overlay(
                &p, snap.cursor_row, snap.cursor_col, snap.cols, cell_w, cell_h, shot_origin.top,
                terminal.theme(), dpi,
            ) {
                jetty_render::shift_x(&mut ov.quads, shot_origin.left);
                jetty_render::shift_labels_x(&mut ov.labels, shot_origin.left);
                rects.extend(ov.quads);
                welcome_labels.extend(ov.labels);
            }
        }

        // JETTY_SHOT_COPYMODE — the copy-mode keyboard cursor (hollow box) + the
        // "COPY" pill. The selection tint (when an anchor is set) is already drawn
        // by the cell_bg_rects path above (the selection was applied pre-snapshot).
        if let Some((cr, cc, select)) = copymode_cursor {
            let mut copy = jetty_render::copy_cursor_rects(
                &snap, cr, cc, cell_w, cell_h, shot_origin.top, terminal.theme().cursor,
            );
            jetty_render::shift_x(&mut copy, shot_origin.left);
            rects.extend(copy);
            let avoid = jetty_render::PillAvoid {
                snap: &snap,
                origin: shot_origin,
                cell_w,
                cell_h,
                cursor: (cr, cc),
                band_bottom: (height as f32 - shot_status_h - shot_bottom_bar_h).max(shot_grid_top),
            };
            let pill = jetty_render::build_copy_pill(
                width, shot_grid_top, terminal.theme(), &mut chrome_text, cm, select, Some(&avoid),
            );
            rects.extend(pill.quads);
            chrome_labels.extend(pill.labels);
        }

        // Where a shot menu `menu_h` tall opens: a fixed spot, or — JETTY_SHOT_
        // MENU_AT=cursor — where the Menu key opens it, at the text cursor's
        // cell (the app's own anchor rule over the shot's grid origin).
        let menu_at = |menu_h: f32| {
            if std::env::var("JETTY_SHOT_MENU_AT").is_ok_and(|v| v == "cursor") {
                let (row, col) = terminal.cursor_viewport_cell();
                let at = jetty_app::menunav::cursor_anchor(
                    row, col, (cell_w, cell_h), shot_origin, menu_h, height as f32,
                );
                eprintln!("jetty-shot: JETTY_SHOT_MENU_AT=cursor: cell ({row},{col}) -> anchor {at:?}");
                at
            } else {
                (620.0 * dpi, 120.0 * dpi)
            }
        };

        // JETTY_SHOT_MENU — render the right-click context menu for visual checks.
        // JETTY_SHOT_MENU_DISABLED=1 renders the no-selection state: Copy (0)
        // and Run in New Tab (2) dimmed with the hover on an ENABLED row —
        // verifies the grayed-row rendering and the ⇧⌃⏎ hint glyph.
        // JETTY_SHOT_MENU_KEYS moves the highlight the keyboard way.
        if env_flag("JETTY_SHOT_MENU") {
            let disabled: &[usize] =
                if env_flag("JETTY_SHOT_MENU_DISABLED") { &[0, 2] } else { &[] };
            let hover = shot_menu_hover(jetty_render::MENU_ITEMS.len(), disabled, Some(1));
            // Hints from the DEFAULT keymap, derived exactly like the app's.
            let hints = jetty_app::default_context_menu_hints();
            let hint_refs: Vec<&str> = hints.iter().map(String::as_str).collect();
            let (mx, my) = menu_at(jetty_render::context_menu_height(cm));
            let menu = jetty_render::build_context_menu(
                mx, my, width, height, hover, terminal.theme(),
                &mut chrome_text, cm, &hint_refs, disabled,
            );
            rects.extend(menu.quads);
            chrome_labels.extend(menu.labels);
        }

        // JETTY_SHOT_DMENU — render the DETACHED window's 4-item context menu
        // (Reattach / Copy / Paste / Run in New Tab) through the same generic
        // builder the app uses. JETTY_SHOT_DMENU_DISABLED=1 dims Copy (1) +
        // Run in New Tab (3) — the no-selection state.
        if env_flag("JETTY_SHOT_DMENU") {
            let owned = jetty_app::detached_menu_items();
            let items: Vec<(&str, &str)> = owned.iter().map(|(l, h)| (*l, h.as_str())).collect();
            let disabled: &[usize] =
                if env_flag("JETTY_SHOT_DMENU_DISABLED") { &[1, 3] } else { &[] };
            let hover = shot_menu_hover(items.len(), disabled, Some(0));
            let (mx, my) = menu_at(jetty_render::menu_height(items.len(), 0, cm));
            let menu = jetty_render::build_menu(
                mx, my, width, height, hover, terminal.theme(),
                &mut chrome_text, cm, &items, &[], disabled,
            );
            rects.extend(menu.quads);
            chrome_labels.extend(menu.labels);
        }

        // JETTY_SHOT_HELP — render the Keyboard Shortcuts help overlay;
        // JETTY_SHOT_HELP_SCROLL=n scrolls it to row n when its rows overflow.
        if env_flag("JETTY_SHOT_HELP") {
            let scroll: usize = std::env::var("JETTY_SHOT_HELP_SCROLL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let help = jetty_render::build_help_overlay(
                width,
                height,
                terminal.theme(),
                &mut chrome_text,
                cm,
                &jetty_render::default_help_rows(),
                scroll,
            );
            rects.extend(help.quads);
            chrome_labels.extend(help.labels);
        }

        // JETTY_SHOT_PALETTE — render the command palette overlay, driven by the
        // SHARED registry + fuzzy filter path (jetty_app::palette) so the self-test
        // exercises the real code, not a hand-built list. JETTY_SHOT_PALETTE_QUERY
        // sets the typed query (drives the fuzzy highlight); JETTY_SHOT_PALETTE_SEL
        // the selected row index.
        if env_flag("JETTY_SHOT_PALETTE") {
            let query = std::env::var("JETTY_SHOT_PALETTE_QUERY").unwrap_or_default();
            let sel: usize = std::env::var("JETTY_SHOT_PALETTE_SEL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            // A representative registry: the SHARED builder over the live theme
            // list plus two sample tabs, filtered exactly like the app.
            let themes = jetty_core::theme_list();
            let tabs: Vec<(u64, String)> = (1..=2).map(|n| (n, sample_tab_title(n as usize))).collect();
            let registry = jetty_app::palette::build_registry(&themes, &tabs, &[]);
            let hits = jetty_app::palette::filter(&registry, &query);
            // Each row's chord from the default keymap, as the app shows it.
            let hints = jetty_app::palette::chord_hints(&jetty_app::keymap::KeyMap::defaults());
            let total = hits.len();
            let sel = sel.min(total.saturating_sub(1));
            let win = jetty_render::MAX_PALETTE_ROWS;
            let first = if sel >= win { sel + 1 - win } else { 0 };
            let vis: Vec<(String, Vec<usize>, bool, String)> = hits
                .iter()
                .enumerate()
                .skip(first)
                .take(win)
                .map(|(i, h)| {
                    let hint = jetty_app::palette::row_hint(&hints, &h.cmd);
                    (h.title.clone(), h.indices.clone(), i == sel, hint)
                })
                .collect();
            let prows: Vec<jetty_render::PaletteRow> = vis
                .iter()
                .map(|(t, idx, s, hint)| jetty_render::PaletteRow {
                    title: t,
                    match_indices: idx,
                    selected: *s,
                    hint,
                })
                .collect();
            let pal = jetty_render::build_command_palette(
                width, height, terminal.theme(), &mut chrome_text, cm, &query, &prows, total, first,
            );
            rects.extend(pal.quads);
            chrome_labels.extend(pal.labels);
            eprintln!("jetty-shot: JETTY_SHOT_PALETTE query={query:?} sel={sel} rows={total}");
        }

        // JETTY_SHOT_TABBAR — render a sample tab strip (3 tabs, one active, plus
        // the window controls) over the top of the frame so the rounded tabs +
        // borders can be inspected.
        if env_flag("JETTY_SHOT_TABBAR") {
            // Self-test hook: an OSC 0/2 title inside JETTY_SHOT_INPUT (e.g.
            // `\e]2;OSC Title\a`) retitles the first tab, so shell-driven
            // titles can be verified headlessly from the PNG.
            let osc_title = terminal.take_title_update().flatten();
            // …and an OSC 9;4 in the input sets tab 1's progress (real parser).
            let osc_progress = terminal.take_progress_update().flatten();
            // JETTY_SHOT_TABBAR_N — how many sample tabs (default 3; tab 1 active).
            let n_tabs: usize = std::env::var("JETTY_SHOT_TABBAR_N")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(|n: usize| n.clamp(1, 99))
                .unwrap_or(3);
            // JETTY_SHOT_TAB_TITLES — comma list of sample tab titles (empty or
            // missing entries keep "Tab N"; tab 1's OSC title still wins).
            let titles_env = std::env::var("JETTY_SHOT_TAB_TITLES").unwrap_or_default();
            let titles: Vec<&str> = titles_env.split(',').map(str::trim).collect();
            let mut first_title = osc_title;
            let tabs: Vec<(String, bool)> = (0..n_tabs)
                .map(|i| {
                    let t = if i == 0 { first_title.take() } else { None };
                    let named = titles.get(i).filter(|s| !s.is_empty()).map(|s| s.to_string());
                    (t.or(named).unwrap_or_else(|| format!("Tab {}", i + 1)), i == 0)
                })
                .collect();
            // JETTY_SHOT_PERF — render the perf HUD ONLY when a human supplies
            // real, measured numbers (read off the live HUD, same glyph/format).
            // A headless one-shot cannot honestly measure fps/CPU/throughput, so
            // there is NO fabricated fallback: unset/empty ⇒ no HUD is drawn.
            let perf_owned: Option<String> = std::env::var("JETTY_SHOT_PERF")
                .ok()
                .filter(|v| !v.is_empty());
            // Per-tab decoration (badges, progress, colors — see header) and the
            // bar options (style, close buttons, hover, opacity).
            let mut deco = shot_tab_deco(n_tabs);
            if let (Some(p), Some(d)) = (osc_progress, deco.first_mut()) {
                d.progress = Some(p);
            }
            let opts = jetty_render::TabBarOpts { bottom: tab_bar_bottom, ..shot_bar_opts() };
            eprintln!("jetty-shot: tab bar {opts:?} deco {deco:?}");
            let mut bar = jetty_render::build_tab_bar_styled(
                width,
                &tabs,
                terminal.theme(),
                None,
                jetty_render::CtrlHover::None,
                None, // perf HUD now lives in the bottom status bar, not the tab row
                &mut chrome_text,
                cm,
                &deco,
                &opts,
            );
            // JETTY_TAB_BAR=bottom — place the bar at the window bottom, just above
            // the status strip (as the app does). build_tab_bar lays it out at
            // y 0..bar_h; translate it down.
            if tab_bar_bottom {
                let bar_y = (height as f32 - cm.bar_h() - shot_status_h).max(0.0);
                for q in &mut bar.quads {
                    q.y += bar_y;
                }
                for l in &mut bar.labels {
                    l.2 += bar_y;
                }
                for l in &mut bar.title_labels {
                    l.2 += bar_y;
                }
            }
            let tab0 = bar.tab_rects.first().copied();
            rects.extend(bar.quads);
            chrome_labels.extend(bar.labels);
            panel_title_labels.extend(bar.title_labels);

            // JETTY_SHOT_TAB_MENU — the tab context menu (or its color list) just
            // below tab 1, built and decorated exactly as the app does.
            if let (Ok(which), Some(t0)) = (std::env::var("JETTY_SHOT_TAB_MENU"), tab0) {
                let labels: Vec<&str> = if which == "colors" {
                    jetty_app::shot_tab_color_menu_items()
                } else {
                    jetty_app::shot_tab_menu_items(n_tabs >= 2)
                };
                let items: Vec<(&str, &str)> = labels.iter().map(|&l| (l, "")).collect();
                let my = if tab_bar_bottom { (height as f32 - cm.bar_h() - shot_status_h - cm.px(240.0)).max(0.0) } else { cm.bar_h() };
                let hover = shot_menu_hover(
                    labels.len(),
                    &[],
                    std::env::var("JETTY_SHOT_TAB_MENU_HOVER").ok().and_then(|v| v.parse().ok()),
                );
                let mut menu = jetty_render::build_menu(
                    t0.x + cm.px(20.0), my, width, height, hover, terminal.theme(), &mut chrome_text, cm, &items, &[], &[],
                );
                let current = deco.first().and_then(|d| d.color);
                menu.quads.extend(jetty_app::shot_tab_color_swatches(&menu.item_rects, &labels, terminal.theme(), current, cm));
                rects.extend(menu.quads);
                chrome_labels.extend(menu.labels);
                eprintln!("jetty-shot: JETTY_SHOT_TAB_MENU={which} rows={labels:?}");
            }

            // Bottom STATUS BAR (perf HUD, off the tab row) — mirrors the live app.
            if let Some(perf) = perf_owned.as_deref() {
                let strip = jetty_render::build_status_strip(
                    width, (height as f32 - shot_status_h).max(0.0), shot_status_h, Some(perf),
                    terminal.theme(), &mut chrome_text, cm,
                );
                rects.push(strip.quad);
                chrome_labels.extend(strip.label);
            }
            eprintln!(
                "jetty-shot: JETTY_SHOT_TABBAR rendered 3 sample tabs ({})",
                if tab_bar_bottom { "BOTTOM" } else { "top" }
            );
        }

        // JETTY_SHOT_DETACHED — render the DETACHED-window chrome: top bar with
        // the tab title + close ✕ (JETTY_SHOT_DETACHED_HOVER=1 for the red hover
        // state), and the bottom status strip with a sample perf HUD — mirroring
        // App::render_detached_window (the grid above was already offset by
        // TABBAR_H and shortened by the status strip).
        if detached_shot {
            let close_hover =
                std::env::var("JETTY_SHOT_DETACHED_HOVER").map(|v| v != "0").unwrap_or(false);
            let title = std::env::var("JETTY_SHOT_DETACHED_TITLE")
                .unwrap_or_else(|_| "Tab 2".to_string());
            let mut deco = shot_tab_deco(1).first().copied().unwrap_or_default();
            if let Some(p) = terminal.take_progress_update().flatten() {
                deco.progress = Some(p);
            }
            let opts = jetty_render::TabBarOpts { bottom: false, ..shot_bar_opts() };
            let bar = jetty_render::build_detached_bar_styled(
                width, &title, terminal.theme(), close_hover, &mut chrome_text, cm, &deco, &opts,
            );
            // The app draws the bar mid-scene, UNDER this window's overlays (its
            // help / palette dim layers cover it): put its quads first.
            rects.splice(0..0, bar.quads);
            chrome_labels.extend(bar.labels);
            panel_title_labels.extend(bar.title_labels);

            // Bottom STATUS strip (same slim strip as the main window). The perf
            // HUD label is drawn ONLY when JETTY_SHOT_PERF supplies real, measured
            // values — no fabricated fallback (a headless shot cannot honestly
            // measure fps/CPU/throughput). The strip itself always renders.
            let perf = std::env::var("JETTY_SHOT_PERF").ok().filter(|v| !v.is_empty());
            let strip = jetty_render::build_status_strip(
                width, (height as f32 - shot_status_h).max(0.0), shot_status_h, perf.as_deref(),
                terminal.theme(), &mut chrome_text, cm,
            );
            rects.push(strip.quad);
            chrome_labels.extend(strip.label);

            eprintln!("jetty-shot: JETTY_SHOT_DETACHED rendered detached-window chrome (title={title:?}, hover={close_hover})");
        }

        // JETTY_SHOT_WELCOME — render the neofetch-style welcome splash overlay
        // (ASCII logo + info rows + 16-color swatch + tip) so the logo legibility
        // and layout can be eyeballed headlessly.
        if env_flag("JETTY_SHOT_WELCOME") {
            // Terminal (monospace) cell metrics — the welcome renders with the
            // terminal font in the app, so the block-art logo aligns regardless
            // of the UI font. Mirror that here for a faithful screenshot.
            let (wcw, wch) = text.cell_size();
            // Below the prompt, as in the app (the cursor row + 1).
            let prompt_rows = snap.cursor_row.min(snap.rows.saturating_sub(1)) + 1;
            let splash = jetty_render::build_welcome_overlay(
                shot_origin.top + prompt_rows as f32 * wch,
                env!("CARGO_PKG_VERSION"),
                jetty_render::backend_display_name(adapter.get_info().backend),
                &jetty_app::default_welcome_tip(),
                terminal.theme(),
                wcw,
                wch,
            );
            rects.extend(splash.quads);
            welcome_labels.extend(splash.labels);
            eprintln!("jetty-shot: JETTY_SHOT_WELCOME rendered welcome splash");
        }

        // JETTY_SHOT_CONFIRM — render the "Close this tab?" confirmation popup.
        if env_flag("JETTY_SHOT_CONFIRM") {
            let popup = jetty_render::build_confirm_close(
                width, height, &sample_tab_title(2), terminal.theme(), &mut chrome_text, cm,
            );
            rects.extend(popup.quads);
            chrome_labels.extend(popup.labels);
        }

        // JETTY_SHOT_QUIT — render the whole-app "Quit JeTTY?" confirmation popup.
        if env_flag("JETTY_SHOT_QUIT") {
            let popup = jetty_render::build_confirm(
                width, height, "Quit JeTTY? — all tabs will close", terminal.theme(),
                &mut chrome_text, cm,
            );
            rects.extend(popup.quads);
            chrome_labels.extend(popup.labels);
        }

        // JETTY_SHOT_PILL="text" — render the app's toast pill (the run-selection
        // status / Shift-drag hint surface) with the SAME metrics-driven geometry
        // as the main window: centred, above the status strip and a bottom bar.
        if let Ok(msg) = std::env::var("JETTY_SHOT_PILL") {
            if !msg.is_empty() {
                let pill = jetty_render::build_toast_pill(
                    width,
                    height as f32 - shot_status_h - shot_bottom_bar_h - cm.px(14.0),
                    0.0,
                    &msg,
                    terminal.theme(),
                    &mut chrome_text,
                    cm,
                );
                rects.push(pill.quad);
                chrome_labels.push(pill.label);
            }
        }
        // JETTY_SHOT_LINK_HOVER on an OSC 8 link whose text is not its target:
        // the app's target pill (Pass 4c''), where the toast pill sits.
        if let Some(hit) = link_hit.as_ref().filter(|h| h.hidden_target) {
            let pill = jetty_render::build_toast_pill(
                width,
                height as f32 - shot_status_h - shot_bottom_bar_h - cm.px(14.0),
                0.0,
                &hit.uri,
                terminal.theme(),
                &mut chrome_text,
                cm,
            );
            rects.push(pill.quad);
            chrome_labels.push(pill.label);
        }

        quad.render(&device, &queue, &view, width, height, &rects);
        // The Settings content, scissored to its viewport (as in the app).
        if let Some((cq, _, vp)) = &panel_content {
            if !cq.is_empty() {
                quad.render_load_scissored(&device, &queue, &view, width, height, cq, *vp);
            }
        }

        // Render chrome labels on top of the quads: the panel through its capped
        // layer (like the app's settings window), everything else through the
        // UI-size chrome layer — neither scales with the terminal font (this is
        // what proves BUG 1 is fixed across JETTY_FONT_SIZE).
        if !panel_labels.is_empty() {
            panel_text.render_overlays(&device, &queue, &view, width, height, &panel_labels)?;
        }
        if let Some((_, cl, vp)) = &panel_content {
            if !cl.is_empty() {
                let (top, bottom) = (vp[1] as i32, (vp[1] + vp[3]) as i32);
                panel_text.render_overlays_clipped(&device, &queue, &view, width, height, cl, top, bottom)?;
            }
        }
        if !chrome_labels.is_empty() {
            chrome_text.render_overlays(&device, &queue, &view, width, height, &chrome_labels)?;
        }
        if !panel_title_labels.is_empty() {
            chrome_text.render_overlays_sans(&device, &queue, &view, width, height, &panel_title_labels)?;
        }
        // Grid-anchored labels (welcome splash, hint chips): terminal layer.
        if !welcome_labels.is_empty() {
            text.render_overlays(&device, &queue, &view, width, height, &welcome_labels)?;
        }
        // Live "Aa" specimen at the TRUE UI size (chrome_text is at the UI size in
        // the harness), drawn over the capped panel-text pass — mirrors the app's
        // dedicated specimen layer so the panel shot shows the honest preview. Use
        // the TITLE path so the `""` default previews the platform sans.
        if let Some((sx, sy)) = ui_specimen_pos.filter(|p| p.1 < height as f32) {
            // The UiPalette accent (a theme file's `accent` applies), like the app.
            chrome_text.render_overlays_sans(
                &device, &queue, &view, width, height,
                &[("Aa".to_string(), sx, sy, specimen_rgb)],
            )?;
        }
    }

    // --- Window border / focus ring (JETTY_SHOT_WINDOW_BORDER) ---
    // The REAL GPU pass on the scene, BEFORE the corner mask (CPU, after
    // readback) and the CRT pass — the app's order. Radii as the mask's.
    if let Ok(mode) = std::env::var("JETTY_SHOT_WINDOW_BORDER") {
        let focused = !env_flag("JETTY_SHOT_CURSOR_UNFOCUSED");
        let radius = std::env::var("JETTY_CORNER_RADIUS")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|v| v.clamp(0.0, 24.0) * dpi)
            .unwrap_or(0.0);
        let top = if env_flag("JETTY_SHOT_DROPDOWN") { 0.0 } else { radius };
        let tab_color = shot_tab_deco(1).first().and_then(|d| d.color);
        if let Some(c) = jetty_app::shot_ring_color(&mode, focused, terminal.theme(), tab_color) {
            eprintln!("jetty-shot: window border {mode:?} focused={focused} color={c:?} radius={radius}");
            let ring = jetty_render::FocusRing::new(&device, format);
            ring.apply(
                &device, &queue, &view, width, height, [top, top, radius, radius],
                jetty_render::ring_width_px(dpi), [c[0], c[1], c[2], 255],
            );
        } else {
            eprintln!("jetty-shot: window border {mode:?} focused={focused}: no ring");
        }
    }

    // --- Caret glow/ripple (JETTY_SHOT_GLOW_T) ---
    // The REAL pass (the app's variant choice, color and scissor) around the
    // shell cursor, after the overlays — where the app runs it, before the mask.
    if let Some(t) = std::env::var("JETTY_SHOT_GLOW_T").ok().and_then(|s| s.parse::<f32>().ok()) {
        let flash = shot_caret_flash.map(|(_, c)| c).unwrap_or([1.0; 3]);
        let bg = terminal.theme().bg;
        let (light, color, intensity) = jetty_render::caret_glow_look(flash, [bg[0], bg[1], bg[2]]);
        eprintln!("jetty-shot: caret glow (GPU pass, t={t}, light={light}, color={color:?})");
        let mut cfx = jetty_render::CaretFx::new(&device, format);
        cfx.prepare(&device, light);
        cfx.apply(
            &device,
            &queue,
            &view,
            &jetty_render::CaretFxUniform {
                resolution: [width as f32, height as f32],
                cursor_px: [
                    shot_origin.col_x(snap.cursor_col, cell_w) + cell_w * 0.5,
                    shot_origin.row_y(snap.cursor_row, cell_h) + cell_h * 0.5,
                ],
                cell: [cell_w, cell_h],
                t: t.clamp(0.0, 1.0),
                intensity,
                color: [color[0], color[1], color[2], 0.0],
            },
            light,
        );
    }

    // --- Visual bell / command pulse (JETTY_SHOT_BELL / JETTY_SHOT_PULSE) ---
    // The app's edge draw (veil + rims) at a chosen point of each animation,
    // through the real quad veil and rim pass.
    {
        use jetty_app::motion::{edge_draw, CommandPulse, PulseKind, VisualBell, BELL_SECS, PULSE_SECS};
        let now = std::time::Instant::now();
        let ago = |t: f32, secs: f32| now - std::time::Duration::from_secs_f32(t.clamp(0.0, 1.0) * secs);
        let getf = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(d);
        let bell = std::env::var("JETTY_SHOT_BELL").ok().map(|b| VisualBell::parse(&b)).filter(|b| *b != VisualBell::Off);
        let pulse = std::env::var("JETTY_SHOT_PULSE").ok().and_then(|p| match p.as_str() {
            "failure" | "fail" => Some(PulseKind::Failure),
            "success" | "ok" => Some(PulseKind::Success),
            other => CommandPulse::parse(other).pulse_for(Some(1), None, std::time::Duration::ZERO),
        });
        let edge = edge_draw(
            bell.map(|b| (ago(getf("JETTY_SHOT_BELL_T", 0.15), BELL_SECS), b)),
            pulse.map(|k| (ago(getf("JETTY_SHOT_PULSE_T", 0.15), PULSE_SECS), k)),
            &jetty_render::UiPalette::cached(terminal.theme()),
            now,
        );
        if !edge.is_empty() {
            eprintln!("jetty-shot: bell {bell:?} / pulse {pulse:?} → {edge:?}");
            let radius = std::env::var("JETTY_CORNER_RADIUS")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 24.0) * dpi)
                .unwrap_or(0.0);
            if let Some(veil) = edge.veil {
                quad.render(
                    &device, &queue, &view, width, height,
                    &[jetty_render::Rect::new(0.0, 0.0, width as f32, height as f32, veil)],
                );
            }
            if !edge.rims.is_empty() {
                // The app's pass: the focus ring as a soft glow.
                let ring = jetty_render::FocusRing::new(&device, format);
                for spec in &edge.rims {
                    ring.apply_soft(
                        &device, &queue, &view, width, height, [radius; 4], spec.band * dpi, spec.rgba(),
                        jetty_app::motion::RIM_SOFTNESS,
                    );
                }
            }
        }
    }

    // --- Bayer Crystallize summon reveal (JETTY_SHOT_SUMMON_T) ---
    // Run the REAL GPU pass on the offscreen view (not a CPU mirror), so this
    // harness validates the actual pipeline + uniform binding and would catch a
    // shader/binding bug headlessly. Both this and the corner mask are dst-multiply
    // (commutative), so applying it before the CPU corner mask gives the same result.
    if let Some(t) = std::env::var("JETTY_SHOT_SUMMON_T").ok().and_then(|s| s.parse::<f32>().ok()) {
        eprintln!("jetty-shot: applying Bayer crystallize reveal (GPU pass, t={t})");
        let bayer = jetty_render::BayerReveal::new(&device, format);
        bayer.apply(&device, &queue, &view, width, height, t);
    }

    // --- Phosphor Ignition summon reveal (JETTY_SHOT_PHOSPHOR_T) ---
    // Run the REAL GPU pass on the offscreen view so this harness validates the
    // actual two-pass pipeline + 32-byte uniform binding headlessly. Uses the
    // app's accent (UiPalette) and the corner radius (JETTY_CORNER_RADIUS,
    // default 16 for a visible rounded rim) so the rim traces the rounded corners.
    if let Some(t) = std::env::var("JETTY_SHOT_PHOSPHOR_T").ok().and_then(|s| s.parse::<f32>().ok()) {
        let radius = std::env::var("JETTY_CORNER_RADIUS")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(16.0);
        eprintln!("jetty-shot: applying Phosphor Ignition reveal (GPU pass, t={t}, radius={radius})");
        let phosphor = jetty_render::PhosphorIgnition::new(&device, format);
        let a = jetty_render::UiPalette::cached(terminal.theme()).accent;
        let accent = [a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0];
        phosphor.apply(&device, &queue, &view, width, height, radius, t, accent);
    }

    // Rounded-corner radius (JETTY_CORNER_RADIUS) — parsed here because BOTH the
    // CRT pass below (which owns the corners while active, like the live app)
    // and the CPU mask after readback consume it.
    let corner_radius = std::env::var("JETTY_CORNER_RADIUS")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 24.0))
        .unwrap_or(0.0);

    // --- CRT post-process (JETTY_SHOT_CRT / JETTY_SHOT_PRESET / JETTY_SHOT_GLITCH) ---
    // Run the REAL CRT GPU pass onto a SECOND texture, sampling the rendered
    // scene — mirroring the Tier-B sample-then-readback pattern — through the
    // app's own settings path (`effects::frame_settings` → `CrtParams::build`),
    // so the harness captures exactly what the app draws. Like the live app, it
    // runs BEFORE a Tier-B summon effect, which then samples the CRT output (the
    // CRT look stays on through the reveal).
    let shot_post = shot_effects()
        .and_then(|(fx, glitch)| jetty_app::effects::frame_settings(&fx, glitch > 0.0).map(|s| (s, glitch)));
    let crt_tex = if let Some((settings, glitch)) = shot_post {
        let getf = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(d);
        let ct = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jetty-shot-crt"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // TEXTURE_BINDING so a following Tier-B summon effect can sample it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ct_view = ct.create_view(&wgpu::TextureViewDescriptor::default());
        // The CRT pass OWNS the rounded corners while active (the live app
        // skips the corner mask then and feeds the radius to the uniform):
        // default to JETTY_CORNER_RADIUS so the interplay matches the app;
        // JETTY_SHOT_CRT_RADIUS still overrides for isolated experiments. The
        // shot renders a free-floating (non-top-flush) window: all four corners
        // round. The scene clear is premultiplied (see Pass 1).
        let radius = getf("JETTY_SHOT_CRT_RADIUS", corner_radius);
        let theme = terminal.theme();
        let params = jetty_render::CrtParams::build(
            &settings,
            &jetty_render::CrtFrame {
                width,
                height,
                corner_radius: radius,
                corner_radius_top: radius,
                time: getf("JETTY_SHOT_CRT_TIME", 0.0) as f64,
                bg: [theme.bg[0], theme.bg[1], theme.bg[2]],
                fg: theme.fg,
                premultiplied: true,
                srgb: format.is_srgb(),
                dpi_scale: dpi,
                glitch,
            },
        );
        eprintln!("jetty-shot: applying CRT post-process (GPU pass, variant {:#06x})", params.key.bits());
        let crt = jetty_render::Crt::new(&device, format);
        let t_build = std::time::Instant::now();
        crt.prepare(&device, params.key);
        device.poll(wgpu::PollType::wait_indefinitely())?;
        eprintln!("jetty-shot: CRT pipeline build {:.1} ms", t_build.elapsed().as_secs_f64() * 1000.0);
        crt.apply(&device, &queue, &ct_view, &view, &params);
        // JETTY_SHOT_CRT_BENCH=<n>: n more passes in one go (the first above
        // warmed the targets/bind groups), GPU-synchronized.
        if let Some(n) = std::env::var("JETTY_SHOT_CRT_BENCH").ok().and_then(|s| s.parse::<u32>().ok()) {
            device.poll(wgpu::PollType::wait_indefinitely())?;
            let n = n.max(1);
            let t = std::time::Instant::now();
            for _ in 0..n {
                crt.apply(&device, &queue, &ct_view, &view, &params);
            }
            device.poll(wgpu::PollType::wait_indefinitely())?;
            eprintln!(
                "jetty-shot: CRT pass {:.3} ms/pass at {width}x{height} (n={n}, variant {:#06x})",
                t.elapsed().as_secs_f64() * 1000.0 / n as f64,
                params.key.bits()
            );
        }
        Some(ct)
    } else {
        None
    };

    // --- Tier-B summon effects (Liquid / Focus / Pop / Glide / Fade) ---
    // These SAMPLE the rendered scene — or, with JETTY_SHOT_CRT, the CRT output
    // of it (the app's order) — and write the result into another texture (a
    // texture can't be sampled and rendered to in the same pass), which is then
    // read back. The REAL GPU passes, so the harness validates the pipelines and
    // texture/sampler bindings headlessly.
    let liquid_t = std::env::var("JETTY_SHOT_LIQUID_T").ok().and_then(|s| s.parse::<f32>().ok());
    let focus_t = std::env::var("JETTY_SHOT_FOCUS_T").ok().and_then(|s| s.parse::<f32>().ok());
    let transform_t = std::env::var("JETTY_SHOT_TRANSFORM_T").ok().and_then(|s| s.parse::<f32>().ok());
    let tier_b_tex = if liquid_t.is_some() || focus_t.is_some() || transform_t.is_some() {
        let tex_b = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jetty-shot-tex-b"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view_b = tex_b.create_view(&wgpu::TextureViewDescriptor::default());
        let crt_view = crt_tex.as_ref().map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        let src = crt_view.as_ref().unwrap_or(&view);
        if let Some(t) = liquid_t {
            eprintln!("jetty-shot: applying LiquidDrop reveal (GPU pass, t={t}, samples frame)");
            let liquid = jetty_render::LiquidDrop::new(&device, format);
            liquid.apply(&device, &queue, &view_b, src, width, height, t);
        } else if let Some(t) = focus_t {
            eprintln!("jetty-shot: applying FocusPull reveal (GPU pass, t={t}, samples frame)");
            let focus = jetty_render::FocusPull::new(&device, format);
            focus.apply(&device, &queue, &view_b, src, width, height, t);
        } else if let Some(t) = transform_t {
            let kind = match std::env::var("JETTY_SHOT_TRANSFORM").unwrap_or_default().as_str() {
                "glide" => jetty_render::TransformKind::Glide,
                "fade" => jetty_render::TransformKind::Fade,
                _ => jetty_render::TransformKind::Pop,
            };
            eprintln!(
                "jetty-shot: applying {kind:?} transform (GPU pass, t={t}, params {:?})",
                jetty_render::transform_params(kind, t)
            );
            let tf = jetty_render::SummonTransform::new(&device, format);
            tf.apply(&device, &queue, &view_b, src, width, height, kind, t);
        }
        Some(tex_b)
    } else {
        None
    };

    // --- Read back to CPU ---
    // wgpu requires bytes_per_row to be a multiple of 256.
    let unpadded = width * 4;
    let align: u32 = 256;
    let padded = unpadded.div_ceil(align) * align;

    let buffer_size = (padded * height) as u64;
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("jetty-shot-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    // Read back the last pass that ran: the Tier-B effect (it sampled the
    // scene or the CRT output), else the CRT output, else the scene itself.
    let readback_tex = tier_b_tex.as_ref().or(crt_tex.as_ref()).unwrap_or(&texture);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: readback_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback_buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));

    // Map and read the buffer.
    let (tx, rx) = std::sync::mpsc::channel();
    readback_buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;

    let padded_data = readback_buffer.slice(..).get_mapped_range();
    // Strip row padding: copy only the unpadded bytes per row.
    let mut tight: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let row_start = (row * padded) as usize;
        let row_end = row_start + unpadded as usize;
        tight.extend_from_slice(&padded_data[row_start..row_end]);
    }
    drop(padded_data);
    readback_buffer.unmap();

    // --- Rounded-corner alpha mask (JETTY_CORNER_RADIUS) ---
    // Apply the SAME antialiased rounded-rect SDF mask the live GPU pass uses, so
    // the shot shows transparent (rounded) corners over the checkerboard while the
    // center stays intact. The texture is premultiplied alpha, so multiply r/g/b/a
    // by the coverage to keep premultiplication consistent.
    //
    // SKIPPED when the CRT pass ran: CRT owns the rounded corners then (its
    // uniform carried the radius above) — the exact mask/CRT interplay of the
    // live app, for both the main and the detached window.
    // JETTY_SHOT_DROPDOWN — verify Dropdown mode's BOTTOM-only rounding: the two
    // top corners are square (top-flush), only the bottom corners round.
    let dropdown = env_flag("JETTY_SHOT_DROPDOWN");
    // JETTY_SHOT_SLIDE_T — the Dropdown slide-in (see the header): move the
    // content up by the app's ease-out offset (rows above are the cleared bg),
    // then cut the window shape at the moving bottom edge.
    let slide_t = std::env::var("JETTY_SHOT_SLIDE_T").ok().and_then(|s| s.parse::<f32>().ok());
    if let Some(t) = slide_t {
        let t = t.clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let offset = -(height as f32) * (1.0 - eased);
        let shift = (-offset).round() as u32;
        eprintln!("jetty-shot: Dropdown slide t={t} → offset {offset:.1}px (radius {corner_radius})");
        let row = (width * 4) as usize;
        let bg = snap.bg_rgba;
        for y in 0..height {
            let dst = y as usize * row;
            if y + shift < height {
                let src = (y + shift) as usize * row;
                tight.copy_within(src..src + row, dst);
            } else {
                for px in tight[dst..dst + row].chunks_mut(4) {
                    px.copy_from_slice(&[bg[0], bg[1], bg[2], bg[3]]);
                }
            }
        }
        for y in 0..height {
            for x in 0..width {
                let cov = jetty_render::rounded_rect_coverage_slid(
                    x as f32, y as f32, width as f32, height as f32,
                    0.0, 0.0, corner_radius, corner_radius, offset,
                );
                if cov < 1.0 {
                    let idx = ((y * width + x) * 4) as usize;
                    for c in 0..4 {
                        tight[idx + c] = (tight[idx + c] as f32 * cov).round() as u8;
                    }
                }
            }
        }
    }
    if corner_radius > 0.0 && crt_tex.is_none() && slide_t.is_none() {
        let (r_tl, r_tr) = if dropdown { (0.0, 0.0) } else { (corner_radius, corner_radius) };
        eprintln!(
            "jetty-shot: applying rounded-corner mask (radius={corner_radius}px, dropdown={dropdown})"
        );
        for y in 0..height {
            for x in 0..width {
                let cov = jetty_render::rounded_rect_coverage_per(
                    x as f32, y as f32, width as f32, height as f32,
                    r_tl, r_tr, corner_radius, corner_radius,
                );
                if cov < 1.0 {
                    let idx = ((y * width + x) * 4) as usize;
                    for c in 0..4 {
                        tight[idx + c] = (tight[idx + c] as f32 * cov).round() as u8;
                    }
                }
            }
        }
    }

    // (Bayer Crystallize summon reveal is applied earlier via the REAL GPU pass,
    // before readback — see above. No CPU mirror here.)

    // --- Composite over checkerboard if bg alpha < 255 (or a corner radius is set,
    // so the now-transparent corners reveal the checkerboard even on an opaque
    // theme) ---
    // The rendered texture uses premultiplied alpha (the clear color is already
    // premultiplied in text.rs).  We un-premultiply before blending onto the
    // checkerboard, then output an opaque RGBA PNG.
    let summon_active = std::env::var("JETTY_SHOT_SUMMON_T").is_ok()
        || std::env::var("JETTY_SHOT_PHOSPHOR_T").is_ok()
        || std::env::var("JETTY_SHOT_LIQUID_T").is_ok()
        || std::env::var("JETTY_SHOT_FOCUS_T").is_ok()
        || std::env::var("JETTY_SHOT_TRANSFORM_T").is_ok();
    // JETTY_SHOT_UNDERLAY=none keeps the frame's own alpha (straight, not
    // premultiplied) instead of the checkerboard — for compositing the shot
    // over something else, e.g. a desktop behind a summon frame.
    let keep_alpha = std::env::var("JETTY_SHOT_UNDERLAY").is_ok_and(|v| v == "none");
    let composited = if keep_alpha {
        let mut out = tight;
        for px in out.chunks_exact_mut(4) {
            let a = px[3] as f32 / 255.0;
            for c in &mut px[..3] {
                *c = if a > 0.0 { (*c as f32 / a).min(255.0) as u8 } else { 0 };
            }
        }
        out
    } else if bg_alpha < 255 || corner_radius > 0.0 || summon_active || slide_t.is_some() {
        eprintln!("jetty-shot: compositing over checkerboard (bg alpha={})", bg_alpha);
        const TILE: u32 = 16;
        const DARK: [u8; 3] = [40, 40, 40];
        const LIGHT: [u8; 3] = [90, 90, 90];

        let mut out = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let src_r = tight[idx] as f32 / 255.0;
                let src_g = tight[idx + 1] as f32 / 255.0;
                let src_b = tight[idx + 2] as f32 / 255.0;
                let src_a = tight[idx + 3] as f32 / 255.0;

                // Checkerboard background
                let tile_x = x / TILE;
                let tile_y = y / TILE;
                let checker = if (tile_x + tile_y).is_multiple_of(2) { DARK } else { LIGHT };
                let dst_r = checker[0] as f32 / 255.0;
                let dst_g = checker[1] as f32 / 255.0;
                let dst_b = checker[2] as f32 / 255.0;

                // Source is premultiplied alpha: un-premultiply for correct over blend.
                // over(src_premul, dst) = src_premul + dst*(1-alpha)
                let out_r = (src_r + dst_r * (1.0 - src_a)).min(1.0);
                let out_g = (src_g + dst_g * (1.0 - src_a)).min(1.0);
                let out_b = (src_b + dst_b * (1.0 - src_a)).min(1.0);

                out[idx] = (out_r * 255.0) as u8;
                out[idx + 1] = (out_g * 255.0) as u8;
                out[idx + 2] = (out_b * 255.0) as u8;
                out[idx + 3] = 255; // opaque output
            }
        }
        out
    } else {
        tight
    };

    // --- Write PNG ---
    let file = File::create(&out_path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(&composited)?;
    drop(png_writer);

    let file_size = std::fs::metadata(&out_path)?.len();
    println!("wrote {} ({}x{}, {} bytes)", out_path, width, height, file_size);
    if bg_alpha < 255 {
        println!("composited over checkerboard (bg alpha={})", bg_alpha);
    }

    Ok(())
}

/// The tab-bar options from the JETTY_SHOT_TAB_* hooks (see the header).
fn shot_bar_opts() -> jetty_render::TabBarOpts {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    jetty_render::TabBarOpts {
        style: jetty_render::TabStyle::from_config(&var("JETTY_SHOT_TAB_STYLE")),
        close_button: jetty_render::CloseButton::from_config(&var("JETTY_SHOT_TAB_CLOSE")),
        hover: var("JETTY_SHOT_TAB_HOVER").parse().ok(),
        opaque: !env_flag("JETTY_SHOT_TAB_BAR_OPACITY"),
        progress: std::env::var("JETTY_SHOT_PROGRESS_BAR").map(|v| v != "0").unwrap_or(true),
        bottom: false,
    }
}

/// One OSC 9;4 state from JETTY_SHOT_TAB_PROGRESS: `40`, `e30`, `e`, `i`, `p50`.
fn shot_progress(s: &str) -> Option<jetty_core::Progress> {
    use jetty_core::{Progress, ProgressState};
    let s = s.trim();
    let num = |t: &str| t.parse::<u8>().ok().map(|v| v.min(100));
    match s.chars().next()? {
        'i' => Some(Progress { state: ProgressState::Indeterminate, value: None }),
        'e' => Some(Progress { state: ProgressState::Error, value: num(&s[1..]) }),
        'p' => Some(Progress { state: ProgressState::Paused, value: num(&s[1..]) }),
        _ => num(s).map(|v| Progress { state: ProgressState::Normal, value: Some(v) }),
    }
}

/// Per-tab decoration from JETTY_SHOT_TABBAR_ACTIVITY / _TAB_PROGRESS /
/// _TAB_COLORS, `n` entries long.
fn shot_tab_deco(n: usize) -> Vec<jetty_render::TabDeco> {
    let list = |k: &str| -> Vec<String> {
        std::env::var(k).map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_default()
    };
    let (acts, progs, colors) =
        (list("JETTY_SHOT_TABBAR_ACTIVITY"), list("JETTY_SHOT_TAB_PROGRESS"), list("JETTY_SHOT_TAB_COLORS"));
    (0..n)
        .map(|i| jetty_render::TabDeco {
            activity: match acts.get(i).map(|s| s.trim()) {
                Some("output") => jetty_render::TabActivity::Output,
                Some("bell") => jetty_render::TabActivity::Bell,
                Some("done") => jetty_render::TabActivity::Done,
                Some("failed") => jetty_render::TabActivity::Failed,
                _ => jetty_render::TabActivity::None,
            },
            progress: progs.get(i).and_then(|s| shot_progress(s)),
            color: colors.get(i).and_then(|s| s.trim().parse::<u8>().ok()).and_then(jetty_render::valid_tab_color),
        })
        .collect()
}
