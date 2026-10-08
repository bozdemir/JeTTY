//! `minimum_contrast`: the snapshot pushes each glyph's FINAL color (after
//! reverse video and faint) to the configured WCAG ratio against its cell
//! background — except concealed text (SGR 8) and the shape glyphs (powerline,
//! blocks, sextants), whose color is the drawing.

use jetty_core::contrast::contrast_ratio;
use jetty_core::{theme_at, theme_index, Terminal};

fn term(theme: &str, min: f32) -> Terminal {
    let mut t = Terminal::new(40, 4);
    t.set_theme(theme_at(theme_index(theme).unwrap()));
    t.set_minimum_contrast(min);
    t
}

#[test]
fn off_by_default_and_at_one() {
    // solarized_dark's palette 8 (bright black) IS its background: zsh
    // autosuggestions (fg 8) are invisible there — and stay so while it's off.
    let mut t = Terminal::new(40, 4);
    t.set_theme(theme_at(theme_index("solarized_dark").unwrap()));
    assert_eq!(t.minimum_contrast(), 1.0);
    t.feed(b"\x1b[90mhint\x1b[0m");
    let s = t.snapshot();
    assert_eq!(s.cell(0, 0).fg, s.cell(0, 0).bg, "1.0 changes nothing");
}

#[test]
fn invisible_autosuggestions_become_readable() {
    let mut t = term("solarized_dark", 4.5);
    t.feed(b"\x1b[90mhint\x1b[0m plain");
    let s = t.snapshot();
    let c = s.cell(0, 0);
    assert!(contrast_ratio(c.fg, c.bg) >= 4.5, "{:?} on {:?}", c.fg, c.bg);
    // Default text already reads (solarized base0 on base03 ≈ 4.75:1): unchanged.
    let theme = theme_at(theme_index("solarized_dark").unwrap());
    assert_eq!(s.cell(0, 5).fg, theme.fg);
}

#[test]
fn the_final_color_is_judged_after_reverse_video_and_faint() {
    let mut t = term("catppuccin_mocha", 3.0);
    // Reverse video: the old bg becomes the glyph color, judged against the old fg.
    t.feed(b"\x1b[7mR\x1b[0m\x1b[2mF\x1b[0m\x1b[48;2;200;200;200;38;2;210;210;210mX");
    let s = t.snapshot();
    for col in 0..3 {
        let c = s.cell(0, col);
        assert!(contrast_ratio(c.fg, c.bg) >= 3.0, "col {col}: {:?} on {:?}", c.fg, c.bg);
    }
    // Truecolor light-on-light flips toward black (white can't reach 3:1 on it).
    let x = s.cell(0, 2);
    assert!(x.fg[0] < 200, "{:?}", x.fg);
}

#[test]
fn concealed_text_stays_invisible() {
    let mut t = term("catppuccin_mocha", 7.0);
    t.feed(b"\x1b[8msecret\x1b[0m");
    let s = t.snapshot();
    assert_eq!(s.cell(0, 0).fg, s.cell(0, 0).bg);
}

#[test]
fn shape_glyphs_keep_their_exact_colors() {
    let mut t = term("catppuccin_mocha", 7.0);
    // A powerline arrow drawn in the next segment's (dark) color, a block and a
    // sextant: their color is the drawing, never adjusted.
    t.feed("\x1b[38;2;40;40;60m\u{E0B0}\u{2588}\u{1FB00}a\x1b[0m".as_bytes());
    let s = t.snapshot();
    for col in 0..3 {
        assert_eq!(s.cell(0, col).fg, [40, 40, 60], "col {col}");
    }
    // An ordinary letter in the same color is adjusted.
    assert_ne!(s.cell(0, 3).fg, [40, 40, 60]);
    assert!(contrast_ratio(s.cell(0, 3).fg, s.cell(0, 3).bg) >= 7.0);
}

#[test]
fn the_underline_follows_the_adjusted_glyph() {
    let mut t = term("solarized_dark", 4.5);
    t.feed(b"\x1b[4;90mu\x1b[0m");
    let s = t.snapshot();
    let c = s.cell(0, 0);
    assert_eq!(c.uline, c.fg, "an underline without SGR 58 takes the glyph color");
    assert!(contrast_ratio(c.fg, c.bg) >= 4.5);
}

#[test]
fn every_builtin_palette_color_reaches_the_ratio_on_its_background() {
    for i in 0..jetty_core::theme::PRESETS.len() {
        let theme = theme_at(i);
        let mut t = Terminal::new(20, 2);
        t.set_theme(theme.clone());
        t.set_minimum_contrast(4.5);
        for n in 0..16u8 {
            t.feed(format!("\x1b[H\x1b[38;5;{n}mX\x1b[0m").as_bytes());
            let s = t.snapshot();
            let c = s.cell(0, 0);
            let r = contrast_ratio(c.fg, c.bg);
            assert!(r >= 4.5, "{} color {n}: {r:.2}", theme.name);
        }
    }
}
