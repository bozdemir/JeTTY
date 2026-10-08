//! DEC private mode 2031 (color-scheme change reports) and the `CSI ? 996 n`
//! color-scheme query, as programs see them through the PTY replies.
//!
//! The protocol (contour's "dark and light mode detection", also kitty, ghostty,
//! neovim): `CSI ? 996 n` asks for the current scheme, answered with
//! `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light); `CSI ? 2031 h` asks for
//! that report unsolicited whenever the palette changes; DECRQM
//! `CSI ? 2031 $ p` must answer set (1) / reset (2) — neovim only enables the
//! mode after one of those.

use jetty_core::{theme_at, theme_index, Terminal, Theme};

const DARK: &[u8] = b"\x1b[?997;1n";
const LIGHT: &[u8] = b"\x1b[?997;2n";

fn theme(name: &str) -> Theme {
    theme_at(theme_index(name).unwrap_or_else(|| panic!("no built-in {name}")))
}

fn replies(t: &mut Terminal) -> Vec<u8> {
    t.drain_pty_writes()
}

/// Feed `seq` split at every byte boundary (and whole), with and without live
/// OSC 133 anchors (which change how the scanner reads CSIs), into a fresh
/// terminal each time; `check` sees the terminal and its replies.
fn at_every_split(seq: &[u8], mut check: impl FnMut(&mut Terminal, Vec<u8>, &str)) {
    for anchors in [false, true] {
        for cut in 0..=seq.len() {
            let mut t = Terminal::new(30, 6);
            if anchors {
                t.feed(b"\x1b]133;A\x07");
            }
            let _ = replies(&mut t);
            t.feed(&seq[..cut]);
            t.feed(&seq[cut..]);
            let r = replies(&mut t);
            check(&mut t, r, &format!("{:?} cut {cut} anchors {anchors}", String::from_utf8_lossy(seq)));
        }
    }
}

#[test]
fn query_996_is_answered_by_the_background_luminance() {
    let mut t = Terminal::new(30, 6);
    t.set_theme(theme("catppuccin_mocha"));
    t.feed(b"\x1b[?996n");
    assert_eq!(replies(&mut t), DARK);
    t.set_theme(theme("solarized_light"));
    t.feed(b"\x1b[?996n");
    assert_eq!(replies(&mut t), LIGHT);
}

#[test]
fn query_996_is_recognized_at_any_split() {
    at_every_split(b"ab\x1b[?996ncd", |t, r, what| {
        assert_eq!(r, DARK, "{what}");
        // The sequence still went through vte and nothing leaked as text.
        let snap = t.snapshot();
        let row: String = snap.cells.iter().take(4).map(|c| c.c).collect();
        assert_eq!(row, "abcd", "{what}");
    });
}

#[test]
fn only_the_exact_query_is_answered() {
    for seq in [
        &b"\x1b[996n"[..],         // not private: a plain DSR alacritty ignores
        b"\x1b[?995n",
        b"\x1b[?9960n",
        b"\x1b[?1;996n",            // two parameters
        b"\x1b[?996;1n",
        b"\x1b[?996$n",             // an intermediate
        b"\x1b[?99\x186n",          // CAN aborts it
    ] {
        at_every_split(seq, |_, r, what| assert!(r.is_empty(), "{what}: {:?}", String::from_utf8_lossy(&r)));
    }
}

#[test]
fn the_996_reply_keeps_its_place_among_other_replies() {
    // DA1 before the query in the same read: alacritty answers DA1 while the
    // scanner answers 996 — the replies must come back in request order (a
    // program using DA1 as the end-of-probe sentinel would otherwise misread).
    let mut t = Terminal::new(30, 6);
    t.feed(b"\x1b[c\x1b[?996n\x1b[5n");
    let r = String::from_utf8(replies(&mut t)).unwrap();
    let da = r.find("\x1b[?6c").or_else(|| r.find('c')).expect("DA1 reply");
    let scheme = r.find("\x1b[?997;1n").expect("996 reply");
    let status = r.find("\x1b[0n").expect("DSR 5 reply");
    assert!(da < scheme && scheme < status, "order: {r:?}");
}

