use crate::chrome::CHROME_ADVANCE;
use crate::ui_palette::UiPalette;
use crate::Rect;

/// The "JETTY" logo in the ANSI-Shadow block style (full-block + box-drawing
/// glyphs, all present in the bundled Nerd Font). Each string is one line of the
/// art, accent-colored, on the left side of the splash. The previous thin
/// pipe-art style read as garble ("JTTU"); this block wordmark is unambiguous.
const LOGO: [&str; 6] = [
    "     ██╗███████╗████████╗████████╗██╗   ██╗",
    "     ██║██╔════╝╚══██╔══╝╚══██╔══╝╚██╗ ██╔╝",
    "     ██║█████╗     ██║      ██║    ╚████╔╝ ",
    "██   ██║██╔══╝     ██║      ██║     ╚██╔╝  ",
    "╚█████╔╝███████╗   ██║      ██║      ██║   ",
    " ╚════╝ ╚══════╝   ╚═╝      ╚═╝      ╚═╝   ",
];

/// Geometry + draw data for the Welcome splash overlay.
///
/// Unlike Help, the Welcome overlay has NO dim backdrop and NO panel border —
/// it renders directly on the terminal background (top-left of the grid area)
/// to look like inline neofetch output. It is non-interactive and vanishes on
/// the first keypress, mouse click in the grid, or Esc.
pub struct WelcomeOverlay {
    /// Quads in draw order: color swatch squares (16 ANSI colors).
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — logo lines, info rows, tip line.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
}

