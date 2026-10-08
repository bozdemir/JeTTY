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
///   JETTY_SHOT_TABBAR_N — number of sample tabs for JETTY_SHOT_TABBAR (default 3).
///   JETTY_SHOT_HELP_SCROLL — first help row for JETTY_SHOT_HELP when its rows
///                    overflow the window (large UI font / short window).
///   JETTY_SHOT_PILL="text" — draw the app's toast pill (run-selection status /
///                    Shift-drag hint surface) above the status strip.
///   JETTY_SHOT_TABBAR_ACTIVITY — comma list aligned with the 3 sample tabs
///                    (`none|output|bell`, unknown → none), e.g.
///                    `none,output,bell` — draws the activity/bell dots on the
///                    inactive tabs for headless inspection.
///   JETTY_SHOT_DETACHED — "1" renders the DETACHED-window chrome: top bar
///                    (title + ✕), grid offset below it, bottom status strip.
///                    JETTY_SHOT_DETACHED_TITLE / _HOVER tweak title and the
///                    ✕ hover state.
///   JETTY_SHOT_LINK_HOVER — "row,col" (0-based viewport cell): print
///                    `link_at(row,col)` to stderr (URL under that cell, OSC 8
///                    or plain text) and draw the app's themed Ctrl+hover
///                    underline for the hit.
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
///   JETTY_SHOT_GRAPHEMES="row,col,cluster;…" — grapheme-cluster overrides for
///                    the renderer (combining marks / VS16 / ZWJ drawn from the
///                    whole cluster instead of the cell's base char).
///   JETTY_SHOT_COPYMODE="row,col" — copy-mode self-test: draw the keyboard cursor
///                    (hollow box) + the "COPY" pill at (row,col). With
///                    JETTY_SHOT_COPYMODE_ANCHOR="row,col" also drive a live
///                    selection anchor→cursor (JETTY_SHOT_COPYMODE_LINE=1 = whole
///                    lines), rendering the real selection tint.
///
/// If the terminal bg alpha < 255, the rendered image is composited over a
/// checkerboard (alternating 16px squares of [40,40,40] and [90,90,90]) so
/// transparency is visible in the output PNG.
use std::fs::File;
use std::io::BufWriter;

use jetty_render::{QuadLayer, TextLayer};