#[test]
fn mode_2031_is_tracked_at_any_split() {
    at_every_split(b"\x1b[?2031h", |t, _, what| assert!(t.color_reports(), "{what}"));
    at_every_split(b"\x1b[?1000;2031;1006h", |t, _, what| assert!(t.color_reports(), "{what}"));
    at_every_split(b"\x1b[?2031h\x1b[?2031l", |t, _, what| assert!(!t.color_reports(), "{what}"));
    at_every_split(b"\x1b[?2031h\x1bc", |t, _, what| assert!(!t.color_reports(), "{what}: RIS resets it"));
    for seq in [&b"\x1b[2031h"[..], b"\x1b[?20310h", b"\x1b[?2031$h", b"\x1b[?2031:1h", b"\x1b[?203\x181h"] {
        at_every_split(seq, |t, _, what| assert!(!t.color_reports(), "{what}"));
    }
}

#[test]
fn decrqm_2031_reports_set_or_reset_never_unrecognized() {
    let mut t = Terminal::new(30, 6);
    t.feed(b"\x1b[?2031$p");
    assert_eq!(replies(&mut t), b"\x1b[?2031;2$y", "supported, currently reset");
    t.feed(b"\x1b[?2031h\x1b[?2031$p");
    assert_eq!(replies(&mut t), b"\x1b[?2031;1$y", "set");
    // Order inside one read: the query answers the state it had at that point.
    t.feed(b"\x1b[?2031$p\x1b[?2031l\x1b[?2031$p");
    assert_eq!(replies(&mut t), b"\x1b[?2031;1$y\x1b[?2031;2$y");
    // Other unknown modes keep alacritty's "not recognized" answer.
    t.feed(b"\x1b[?2032$p");
    assert_eq!(replies(&mut t), b"\x1b[?2032;0$y");
}

#[test]
fn a_palette_change_is_reported_only_while_2031_is_on() {
    let mut t = Terminal::new(30, 6);
    t.set_theme(theme("catppuccin_mocha"));
    t.set_theme(theme("solarized_light"));
    assert!(replies(&mut t).is_empty(), "mode off: no unsolicited report");

    t.feed(b"\x1b[?2031h");
    let _ = replies(&mut t);
    t.set_theme(theme("catppuccin_mocha"));
    assert_eq!(replies(&mut t), DARK);
    t.set_theme(theme("solarized_light"));
    assert_eq!(replies(&mut t), LIGHT);
    // A dark → dark switch still changes the palette: programs re-query it.
    t.set_theme(theme("dracula"));
    t.set_theme(theme("nord"));
    assert_eq!(replies(&mut t), [DARK, DARK].concat());

    // Opacity alone (the alpha of `bg`) is not a palette change.
    let mut faded = theme("nord");
    faded.bg[3] = 128;
    t.set_theme(faded);
    t.set_theme(theme("nord"));
    assert!(replies(&mut t).is_empty(), "opacity-only change must not report");

    t.feed(b"\x1b[?2031l");
    t.set_theme(theme("solarized_light"));
    assert!(replies(&mut t).is_empty(), "mode turned off again");
}

#[test]
fn reports_follow_the_theme_shown_not_the_one_at_spawn() {
    let mut t = Terminal::new(30, 6);
    t.set_theme(theme("solarized_light"));
    t.feed(b"\x1b[?2031h\x1b[?996n");
    assert_eq!(replies(&mut t), LIGHT);
    t.set_theme(theme("gruvbox_dark"));
    t.feed(b"\x1b[?996n");
    assert_eq!(replies(&mut t), [DARK, DARK].concat(), "the change report, then the query answer");
}