/// Build the neofetch-style welcome splash overlay.
///
/// Layout (top-left of the grid area, below the tab bar):
///
///  [LOGO lines]     JeTTY  │ <version>
///                   Render │ wgpu · <backend>
///                   ...
///                   [████ 16-color palette swatch]
///                   tip: …
///
/// All coordinates are in physical pixels. `grid_top_px` is the pixel Y of the
/// grid origin (0 when the tab bar is at the bottom, `TABBAR_H` when at top).
/// The overlay is drawn at a fixed inset; it clips gracefully for tiny windows.
/// `tip` is the last line (see [`welcome_tip`]).
///
/// `char_w` / `line_h` are the terminal cell (from `TextLayer::cell_size()`),
/// which the splash is drawn in. Its insets, gaps and swatches are authored
/// for the default cell (MesloLGS NF at 16 px on a 1× display) and scale
/// with the cell, so they keep their proportion to the text on a HiDPI
/// display or at a larger font size.
pub fn build_welcome_overlay(
    grid_top_px: f32,
    version: &str,
    backend: &str,
    tip: &str,
    theme: &jetty_core::Theme,
    char_w: f32,
    line_h: f32,
) -> WelcomeOverlay {
    // Design px → physical px, whole pixels (crisp swatch edges).
    let k = if char_w.is_finite() && char_w > 0.0 { char_w / CHROME_ADVANCE } else { 1.0 };
    let dp = |v: f32| (v * k).round();
    // --- Theme-derived colors (the shared UiPalette, like help.rs) ---
    // Every color follows the active theme, so the splash re-skins itself with
    // every theme — and its dim rows still read on light themes (the fixed
    // 0.35 tip blend did not).
    let ui = UiPalette::cached(theme);
    // Accent (the theme's, else its blue — kept readable) for the logo.
    let accent = ui.accent;
    // Foreground for info values.
    let fg_col = ui.text;
    // Dim foreground for info key labels.
    let dim_col = ui.text_dim;
    // Dimmer still for the tip line.
    let tip_col = ui.text_hint;

    // --- Layout constants (physical px) ---
    // `char_w` / `line_h` are the caller-supplied MONOSPACE terminal cell metrics
    // (advance × line height). The overlay renders with the terminal font — like
    // real neofetch output — so the block-art logo stays column- AND row-aligned
    // regardless of the (possibly proportional) UI/chrome font. Spacing the logo
    // rows by exactly the cell height makes the full-block glyphs tile seamlessly.
    // Top inset from the grid origin.
    let top_inset = dp(20.0);
    // Left inset from the window left edge.
    let left_inset = dp(16.0);

    // Logo dimensions: max chars wide across all LOGO lines.
    let logo_char_w = LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let logo_px_w = logo_char_w as f32 * char_w;

    // Gap between logo block and info column.
    let col_gap = dp(24.0);

    // Info column starts after the logo block.
    let info_x = left_inset + logo_px_w + col_gap;

    // Key label column width: longest key label + a separator " │ " (3 chars).
    let key_labels = ["JeTTY", "Render", "Terminal", "Themes"];
    let key_col_chars = key_labels.iter().map(|k| k.chars().count()).max().unwrap_or(0);
    let sep = " | "; // ASCII pipe separator (portable, no fancy Unicode in all fonts)
    let key_w = (key_col_chars + sep.chars().count()) as f32 * char_w;

    // Value column starts after the key column.
    let val_x = info_x + key_w;

    // Info row values. "Themes" names the active theme and how many there are
    // (built-ins + user themes — the registry the picker lists).
    let info_rows: &[(&str, String)] = &[
        ("JeTTY", version.to_string()),
        ("Render", format!("wgpu · {}", backend)),
        ("Terminal", format!("JeTTY {}", version)),
        ("Themes", format!("{} · {} themes", theme.display_name, jetty_core::theme_count())),
    ];

    // Compute logo block height so we can vertically center the info rows
    // alongside it (or just start them from the top of the logo).
    let logo_h = LOGO.len() as f32 * line_h;
    let info_h = info_rows.len() as f32 * line_h;
    // Vertically center the info block relative to the logo block.
    let info_y_offset = ((logo_h - info_h) / 2.0).max(0.0);

    // Swatch row sits below whichever is taller (logo or info block).
    let content_h = logo_h.max(info_h);
    let swatch_gap = dp(14.0); // gap between content and swatches
    let swatch_y = grid_top_px + top_inset + content_h + swatch_gap;
    let swatch = dp(16.0);
    let swatch_pad = dp(3.0); // spacing between swatches

    // Tip line sits below the swatches.
    let tip_y = swatch_y + swatch + dp(14.0);

    // --- Assemble quads (color swatches only — no dim backdrop or border) ---
    let mut quads: Vec<Rect> = Vec::new();

    // 16 ANSI color swatches — a small row of filled squares showing the theme palette.
    for i in 0..16usize {
        let color = theme.palette[i];
        let x = left_inset + i as f32 * (swatch + swatch_pad);
        quads.push(Rect {
            x,
            y: swatch_y,
            w: swatch,
            h: swatch,
            color: [color[0], color[1], color[2], 220],
            radius: 3.0 * k,
            shear: 0.0,
        });
    }

    // --- Assemble labels ---
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // ASCII logo block (accent-colored).
    let logo_top = grid_top_px + top_inset;
    for (i, line) in LOGO.iter().enumerate() {
        let y = logo_top + i as f32 * line_h;
        labels.push((line.to_string(), left_inset, y, accent));
    }

    // Info rows: key (dim) + separator + value (fg), rendered as a single label
    // each to keep layout predictable (no mid-string color changes needed).
    let info_top = logo_top + info_y_offset;
    for (i, (key, val)) in info_rows.iter().enumerate() {
        let y = info_top + i as f32 * line_h;
        // Key label.
        let padded_key = format!("{:>width$}{}", key, sep, width = key_col_chars);
        labels.push((padded_key, info_x, y, dim_col));
        // Value.
        labels.push((val.clone(), val_x, y, fg_col));
    }

    // Tip line.
    labels.push((tip.to_string(), left_inset, tip_y, tip_col));

    WelcomeOverlay { quads, labels }
}

