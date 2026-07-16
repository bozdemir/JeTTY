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
///
/// `char_w` is the measured physical-pixel advance of one chrome-font character
/// (from `TextLayer::cell_size().0`). Pass `9.8` when a real measurement is not
/// available (scale-1 fallback used by tests).
pub fn build_welcome_overlay(
    _win_w: u32,
    _win_h: u32,
    grid_top_px: f32,
    version: &str,
    backend: &str,
    theme: &jetty_core::Theme,
    char_w: f32,
    line_h: f32,
) -> WelcomeOverlay {
    // --- Theme-derived colors (mirrors help.rs / panel.rs) ---
    // All colors blend the active theme's bg→fg so the overlay re-skins itself
    // with every theme instead of being a fixed dark card.
    let tbg = theme.bg;
    let tfg = theme.fg;
    let lerp = |t: f32| -> [u8; 3] {
        [
            (tbg[0] as f32 + (tfg[0] as f32 - tbg[0] as f32) * t).round() as u8,
            (tbg[1] as f32 + (tfg[1] as f32 - tbg[1] as f32) * t).round() as u8,
            (tbg[2] as f32 + (tfg[2] as f32 - tbg[2] as f32) * t).round() as u8,
        ]
    };
    // Accent: palette index 4 (blue-ish in most themes) for the logo + labels.
    let accent = theme.palette[4];
    // Foreground for info values.
    let fg_col = tfg;
    // Dim foreground for info key labels.
    let dim_col = lerp(0.55);
    // Dimmer still for the tip line.
    let tip_col = lerp(0.35);

    // --- Layout constants (physical px) ---
    // `char_w` / `line_h` are the caller-supplied MONOSPACE terminal cell metrics
    // (advance × line height). The overlay renders with the terminal font — like
    // real neofetch output — so the block-art logo stays column- AND row-aligned
    // regardless of the (possibly proportional) UI/chrome font. Spacing the logo
    // rows by exactly the cell height makes the full-block glyphs tile seamlessly.
    // Top inset from the grid origin.
    const TOP_INSET: f32 = 20.0;
    // Left inset from the window left edge.
    const LEFT_INSET: f32 = 16.0;

    // Logo dimensions: max chars wide across all LOGO lines.
    let logo_char_w = LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let logo_px_w = logo_char_w as f32 * char_w;

    // Gap between logo block and info column.
    const COL_GAP: f32 = 24.0;

    // Info column starts after the logo block.
    let info_x = LEFT_INSET + logo_px_w + COL_GAP;

    // Key label column width: longest key label + a separator " │ " (3 chars).
    let key_labels = ["JeTTY", "Render", "Terminal", "Themes"];
    let key_col_chars = key_labels.iter().map(|k| k.chars().count()).max().unwrap_or(0);
    let sep = " | "; // ASCII pipe separator (portable, no fancy Unicode in all fonts)
    let key_w = (key_col_chars + sep.chars().count()) as f32 * char_w;

    // Value column starts after the key column.
    let val_x = info_x + key_w;

    // Info row values.
    let info_rows: &[(&str, String)] = &[
        ("JeTTY", version.to_string()),
        ("Render", format!("wgpu · {}", backend)),
        ("Terminal", format!("JeTTY {}", version)),
        ("Themes", "Mocha · Tokyo · Gruvbox · Dracula · Onyx".to_string()),
    ];

    // Compute logo block height so we can vertically center the info rows
    // alongside it (or just start them from the top of the logo).
    let logo_h = LOGO.len() as f32 * line_h;
    let info_h = info_rows.len() as f32 * line_h;
    // Vertically center the info block relative to the logo block.
    let info_y_offset = ((logo_h - info_h) / 2.0).max(0.0);

    // Swatch row sits below whichever is taller (logo or info block).
    let content_h = logo_h.max(info_h);
    const SWATCH_GAP: f32 = 14.0; // gap between content and swatches
    let swatch_y = grid_top_px + TOP_INSET + content_h + SWATCH_GAP;
    const SWATCH_W: f32 = 16.0;
    const SWATCH_H: f32 = 16.0;
    const SWATCH_PAD: f32 = 3.0; // spacing between swatches

    // Tip line sits below the swatches.
    const TIP_GAP: f32 = 14.0;
    let tip_y = swatch_y + SWATCH_H + TIP_GAP;

    // --- Assemble quads (color swatches only — no dim backdrop or border) ---
    let mut quads: Vec<Rect> = Vec::new();

    // 16 ANSI color swatches — a small row of filled squares showing the theme palette.
    for i in 0..16usize {
        let color = theme.palette[i];
        let x = LEFT_INSET + i as f32 * (SWATCH_W + SWATCH_PAD);
        quads.push(Rect {
            x,
            y: swatch_y,
            w: SWATCH_W,
            h: SWATCH_H,
            color: [color[0], color[1], color[2], 220],
            radius: 3.0,
        });
    }

    // --- Assemble labels ---
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // ASCII logo block (accent-colored).
    let logo_top = grid_top_px + TOP_INSET;
    for (i, line) in LOGO.iter().enumerate() {
        let y = logo_top + i as f32 * line_h;
        labels.push((line.to_string(), LEFT_INSET, y, accent));
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
    labels.push((
        "tip: try help, theme <name>, bench — or just start typing.".to_string(),
        LEFT_INSET,
        tip_y,
        tip_col,
    ));

    WelcomeOverlay { quads, labels }
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
        let w = build_welcome_overlay(1000, 700, 36.0, "0.1.0", "Vulkan", &theme(), TEST_CHAR_W, 22.0);
        assert!(!w.labels.is_empty(), "welcome overlay must have labels");
    }

    #[test]
    fn swatch_quad_count_is_16() {
        let w = build_welcome_overlay(1000, 700, 36.0, "0.1.0", "Vulkan", &theme(), TEST_CHAR_W, 22.0);
        // All quads are swatches (16 ANSI colors).
        assert_eq!(w.quads.len(), 16, "expected exactly 16 swatch quads");
    }

    #[test]
    fn content_includes_jetty() {
        let w = build_welcome_overlay(1000, 700, 36.0, "0.1.0", "Vulkan", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("JeTTY"), "welcome overlay must mention JeTTY");
    }

    #[test]
    fn content_includes_tip() {
        let w = build_welcome_overlay(1000, 700, 36.0, "0.1.0", "Vulkan", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("tip:"), "welcome overlay must include a tip line");
    }

    #[test]
    fn works_at_small_window() {
        // Should not panic at small sizes; we just clip gracefully.
        let w = build_welcome_overlay(320, 200, 36.0, "0.1.0", "Gl", &theme(), TEST_CHAR_W, 22.0);
        assert_eq!(w.quads.len(), 16);
    }

    #[test]
    fn backend_name_appears_in_render_row() {
        let w = build_welcome_overlay(1000, 700, 36.0, "1.2.3", "Metal", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("Metal"), "backend name must appear in Render row");
    }

    #[test]
    fn version_appears() {
        let w = build_welcome_overlay(1000, 700, 36.0, "9.8.7", "Vulkan", &theme(), TEST_CHAR_W, 22.0);
        let joined: String = w.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("9.8.7"), "version must appear in welcome");
    }
}
