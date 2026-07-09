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
    let mut text = TextLayer::new_with_family(&device, &queue, format, font_size, &font_family);
    // Chrome layer at the UI font size, mirroring the live app: ALL window chrome
    // (tab bar, status bar, context menu, settings panel, help, confirm, welcome)
    // renders through this in the chosen UI family, independent of JETTY_FONT_SIZE.
    // The terminal grid renders through `text` (which scales with the font).
    let mut chrome_text = TextLayer::new_with_family(&device, &queue, format, ui_font_size, &font_family);
    chrome_text.set_ui_family(if ui_font_family.is_empty() { None } else { Some(ui_font_family.as_str()) });
    // Measured chrome-font advance: used by all overlay builders for scale-correct
    // width reservations (HiDPI-aware). On a CI/scale-1 run this is ~9.6–9.8 px.
    let chrome_char_w = chrome_text.cell_size().0;
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
    let shot_grid_top: f32 = if detached_shot { jetty_render::TABBAR_H } else { 0.0 };
    let shot_status_h: f32 = if detached_shot { 22.0 } else { 0.0 };

    // Reserve the scrollbar gutter EXACTLY like the live windows
    // (app.rs grid_dims / detached.rs), so a screenshot builds the same column
    // count the user sees and never lays text UNDER the drawn scrollbar (F22).
    // SCROLLBAR_GUTTER = jetty_render::SCROLLBAR_W (14) + 4.
    let scrollbar_gutter = jetty_render::SCROLLBAR_W + 4.0;
    let cols = ((width as f32 - scrollbar_gutter) / cell_w).floor().max(2.0) as usize;
    let rows = (((height as f32 - shot_grid_top - shot_status_h) / cell_h).floor()).max(1.0) as usize;

    eprintln!("jetty-shot: grid = {cols}x{rows} cells (cell {cell_w:.1}x{cell_h:.1}px)");

    // --- Build terminal snapshot ---
    // Terminal::new picks up JETTY_THEME and JETTY_OPACITY from the environment.
    let mut terminal = jetty_core::Terminal::new(cols, rows);

    if env_flag("JETTY_SHOT_PTY") {
        // Drive a REAL shell offscreen so we can see the live startup prompt
        // (e.g. zsh+p10k) settle exactly as in the running app. This feeds the
        // shell's output into the terminal and writes the terminal's query
        // replies (DSR/DA/etc.) back to the PTY, which is what clears the
        // startup red "x".
        use std::io::Write;
        let pty = jetty_core::PtySession::spawn(cols as u16, rows as u16, None, None, || {})?;
        let mut w = pty.writer();

        // ~3.5s startup settle: 700 iterations of 5ms.
        // Short sleep = replies go out within ~5ms of each query, well inside
        // p10k's capability-probe timeouts (mirrors the fixed live-app latency).
        for _ in 0..700 {
            while let Ok(chunk) = pty.output().try_recv() {
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
        while let Ok(chunk) = pty.output().try_recv() {
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
                while let Ok(chunk) = pty.output().try_recv() {
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
            while let Ok(chunk) = pty.output().try_recv() {
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

    // --- Pass 1: clear to theme bg + paint per-cell background quads UNDER text ---
    let (cell_w, cell_h) = text.cell_size();
    let sel_accent = terminal.theme().palette[4];
    let sel_bg = [
        ((terminal.theme().bg[0] as u16 + sel_accent[0] as u16 * 2) / 3) as u8,
        ((terminal.theme().bg[1] as u16 + sel_accent[1] as u16 * 2) / 3) as u8,
        ((terminal.theme().bg[2] as u16 + sel_accent[2] as u16 * 2) / 3) as u8,
    ];
    let mut bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h, shot_grid_top, sel_bg);
    if search_query.is_some() {
        // Same pass-1 placement as the app: match tints under the glyphs,
        // appended after the selection rects so they win where overlapping.
        bg_rects.extend(jetty_render::search_hit_rects(
            &terminal.search_viewport_hits(),
            cell_w,
            cell_h,
            shot_grid_top,
            terminal.theme(),
        ));
    }
    quad.render_clear(
        &device,
        &queue,
        &view,
        width,
        height,
        &bg_rects,
        // Headless harness CPU-composites over its own checkerboard, which expects
        // the historical premultiplied clear — keep it independent of any surface.
        jetty_render::default_bg_clear(&snap, true),
    );

    // --- Pass 2: render the grid text on top of the painted background (load) ---
    text.render_to(&device, &queue, &view, width, height, &snap, false, shot_grid_top)?;

    // --- Draw scrollbar quad (and optionally the settings panel) over the text ---
    {
        let mut rects: Vec<jetty_render::Rect> = Vec::new();
        let sb_bg = terminal.theme().bg;
        let sb_fg = terminal.theme().fg;
        let sb_mix = |i: usize| (sb_bg[i] as f32 + (sb_fg[i] as f32 - sb_bg[i] as f32) * 0.35) as u8;
        let sb_thumb = [sb_mix(0), sb_mix(1), sb_mix(2), 210];
        if let Some(r) = jetty_render::scrollbar_rect(&snap, width, height, shot_grid_top, shot_status_h, sb_thumb) {
            rects.push(r);
        }

        // OSC 133 failed-command marker (JETTY_SHOT_OSC133): the same themed
        // left-edge accent bar the app draws, so the headless PNG verifies the
        // whole pipeline (scanner → marks → visible-row mapping → render).
        let failed_rows = terminal.failed_prompt_rows();
        if !failed_rows.is_empty() {
            rects.extend(jetty_render::failed_marker_rects(
                &failed_rows,
                cell_h,
                shot_grid_top,
                3.0,
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
            rects.extend(jetty_render::link_underline_rects(
                &hit.spans,
                [p12[0], p12[1], p12[2], 255],
                cell_w,
                cell_h,
                shot_grid_top,
            ));
        }

        // Cursor as quads (drawn over the glyphs + decorations). Focused unless
        // JETTY_SHOT_CURSOR_UNFOCUSED is set, so the harness can screenshot the
        // unfocused-hollow cursor. The shape itself comes from the snapshot
        // (DECSCUSR in the input, e.g. `\e[5 q` for a beam).
        let cursor_focused = !env_flag("JETTY_SHOT_CURSOR_UNFOCUSED");
        rects.extend(jetty_render::cursor_rects(
            &snap,
            cell_w,
            cell_h,
            shot_grid_top,
            cursor_focused,
            None,
            [0.0, 0.0, 0.0],
        ));

        // Baseline for the live "Aa" UI-font specimen, set when the panel is built.
        let mut ui_specimen_pos: Option<(f32, f32)> = None;
        let shot_panel = std::env::var("JETTY_SHOT_PANEL").unwrap_or_else(|_| "0".to_string());
        let mut panel_labels = if shot_panel == "1" {
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
                std::env::var("JETTY_SHOT_PANEL_AUTOHIDE").map(|s| s != "0").unwrap_or(true),
                std::env::var("JETTY_SHOT_PANEL_LAUNCH").map(|s| s != "0").unwrap_or(false),
                ui_font_size,
                &ui_families,
                ui_font_family.as_str(),
                0,
                panel_dx,
                panel_dy,
                terminal.theme(),
                chrome_char_w,
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

        // Tab titles render in the proportional sans (Family::SansSerif); collect
        // them separately so the harness renders them like the live app does.
        let mut panel_title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

        // JETTY_SHOT_SEARCH — the themed search bar (top-right of the grid),
        // built with the SAME builder + args as the app's draw call.
        if let Some(q) = &search_query {
            let (cur, total) = terminal.search_counter();
            let sb = jetty_render::build_search_bar(
                width, shot_grid_top, terminal.theme(), chrome_char_w, q, cur, total,
            );
            rects.extend(sb.quads);
            panel_labels.extend(sb.labels);
        }

        // JETTY_SHOT_MENU — render the right-click context menu for visual checks.
        if env_flag("JETTY_SHOT_MENU") {
            let menu = jetty_render::build_context_menu(620.0, 120.0, width, height, Some(1), terminal.theme(), chrome_char_w);
            rects.extend(menu.quads);
            panel_labels.extend(menu.labels);
        }

        // JETTY_SHOT_HELP — render the Keyboard Shortcuts help overlay.
        if env_flag("JETTY_SHOT_HELP") {
            let help = jetty_render::build_help_overlay(width, height, terminal.theme(), chrome_char_w);
            rects.extend(help.quads);
            panel_labels.extend(help.labels);
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
            let tabs = vec!["Tab 1".to_string(), "Tab 2".to_string()];
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
                width, height, terminal.theme(), chrome_char_w, &query, &prows, total, first,
            );
            rects.extend(pal.quads);
            panel_labels.extend(pal.labels);
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
            let tabs = [
                (osc_title.unwrap_or_else(|| "Tab 1".to_string()), true),
                ("Tab 2".to_string(), false),
                ("Tab 3".to_string(), false),
            ];
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
                chrome_char_w,
                &activity,
            );
            // JETTY_TAB_BAR=bottom — place the bar flush at the window bottom.
            // build_tab_bar lays it out at y 0..TABBAR_H; translate it down.
            let tab_bar_bottom =
                std::env::var("JETTY_TAB_BAR").map(|v| v == "bottom").unwrap_or(false);
            if tab_bar_bottom {
                let bar_y = (height as f32 - jetty_render::TABBAR_H).max(0.0);
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
            panel_labels.extend(bar.labels);
            panel_title_labels.extend(bar.title_labels);

            // Bottom STATUS BAR (perf HUD, off the tab row) — mirrors the live app.
            if let Some(perf) = perf_owned.as_deref() {
                const STATUS_H: f32 = 22.0;
                let sy = (height as f32 - STATUS_H).max(0.0);
                let tb = terminal.theme().bg;
                let tf = terminal.theme().fg;
                let nl = |t: f32| -> [u8; 4] {
                    [
                        (tb[0] as f32 + (tf[0] as f32 - tb[0] as f32) * t) as u8,
                        (tb[1] as f32 + (tf[1] as f32 - tb[1] as f32) * t) as u8,
                        (tb[2] as f32 + (tf[2] as f32 - tb[2] as f32) * t) as u8,
                        255,
                    ]
                };
                rects.push(jetty_render::Rect {
                    x: 0.0, y: sy, w: width as f32, h: STATUS_H, color: nl(0.05), ..Default::default()
                });
                let perf_w = perf.chars().count() as f32 * chrome_char_w;
                let px = (width as f32 - perf_w - 12.0).max(8.0);
                let dim = nl(0.5);
                panel_labels.push((perf.to_string(), px, sy + (STATUS_H - 16.0) / 2.0, [dim[0], dim[1], dim[2]]));
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
                width, &title, terminal.theme(), close_hover, chrome_char_w,
            );
            rects.extend(bar.quads);
            panel_labels.extend(bar.labels);
            panel_title_labels.extend(bar.title_labels);

            // Bottom STATUS strip (same slim strip as the main window). The perf
            // HUD label is drawn ONLY when JETTY_SHOT_PERF supplies real, measured
            // values — no fabricated fallback (a headless shot cannot honestly
            // measure fps/CPU/throughput). The strip itself always renders.
            let perf = std::env::var("JETTY_SHOT_PERF").ok().filter(|v| !v.is_empty());
            let sy = (height as f32 - shot_status_h).max(0.0);
            let tb = terminal.theme().bg;
            let tf = terminal.theme().fg;
            let nl = |t: f32| -> [u8; 4] {
                [
                    (tb[0] as f32 + (tf[0] as f32 - tb[0] as f32) * t) as u8,
                    (tb[1] as f32 + (tf[1] as f32 - tb[1] as f32) * t) as u8,
                    (tb[2] as f32 + (tf[2] as f32 - tb[2] as f32) * t) as u8,
                    255,
                ]
            };
            rects.push(jetty_render::Rect {
                x: 0.0, y: sy, w: width as f32, h: shot_status_h, color: nl(0.05), ..Default::default()
            });
            if let Some(perf) = perf {
                let perf_w = perf.chars().count() as f32 * chrome_char_w;
                let px = (width as f32 - perf_w - 12.0).max(8.0);
                let dim = nl(0.5);
                panel_labels.push((perf, px, sy + (shot_status_h - 16.0) / 2.0, [dim[0], dim[1], dim[2]]));
            }

            eprintln!("jetty-shot: JETTY_SHOT_DETACHED rendered detached-window chrome (title={title:?}, hover={close_hover})");
        }

        // JETTY_SHOT_WELCOME — render the neofetch-style welcome splash overlay
        // (ASCII logo + info rows + 16-color swatch + tip) so the logo legibility
        // and layout can be eyeballed headlessly.
        if env_flag("JETTY_SHOT_WELCOME") {
            let splash = jetty_render::build_welcome_overlay(
                width,
                height,
                jetty_render::TABBAR_H,
                env!("CARGO_PKG_VERSION"),
                "Vulkan",
                terminal.theme(),
                chrome_char_w,
            );
            rects.extend(splash.quads);
            panel_labels.extend(splash.labels);
            eprintln!("jetty-shot: JETTY_SHOT_WELCOME rendered welcome splash");
        }

        // JETTY_SHOT_CONFIRM — render the "Close this tab?" confirmation popup.
        if env_flag("JETTY_SHOT_CONFIRM") {
            let popup = jetty_render::build_confirm_close(width, height, "Tab 2", terminal.theme(), chrome_char_w);
            rects.extend(popup.quads);
            panel_labels.extend(popup.labels);
        }

        // JETTY_SHOT_QUIT — render the whole-app "Quit JeTTY?" confirmation popup.
        if env_flag("JETTY_SHOT_QUIT") {
            let popup = jetty_render::build_confirm(
                width, height, "Quit JeTTY? — all tabs will close", terminal.theme(), chrome_char_w,
            );
            rects.extend(popup.quads);
            panel_labels.extend(popup.labels);
        }

        quad.render(&device, &queue, &view, width, height, &rects);

        // Render chrome (panel/tabbar/menu/help/confirm) labels on top of the
        // quads through the chrome layer (at the UI size), so they don't scale with
        // the terminal font (this is what proves BUG 1 is fixed across JETTY_FONT_SIZE).
        if !panel_labels.is_empty() {
            chrome_text.render_overlays(&device, &queue, &view, width, height, &panel_labels)?;
        }
        if !panel_title_labels.is_empty() {
            chrome_text.render_overlays_sans(&device, &queue, &view, width, height, &panel_title_labels)?;
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