/// The splash's tip line, naming the command palette's chord as bound
/// (`palette_chord`, e.g. "Ctrl+Shift+P"; "" when unbound — then the ?
/// button's shortcut list, which needs no chord).
pub fn welcome_tip(palette_chord: &str) -> String {
    if palette_chord.is_empty() {
        "tip: the ? button lists every shortcut — or just start typing.".to_string()
    } else {
        format!("tip: {palette_chord} opens the command palette — or just start typing.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests.
    const TEST_CHAR_W: f32 = 9.8;

    #[test]
    fn labels_non_empty() {
        let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        assert!(!w.labels.is_empty(), "welcome overlay must have labels");
    }

    #[test]
    fn swatch_quad_count_is_16() {
        let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        // All quads are swatches (16 ANSI colors).
        assert_eq!(w.quads.len(), 16, "expected exactly 16 swatch quads");
    }

    #[test]
    fn content_includes_jetty() {
        let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("JeTTY"), "welcome overlay must mention JeTTY");
    }

    #[test]
    fn content_includes_tip() {
        let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("tip:"), "welcome overlay must include a tip line");
    }

    #[test]
    fn works_at_small_window() {
        // Should not panic at small sizes; we just clip gracefully.
        let w = build_welcome_overlay(36.0, "0.1.0", "Gl", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        assert_eq!(w.quads.len(), 16);
    }

    #[test]
    fn backend_name_appears_in_render_row() {
        let w = build_welcome_overlay(36.0, "1.2.3", "Metal", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("Metal"), "backend name must appear in Render row");
    }

    #[test]
    fn logo_and_rows_read_on_the_terminal_bg_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let bg = [t.bg[0], t.bg[1], t.bg[2]];
            let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &t, TEST_CHAR_W, 22.0);
            for (text, _, _, c) in &w.labels {
                assert!(cr(*c, bg) >= 3.0, "{}: {text:?} {}", t.name, cr(*c, bg));
            }
        }
    }

    #[test]
    fn themes_row_names_the_active_theme_and_the_count() {
        let t = jetty_core::Theme::by_name("gruvbox_light");
        let w = build_welcome_overlay(36.0, "0.1.0", "Vulkan", "tip: test", &t, TEST_CHAR_W, 22.0);
        let n = jetty_core::theme_count();
        assert!(n >= jetty_core::theme::PRESETS.len());
        let row = format!("Gruvbox Light · {n} themes");
        assert!(w.labels.iter().any(|l| l.0 == row), "missing {row:?}");
    }

    #[test]
    fn version_appears() {
        let w = build_welcome_overlay(36.0, "9.8.7", "Vulkan", "tip: test", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("9.8.7"), "version must appear in welcome");
    }

    /// The splash grows with the terminal cell it is drawn in: on a 2×
    /// display every inset, gap and swatch doubles with the text (the
    /// swatches stayed 16 physical px — half size next to 2× text), and the
    /// default 1× layout is the one it always was.
    #[test]
    fn the_splash_scales_with_the_cell() {
        let one = build_welcome_overlay(30.0, "1.0.0", "Vulkan", "tip: x", &theme(), CHROME_ADVANCE, 21.0);
        let two = build_welcome_overlay(60.0, "1.0.0", "Vulkan", "tip: x", &theme(), 2.0 * CHROME_ADVANCE, 42.0);
        assert_eq!((one.quads[0].x, one.quads[0].w, one.quads[0].h), (16.0, 16.0, 16.0), "1× unchanged");
        let near = |a: f32, b: f32| (b - 2.0 * a).abs() <= 1.0;
        for (a, b) in one.quads.iter().zip(&two.quads) {
            assert!(near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h), "({}, {}) {}×{} vs ({}, {}) {}×{}", a.x, a.y, a.w, a.h, b.x, b.y, b.w, b.h);
        }
        for (a, b) in one.labels.iter().zip(&two.labels) {
            assert!(near(a.1, b.1) && near(a.2, b.2), "{:?}", a.0);
        }
    }

    /// The tip points at things that exist: it used to suggest "help, theme
    /// <name>, bench" — commands JeTTY never had (a shell answers "command
    /// not found") — and now names the command palette's live chord.
    #[test]
    fn the_tip_names_the_palette_chord() {
        assert_eq!(welcome_tip("Ctrl+Shift+P"), "tip: Ctrl+Shift+P opens the command palette — or just start typing.");
        let unbound = welcome_tip("");
        assert!(unbound.starts_with("tip: ") && !unbound.contains("bench") && !unbound.contains("theme <"));
        let w = build_welcome_overlay(0.0, "1", "Vulkan", &welcome_tip("Cmd+Shift+P"), &theme(), TEST_CHAR_W, 22.0);
        assert!(w.labels.iter().any(|l| l.0.contains("Cmd+Shift+P")));
    }
}