/// A boolean shot flag is ON only when set to a non-empty value other than "0",
/// matching the JETTY_SHOT_DETACHED / JETTY_SHOT_PANEL_* semantics so a harness
/// can turn a mode off with `=0` instead of the mode staying on for any value.
fn env_flag(k: &str) -> bool {
    std::env::var(k).map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
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
    let width: u32 = std::env::var("JETTY_SHOT_WIDTH").ok().and_then(|s| s.parse().ok()).map(|v: u32| v.clamp(1, 8192)).unwrap_or(1000);
    let height: u32 = std::env::var("JETTY_SHOT_HEIGHT").ok().and_then(|s| s.parse().ok()).map(|v: u32| v.clamp(1, 8192)).unwrap_or(640);
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
    let mut instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))?
        }
    };

    eprintln!(
        "jetty-shot: GPU adapter = {} ({:?})",
        adapter.get_info().name,
        adapter.get_info().backend
    );

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jetty-shot-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
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
    let ui_font_size: f32 = std::env::var("JETTY_SHOT_UI_FONT_SIZE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(10.0, 28.0))
        .unwrap_or(16.0);
    let ui_font_family = std::env::var("JETTY_SHOT_UI_FONT").unwrap_or_default();
    if std::env::var("JETTY_SHOT_UI_FONT_SIZE").is_ok() || std::env::var("JETTY_SHOT_UI_FONT").is_ok() {
        eprintln!("jetty-shot: UI font size={ui_font_size}, family={ui_font_family:?}");
    }

    // --- Build TextLayer ---
    let mut text = TextLayer::new_with_family(&device, &queue, format, font_size * dpi, &font_family);
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
    // (app.rs grid_dims / detached.rs), so a screenshot builds the same column
    // count the user sees and never lays text UNDER the drawn scrollbar (F22).
    // SCROLLBAR_GUTTER = jetty_render::SCROLLBAR_W (14) + 4.
    let scrollbar_gutter = jetty_render::SCROLLBAR_W + 4.0;
    let band_h = height as f32 - shot_grid_top - shot_status_h - shot_bottom_bar_h;
    let (cols, rows) = jetty_render::grid_dims(width as f32, band_h, cell_w, cell_h, scrollbar_gutter, pad_x, pad_y);

    eprintln!(
        "jetty-shot: grid = {cols}x{rows} cells (cell {cell_w:.2}x{cell_h:.1}px, origin {:.0},{:.0}, padding {pad_x}x{pad_y}px)",
        shot_origin.left, shot_origin.top,
    );

    // --- Build terminal snapshot ---
    // Terminal::new picks up JETTY_THEME and JETTY_OPACITY from the environment.
    let mut terminal = jetty_core::Terminal::new(cols, rows);
    // Push the real cell metrics so a fed sixel (JETTY_SHOT_SIXEL) reserves the
    // correct row footprint — the shot's analogue of App::reflow's set_cell_px.
    terminal.set_cell_px(cell_w, cell_h);

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
    let copymode_cursor: Option<(usize, usize, bool, bool)> = std::env::var("JETTY_SHOT_COPYMODE")
        .ok()
        .and_then(|s| parse_rc(&s))
        .map(|(r, c)| {
            let line_mode = env_flag("JETTY_SHOT_COPYMODE_LINE");
            let mut selecting = false;
            if let Some(a) = std::env::var("JETTY_SHOT_COPYMODE_ANCHOR").ok().and_then(|s| parse_rc(&s)) {
                selecting = true;
                let cursor = (r, c);
                if line_mode {
                    let (sr, er) = if cursor >= a { (a.0, cursor.0) } else { (cursor.0, a.0) };
                    terminal.selection_start_lines(sr);
                    terminal.selection_update(er, c, false);
                } else {
                    let (start, end) = if cursor >= a { (a, cursor) } else { (cursor, a) };
                    terminal.selection_start(start.0, start.1, true); // Left
                    terminal.selection_update(end.0, end.1, false); // Right
                }
            }
            eprintln!("jetty-shot: JETTY_SHOT_COPYMODE cursor=({r},{c}) selecting={selecting} line={line_mode}");
            (r, c, selecting, line_mode)
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
    // Grid-space builders (x from the grid's left edge), each moved onto the
    // origin right where it is built — the app's `render_grid_scene` order.
    let (mut cursor_under, mut cursor_over) = if copymode_cursor.is_none() {
        jetty_render::cursor_rects_split(&snap, cell_w, cell_h, shot_origin.top, cursor_focused, None, [0.0, 0.0, 0.0])
    } else {
        (None, Vec::new())
    };
    if let Some(block) = cursor_under.as_mut() {
        block.x += shot_origin.left;
    }
    jetty_render::shift_x(&mut cursor_over, shot_origin.left);
    let mut bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h, shot_origin.top, selection.bg);
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
    bg_rects.extend(cursor_under);

    // --- Pass 2: the grid text on top of the painted background ---
    // JETTY_SHOT_GRAPHEMES="row,col,cluster;…" — grapheme-cluster overrides
    // (e.g. "0,0,e\u{301}" for an NFD é), fed straight to the renderer until the
    // snapshot carries them itself.
    let grapheme_spec = std::env::var("JETTY_SHOT_GRAPHEMES").unwrap_or_default();
    let graphemes: Vec<(usize, usize, &str)> = grapheme_spec
        .split(';')
        .filter_map(|e| {
            let mut it = e.splitn(3, ',');
            Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?, it.next()?))
        })
        .collect();
    let paint = jetty_render::GridPaint {
        cursor_glyph: cursor_under.map(|_| {
            (snap.cursor_row, snap.cursor_col, jetty_render::cursor_text_color(terminal.theme(), snap.cursor_rgb))
        }),
        selection: Some(selection),
        graphemes: &graphemes,
        recolor: &search_recolor,
    };
    // Passes 1 + 2 in ONE render pass + submit, exactly like the app's
    // `render_grid_scene`. The clear is the historical premultiplied value: the
    // harness CPU-composites over its own checkerboard, independent of any surface.
    let bg_count = quad.upload(&device, &queue, width, height, &bg_rects);
    text.prepare_grid(&device, &queue, width, height, &snap, shot_origin, &paint)?;
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
            quad.draw_uploaded(&mut pass, bg_count);
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
        let sb_bg = terminal.theme().bg;
        let sb_fg = terminal.theme().fg;
        let sb_mix = |i: usize| (sb_bg[i] as f32 + (sb_fg[i] as f32 - sb_bg[i] as f32) * 0.35) as u8;
        let sb_thumb = [sb_mix(0), sb_mix(1), sb_mix(2), 210];
        if let Some(r) = jetty_render::scrollbar_rect(&snap, width, height, shot_grid_top, shot_status_h + shot_bottom_bar_h, sb_thumb) {
            rects.push(r);
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
            let mut link = jetty_render::link_underline_rects(
                &hit.spans,
                [p12[0], p12[1], p12[2], 255],
                cell_w,
                cell_h,
                shot_origin.top,
            );
            jetty_render::shift_x(&mut link, shot_origin.left);
            rects.extend(link);
        }

        // The thin cursor shapes over the glyphs + decorations (the solid block
        // was painted under the text in Pass 1).
        rects.extend(cursor_over);

        // Baseline for the live "Aa" UI-font specimen, set when the panel is built.
        let mut ui_specimen_pos: Option<(f32, f32)> = None;
        let shot_panel = std::env::var("JETTY_SHOT_PANEL").unwrap_or_else(|_| "0".to_string());
        let panel_labels = if shot_panel == "1" {
            // Read opacity + theme_idx from env (same vars as the live app).
            let opacity = std::env::var("JETTY_OPACITY")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .map(|v| v.clamp(0.1, 1.0))
                .unwrap_or(1.0);
            let theme_name = std::env::var("JETTY_THEME").unwrap_or_default();
            let theme_idx = jetty_core::theme_index(&theme_name).unwrap_or(0);

            // JETTY_SHOT_PANEL_OFFSET="dx,dy" — two f32 (default "0,0").
            // Lets the caller verify the moveable-dialog path at an offset.
            let (panel_dx, panel_dy) = std::env::var("JETTY_SHOT_PANEL_OFFSET")
                .ok()
                .and_then(|s| {
                    let mut parts = s.splitn(2, ',');
                    let dx = parts.next()?.parse::<f32>().ok()?;
                    let dy = parts.next()?.parse::<f32>().ok()?;
                    Some((dx, dy))
                })
                .unwrap_or((0.0, 0.0));

            let panel_radius = std::env::var("JETTY_CORNER_RADIUS")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 24.0))
                .unwrap_or(10.0);
            let pv = jetty_render::build_panel(
                width, height, opacity, theme_idx, font_size,
                &mono_families,
                mono_families.first().map(String::as_str).unwrap_or(""),
                0,
                panel_radius,
                std::env::var("JETTY_SHOT_PANEL_EFFECT").unwrap_or_else(|_| "Bayer".to_string()).as_str(),
                std::env::var("JETTY_SHOT_PANEL_WINMODE").unwrap_or_else(|_| "Center".to_string()).as_str(),
                if std::env::var("JETTY_TAB_BAR").map(|v| v == "bottom").unwrap_or(false) { "Bottom" } else { "Top" },
                // JETTY_SHOT_PANEL_SCROLLBACK sets the SCROLLBACK LINES band's
                // display value (test-only; defaults to "10k").
                std::env::var("JETTY_SHOT_PANEL_SCROLLBACK")
                    .unwrap_or_else(|_| "10k".to_string())
                    .as_str(),
                std::env::var("JETTY_SHOT_PANEL_DH").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.50),
                std::env::var("JETTY_SHOT_PANEL_DW").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0),
                std::env::var("JETTY_SHOT_PANEL_WINMODE").map(|m| m == "Dropdown").unwrap_or(false),
                // JETTY_SHOT_PANEL_FULLSCREEN=1 — render the panel as if the main
                // window were currently in OS fullscreen, which DIMS the CORNER
                // RADIUS band (the radius is suppressed at display time there).
                // Defaults to the Fullscreen window mode's own answer so
                // `JETTY_SHOT_PANEL_WINMODE=Fullscreen` alone already shows the
                // dimmed state, and can be forced independently to capture the
                // "Center/Dropdown + ad-hoc F11" case.
                env_flag("JETTY_SHOT_PANEL_FULLSCREEN")
                    || std::env::var("JETTY_SHOT_PANEL_WINMODE")
                        .map(|m| m == "Fullscreen")
                        .unwrap_or(false),
                std::env::var("JETTY_SHOT_PANEL_AUTOHIDE").map(|s| s != "0").unwrap_or(true),
                std::env::var("JETTY_SHOT_PANEL_LAUNCH").map(|s| s != "0").unwrap_or(false),
                ui_font_size,
                &ui_families,
                ui_font_family.as_str(),
                0,
                panel_dx,
                panel_dy,
                terminal.theme(),
                &mut panel_text,
                panel_cm,
                // JETTY_SHOT_PANEL_SHELL sets the SHELL band's display name
                // (test-only; defaults to "System default").
                std::env::var("JETTY_SHOT_PANEL_SHELL")
                    .unwrap_or_else(|_| "System default".to_string())
                    .as_str(),
                // RUN & NOTIFY section (Shell tab, v0.15): default representative
                // state for the headless shot (on, all-commands, 10s, no summon).
                &jetty_render::NotifyParams::default(),
                // JETTY_SHOT_PANEL_TAB (0..=4) selects the active settings tab
                // (test-only; defaults to 0 = "Look").
                std::env::var("JETTY_SHOT_PANEL_TAB")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0),
                &jetty_render::EffectsParams::default(),
                // JETTY_SHOT_PANEL_FX_SCROLL — Effects-tab scroll offset in px
                // (test-only; clamped like the app does; default 0 = top).
                std::env::var("JETTY_SHOT_PANEL_FX_SCROLL")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .map(|v| {
                        v.clamp(
                            0.0,
                            (jetty_render::EFFECTS_CONTENT_H - jetty_render::EFFECTS_VISIBLE_H)
                                .max(0.0),
                        )
                    })
                    .unwrap_or(0.0),
                // JETTY_SHOT_PANEL_THEME_OPEN=1 expands the theme dropdown; the
                // scroll offset comes from JETTY_SHOT_PANEL_THEME_SCROLL (test-only).
                std::env::var("JETTY_SHOT_PANEL_THEME_OPEN").map(|s| s != "0").unwrap_or(false),
                std::env::var("JETTY_SHOT_PANEL_THEME_SCROLL")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0),
            );
            rects.extend(pv.quads);
            // Effects-tab content is scissored to the content viewport in the
            // live app. The harness has no per-pass scissor, so clip the quads
            // in software (rect ∩ viewport) and drop out-of-viewport labels —
            // otherwise scrolled-out widgets would leak over the footer/panel
            // edge in shots and misrepresent the real render.
            let mut lab = pv.labels;
            if let Some(vp) = pv.effects_viewport {
                let (vt, vb) = (vp[1] as f32, (vp[1] + vp[3]) as f32);
                for mut q in pv.effects_quads {
                    let top = q.y.max(vt);
                    let bottom = (q.y + q.h).min(vb);
                    if bottom - top > 0.5 {
                        q.h = bottom - top;
                        if (q.y - top).abs() > 0.01 {
                            q.y = top;
                            q.radius = 0.0; // clipped edge: drop rounding
                        }
                        rects.push(q);
                    }
                }
                lab.extend(
                    pv.effects_labels
                        .into_iter()
                        .filter(|l| l.2 >= vt - 1.0 && l.2 + 16.0 <= vb + 1.0),
                );
            } else {
                rects.extend(pv.effects_quads);
                lab.extend(pv.effects_labels);
            }
            // The live "Aa" specimen is drawn at the TRUE UI size via chrome_text
            // (here chrome_text IS at the UI size), so capture its baseline.
            ui_specimen_pos = Some(pv.ui_specimen_pos);
            eprintln!(
                "jetty-shot: panel enabled (opacity={opacity:.2}, theme_idx={theme_idx}, font_size={font_size}, ui_font_size={ui_font_size}, offset=({panel_dx},{panel_dy}))"
            );
            lab
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
        if let Some((cr, cc, selecting, line_mode)) = copymode_cursor {
            let mut copy = jetty_render::copy_cursor_rects(
                cr, cc, cell_w, cell_h, shot_origin.top, terminal.theme().cursor,
            );
            jetty_render::shift_x(&mut copy, shot_origin.left);
            rects.extend(copy);
            let pill = jetty_render::build_copy_pill(
                width, shot_grid_top, terminal.theme(), &mut chrome_text, cm, line_mode, selecting,
            );
            rects.extend(pill.quads);
            chrome_labels.extend(pill.labels);
        }

        // JETTY_SHOT_MENU — render the right-click context menu for visual checks.
        // JETTY_SHOT_MENU_DISABLED=1 renders the no-selection state: Copy (0)
        // and Run in New Tab (2) dimmed with the hover on an ENABLED row —
        // verifies the grayed-row rendering and the ⇧⌃⏎ hint glyph.
        if env_flag("JETTY_SHOT_MENU") {
            let disabled: &[usize] =
                if env_flag("JETTY_SHOT_MENU_DISABLED") { &[0, 2] } else { &[] };
            // Hints from the DEFAULT keymap, derived exactly like the app's.
            let hints = jetty_app::default_context_menu_hints();
            let hint_refs: Vec<&str> = hints.iter().map(String::as_str).collect();
            let menu = jetty_render::build_context_menu(
                620.0 * dpi, 120.0 * dpi, width, height, Some(1), terminal.theme(),
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
            let menu = jetty_render::build_menu(
                620.0 * dpi, 120.0 * dpi, width, height, Some(0), terminal.theme(),
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
            let tabs = vec![(1, "Tab 1".to_string()), (2, "Tab 2".to_string())];
            let registry = jetty_app::palette::build_registry(&themes, &tabs, &[]);
            let hits = jetty_app::palette::filter(&registry, &query);
            let total = hits.len();
            let sel = sel.min(total.saturating_sub(1));
            let win = jetty_render::MAX_PALETTE_ROWS;
            let first = if sel >= win { sel + 1 - win } else { 0 };
            let vis: Vec<(String, Vec<usize>, bool)> = hits
                .iter()
                .enumerate()
                .skip(first)
                .take(win)
                .map(|(i, h)| (h.title.clone(), h.indices.clone(), i == sel))
                .collect();
            let prows: Vec<jetty_render::PaletteRow> = vis
                .iter()
                .map(|(t, idx, s)| jetty_render::PaletteRow {
                    title: t,
                    match_indices: idx,
                    selected: *s,
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
            // JETTY_SHOT_TABBAR_N — how many sample tabs (default 3; tab 1 active).
            let n_tabs: usize = std::env::var("JETTY_SHOT_TABBAR_N")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(|n: usize| n.clamp(1, 99))
                .unwrap_or(3);
            let mut first_title = osc_title;
            let tabs: Vec<(String, bool)> = (0..n_tabs)
                .map(|i| {
                    let t = if i == 0 { first_title.take() } else { None };
                    (t.unwrap_or_else(|| format!("Tab {}", i + 1)), i == 0)
                })
                .collect();
            // JETTY_SHOT_PERF — render the perf HUD ONLY when a human supplies
            // real, measured numbers (read off the live HUD, same glyph/format).
            // A headless one-shot cannot honestly measure fps/CPU/throughput, so
            // there is NO fabricated fallback: unset/empty ⇒ no HUD is drawn.
            let perf_owned: Option<String> = std::env::var("JETTY_SHOT_PERF")
                .ok()
                .filter(|v| !v.is_empty());
            // JETTY_SHOT_TABBAR_ACTIVITY — per-tab activity dots (see header).
            let activity: Vec<jetty_render::TabActivity> =
                std::env::var("JETTY_SHOT_TABBAR_ACTIVITY")
                    .map(|v| {
                        v.split(',')
                            .map(|s| match s.trim() {
                                "output" => jetty_render::TabActivity::Output,
                                "bell" => jetty_render::TabActivity::Bell,
                                _ => jetty_render::TabActivity::None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            if !activity.is_empty() {
                eprintln!("jetty-shot: JETTY_SHOT_TABBAR_ACTIVITY = {activity:?}");
            }
            let mut bar = jetty_render::build_tab_bar_ex(
                width,
                &tabs,
                terminal.theme(),
                None,
                jetty_render::CtrlHover::None,
                None, // perf HUD now lives in the bottom status bar, not the tab row
                &mut chrome_text,
                cm,
                &activity,
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
            rects.extend(bar.quads);
            chrome_labels.extend(bar.labels);
            panel_title_labels.extend(bar.title_labels);

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
            let bar = jetty_render::build_detached_bar(
                width, &title, terminal.theme(), close_hover, &mut chrome_text, cm,
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
                width,
                height,
                shot_origin.top + prompt_rows as f32 * wch,
                env!("CARGO_PKG_VERSION"),
                "Vulkan",
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
                width, height, "Tab 2", terminal.theme(), &mut chrome_text, cm,
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

        quad.render(&device, &queue, &view, width, height, &rects);

        // Render chrome labels on top of the quads: the panel through its capped
        // layer (like the app's settings window), everything else through the
        // UI-size chrome layer — neither scales with the terminal font (this is
        // what proves BUG 1 is fixed across JETTY_FONT_SIZE).
        if !panel_labels.is_empty() {
            panel_text.render_overlays(&device, &queue, &view, width, height, &panel_labels)?;
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
        if let Some((sx, sy)) = ui_specimen_pos {
            let accent = terminal.theme().palette[4];
            chrome_text.render_overlays_sans(
                &device, &queue, &view, width, height,
                &[("Aa".to_string(), sx, sy, [accent[0], accent[1], accent[2]])],
            )?;
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
    // actual two-pass pipeline + 32-byte uniform binding headlessly. Uses a
    // sample accent (the theme's blue) and the corner radius (JETTY_CORNER_RADIUS,
    // default 16 for a visible rounded rim) so the rim traces the rounded corners.
    if let Some(t) = std::env::var("JETTY_SHOT_PHOSPHOR_T").ok().and_then(|s| s.parse::<f32>().ok()) {
        let radius = std::env::var("JETTY_CORNER_RADIUS")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(16.0);
        eprintln!("jetty-shot: applying Phosphor Ignition reveal (GPU pass, t={t}, radius={radius})");
        let phosphor = jetty_render::PhosphorIgnition::new(&device, format);
        let a = terminal.theme().palette[4];
        let accent = [a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0];
        phosphor.apply(&device, &queue, &view, width, height, radius, t, accent);
    }

    // --- Tier-B summon effects (LiquidDrop / FocusPull) ---
    // These SAMPLE the rendered scene (the `texture` above, now also
    // TEXTURE_BINDING-capable) and write the displaced/blurred result into a
    // SECOND output texture (a texture can't be sampled and rendered to in the
    // same pass). When one runs, we read back from `tex_b` instead of `texture`.
    // This runs the REAL GPU pass so the harness validates the actual pipeline +
    // texture/sampler binding headlessly, mirroring the SUMMON/PHOSPHOR hooks.
    let liquid_t = std::env::var("JETTY_SHOT_LIQUID_T").ok().and_then(|s| s.parse::<f32>().ok());
    let focus_t = std::env::var("JETTY_SHOT_FOCUS_T").ok().and_then(|s| s.parse::<f32>().ok());
    let tier_b_tex = if liquid_t.is_some() || focus_t.is_some() {
        let tex_b = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jetty-shot-tex-b"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // TEXTURE_BINDING so a following CRT pass can SAMPLE this Tier-B
            // output (JETTY_SHOT_LIQUID_T/_FOCUS_T + JETTY_SHOT_CRT combined).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view_b = tex_b.create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(t) = liquid_t {
            eprintln!("jetty-shot: applying LiquidDrop reveal (GPU pass, t={t}, samples frame)");
            let liquid = jetty_render::LiquidDrop::new(&device, format);
            liquid.apply(&device, &queue, &view_b, &view, width, height, t);
        } else if let Some(t) = focus_t {
            eprintln!("jetty-shot: applying FocusPull reveal (GPU pass, t={t}, samples frame)");
            let focus = jetty_render::FocusPull::new(&device, format);
            focus.apply(&device, &queue, &view_b, &view, width, height, t);
        }
        Some(tex_b)
    } else {
        None
    };

    // Rounded-corner radius (JETTY_CORNER_RADIUS) — parsed here because BOTH the
    // CRT pass below (which owns the corners while active, like the live app)
    // and the CPU mask after readback consume it.
    let corner_radius = std::env::var("JETTY_CORNER_RADIUS")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 24.0))
        .unwrap_or(0.0);

    // --- CRT post-process (JETTY_SHOT_CRT) ---
    // Run the REAL CRT GPU pass (curvature/scanlines/shadow-mask/bloom/chromatic/
    // vignette) onto a SECOND texture, sampling the rendered scene — mirroring the
    // Tier-B sample-then-readback pattern. Lets the harness capture the CRT look
    // headlessly (e.g. the neofetch hero). Params are env-overridable.
    let crt_tex = if env_flag("JETTY_SHOT_CRT") {
        let getf = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(d);
        let ct = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("jetty-shot-crt"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let ct_view = ct.create_view(&wgpu::TextureViewDescriptor::default());
        // Sample the current scene: the Tier-B output if one ran, else the frame.
        let src_view = match &tier_b_tex {
            Some(t) => t.create_view(&wgpu::TextureViewDescriptor::default()),
            None => texture.create_view(&wgpu::TextureViewDescriptor::default()),
        };
        eprintln!("jetty-shot: applying CRT post-process (GPU pass)");
        let crt = jetty_render::Crt::new(&device, format);
        crt.apply(&device, &queue, &ct_view, &src_view, width, height, &jetty_render::CrtUniform {
            resolution: [width as f32, height as f32],
            curvature: getf("JETTY_SHOT_CRT_CURVATURE", 0.24),
            scanline: getf("JETTY_SHOT_CRT_SCANLINE", 0.55),
            mask: getf("JETTY_SHOT_CRT_MASK", 0.32),
            bloom: getf("JETTY_SHOT_CRT_BLOOM", 0.45),
            chromatic: getf("JETTY_SHOT_CRT_CHROMATIC", 0.22),
            vignette: getf("JETTY_SHOT_CRT_VIGNETTE", 0.45),
            tint: [1.0, 1.0, 1.0, 0.0],
            // The CRT pass OWNS the rounded corners while active (the live app
            // skips the corner mask then and feeds the radius to this uniform):
            // default to JETTY_CORNER_RADIUS so the interplay matches the app;
            // JETTY_SHOT_CRT_RADIUS still overrides for isolated experiments.
            corner_radius: getf("JETTY_SHOT_CRT_RADIUS", corner_radius),
            time: 0.0,
            flags: 0,
            // The shot renders a free-floating (non-top-flush) window look:
            // all four corners round, so the top radius matches the bottom.
            corner_radius_top: getf("JETTY_SHOT_CRT_RADIUS", corner_radius),
        });
        Some(ct)
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
    // Read back the Tier-B effect output when one ran (it sampled `texture` and
    // wrote the displaced/blurred result into its own texture); otherwise the
    // scene texture itself.
    let readback_tex = crt_tex.as_ref().or(tier_b_tex.as_ref()).unwrap_or(&texture);
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
    if corner_radius > 0.0 && crt_tex.is_none() {
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
        || std::env::var("JETTY_SHOT_FOCUS_T").is_ok();
    let composited = if bg_alpha < 255 || corner_radius > 0.0 || summon_active {
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
