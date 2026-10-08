//! Per-window overlay state: the scrollback-search bar, the keyboard-shortcuts
//! help, the command palette, hint mode and keyboard copy-mode.
//!
//! The main window owns one [`Overlays`] (`App::ov`) and every detached window
//! owns its own (`DetachedWindow::ov`), so an overlay opened in a window acts on
//! THAT window's terminal and is drawn in that window. Operations name their
//! window with a [`Surface`].

use std::time::Instant;

/// How often an open search re-collects its matches while output streams in
/// (stored match points go stale as the scrollback rotates).
pub(crate) const SEARCH_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// Which window an overlay operation targets: the main (tabbed) window, or a
/// detached window by its index in `App::detached`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    Main,
    Detached(usize),
}

/// Owned command-palette draw data, captured before the render borrow:
/// `(query, visible rows as (title, matched-char indices, selected), total,
/// first_visible)`.
pub(crate) type PaletteDrawData = (String, Vec<(String, Vec<usize>, bool)>, usize, usize);

/// Hint-mode overlay draw data captured before the mutable render borrow: the
/// visible `(label, vp_row, col_start)` chips + the typed prefix.
pub(crate) type HintDrawData = (Vec<(String, usize, usize)>, String);

/// Hint-mode capture state: the scanned tokens, their parallel labels, and the
/// typed label prefix.
pub(crate) struct HintState {
    pub tokens: Vec<jetty_core::HintToken>,
    pub labels: Vec<String>,
    pub typed: String,
}

/// One window's overlays. Zero cost while everything is closed: the vectors are
/// empty and every overlay is one bool / `Option` test on the hot path.
#[derive(Default)]
pub(crate) struct Overlays {
    /// Whether the keyboard-shortcuts help is open. Dismissed by Esc, the "?"
    /// button, or a click outside the panel.
    pub help_open: bool,
    /// First help row shown when the rows overflow the window (large UI font /
    /// short window); scrolled by the wheel, arrows, PgUp/PgDn, Home/End.
    pub help_scroll: usize,
    /// Whether the scrollback-search bar is open on this window's (active)
    /// tab. While open, keys edit the query; Esc / ✕ / the search chord close
    /// it and clear the matches.
    pub search_open: bool,
    /// Last streaming refresh of the open search's matches (throttled to
    /// [`SEARCH_REFRESH_INTERVAL`] on the drain path so heavy output never
    /// re-scans history every frame). `None` until the first refresh.
    pub search_refresh_at: Option<Instant>,
    /// True while the open search's stored matches may be stale: set when a
    /// drain consumed output but the throttle skipped the re-collect, cleared by
    /// every refresh. While set, `about_to_wait` schedules ONE wake at the
    /// throttle deadline so a burst that ends inside the window still gets a
    /// trailing refresh — the flag never exists while idle.
    pub search_dirty: bool,
    /// Whether the command palette is open. While open it captures ALL keyboard
    /// and mouse input of its window.
    pub palette_open: bool,
    /// The typed query. Refiltered only on a keystroke — never per frame.
    pub palette_query: String,
    /// Index of the highlighted row within `palette_filtered`.
    pub palette_selected: usize,
    /// First visible row (scroll offset) into `palette_filtered`.
    pub palette_scroll: usize,
    /// The action registry, rebuilt FRESH on open and dropped on close.
    pub palette_registry: Vec<crate::palette::PaletteEntry>,
    /// The current fuzzy hits (resolved command + title + matched indices),
    /// recomputed on each keystroke. Enter runs the stored command, never a
    /// stale index.
    pub palette_filtered: Vec<crate::palette::PaletteHit>,
    /// Active hint-mode state (primary screen only). `Some` while the labelled
    /// URL/path/hash/IPv4 chips are shown; the tokens are scanned ONCE on enter.
    pub hint_mode: Option<HintState>,
    /// Active copy-mode state: a keyboard vi-cursor over the viewport +
    /// scrollback. `Some` while active; the shell cursor is suppressed and the
    /// terminal selection drives the highlight.
    pub copy_mode: Option<crate::copymode::CopyMode>,
}

impl Overlays {
    /// True while one of this window's bar/modal overlays owns the keyboard
    /// (palette, help, search) — hint and copy-mode can't start then.
    pub fn owns_keys(&self) -> bool {
        self.palette_open || self.help_open || self.search_open
    }

    /// Recompute the palette hits from the query (on open + each keystroke) and
    /// reset the selection/scroll to the top.
    pub fn refilter_palette(&mut self) {
        self.palette_filtered = crate::palette::filter(&self.palette_registry, &self.palette_query);
        self.palette_selected = 0;
        self.palette_scroll = 0;
    }

    /// Close the palette and free its transient state. Returns whether it was
    /// open (the caller repaints only then).
    pub fn close_palette(&mut self) -> bool {
        if !self.palette_open {
            return false;
        }
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_filtered = Vec::new();
        self.palette_registry = Vec::new();
        true
    }

    /// Move the palette selection by `delta` rows (clamped), keeping it inside
    /// the `MAX_PALETTE_ROWS` scroll window.
    pub fn palette_move(&mut self, delta: isize) {
        let n = self.palette_filtered.len();
        if n == 0 {
            return;
        }
        let next = (self.palette_selected as isize + delta).clamp(0, n as isize - 1) as usize;
        self.palette_selected = next;
        let win = jetty_render::MAX_PALETTE_ROWS;
        if next < self.palette_scroll {
            self.palette_scroll = next;
        } else if next >= self.palette_scroll + win {
            self.palette_scroll = next + 1 - win;
        }
    }

    /// The highlighted palette command, if any.
    pub fn palette_pick(&self) -> Option<crate::palette::PaletteCmd> {
        self.palette_filtered.get(self.palette_selected).map(|h| h.cmd.clone())
    }

    /// Output was drained into this window's (active) tab: an open search's
    /// stored matches may be stale until the next re-collect.
    pub fn note_output(&mut self) {
        if self.search_open {
            self.search_dirty = true;
        }
    }

    /// Whether a throttled search re-collect is due at `now` (open, dirty, and
    /// the throttle window has passed).
    pub fn search_refresh_due(&self, now: Instant) -> bool {
        self.search_open
            && self.search_dirty
            && self.search_refresh_at.is_none_or(|t| now.saturating_duration_since(t) >= SEARCH_REFRESH_INTERVAL)
    }

    /// Record a search re-collect at `now`.
    pub fn search_refreshed(&mut self, now: Instant) {
        self.search_dirty = false;
        self.search_refresh_at = Some(now);
    }

    /// The one wake a skipped (throttled) refresh needs — the throttle deadline —
    /// or `None` when nothing is pending (idle costs nothing).
    pub fn search_wake(&self) -> Option<Instant> {
        if self.search_open && self.search_dirty {
            self.search_refresh_at.map(|t| t + SEARCH_REFRESH_INTERVAL)
        } else {
            None
        }
    }

    /// Hint chips to draw: `(label, row, col)` of every token whose label still
    /// matches the typed prefix, plus the prefix. `None` while inactive.
    pub fn hint_draw(&self) -> Option<HintDrawData> {
        self.hint_mode.as_ref().map(|hs| {
            let typed = hs.typed.clone();
            let labeled = hs
                .labels
                .iter()
                .zip(hs.tokens.iter())
                .filter(|(lab, _)| typed.is_empty() || lab.starts_with(&typed))
                .filter_map(|(lab, tok)| tok.spans.first().map(|(r, c, _)| (lab.clone(), *r, *c)))
                .collect();
            (labeled, typed)
        })
    }

    /// Palette rows to draw: `(query, visible (title, matched indices, selected),
    /// total, first_visible)`. `None` while closed.
    pub fn palette_draw(&self) -> Option<PaletteDrawData> {
        if !self.palette_open {
            return None;
        }
        let first = self.palette_scroll;
        let sel = self.palette_selected;
        let rows = self
            .palette_filtered
            .iter()
            .enumerate()
            .skip(first)
            .take(jetty_render::MAX_PALETTE_ROWS)
            .map(|(i, h)| (h.title.clone(), h.indices.clone(), i == sel))
            .collect();
        Some((self.palette_query.clone(), rows, self.palette_filtered.len(), first))
    }

    /// Copy-mode cursor + pill state: `(row, col, selecting, line_mode)`.
    pub fn copy_draw(&self) -> Option<(usize, usize, bool, bool)> {
        self.copy_mode.as_ref().map(|c| (c.row, c.col, c.selecting, c.line_mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(n: usize) -> Vec<crate::palette::PaletteHit> {
        let reg: Vec<crate::palette::PaletteEntry> = (0..n)
            .map(|i| crate::palette::PaletteEntry {
                title: format!("Entry {i}"),
                keywords: "",
                cmd: crate::palette::PaletteCmd::SelectTab(i as u64),
            })
            .collect();
        crate::palette::filter(&reg, "")
    }

    #[test]
    fn palette_selection_clamps_and_scrolls_with_its_window() {
        let mut ov = Overlays { palette_open: true, palette_filtered: hits(40), ..Default::default() };
        ov.palette_move(-3);
        assert_eq!((ov.palette_selected, ov.palette_scroll), (0, 0));
        let win = jetty_render::MAX_PALETTE_ROWS;
        ov.palette_move(win as isize);
        assert_eq!(ov.palette_selected, win);
        assert_eq!(ov.palette_scroll, 1, "the selected row stays inside the scroll window");
        ov.palette_move(1000);
        assert_eq!(ov.palette_selected, 39);
        assert_eq!(ov.palette_scroll, 40 - win);
        ov.palette_move(-1000);
        assert_eq!((ov.palette_selected, ov.palette_scroll), (0, 0));
    }

    #[test]
    fn closing_the_palette_frees_its_state_once() {
        let mut ov = Overlays {
            palette_open: true,
            palette_query: "tab".into(),
            palette_filtered: hits(3),
            ..Default::default()
        };
        assert!(ov.close_palette());
        assert!(!ov.palette_open && ov.palette_query.is_empty() && ov.palette_filtered.is_empty());
        assert!(!ov.close_palette(), "a second close is a no-op (no repaint)");
    }

    #[test]
    fn search_refresh_is_throttled_and_wakes_once_for_the_trailing_refresh() {
        let t0 = Instant::now();
        let mut ov = Overlays::default();
        ov.note_output();
        assert!(!ov.search_dirty, "output with the bar closed marks nothing");
        ov.search_open = true;
        ov.note_output();
        assert!(ov.search_refresh_due(t0), "first refresh is due at once");
        ov.search_refreshed(t0);
        assert_eq!(ov.search_wake(), None, "clean → no wake");
        ov.note_output();
        let soon = t0 + SEARCH_REFRESH_INTERVAL / 2;
        assert!(!ov.search_refresh_due(soon), "inside the throttle window");
        assert_eq!(ov.search_wake(), Some(t0 + SEARCH_REFRESH_INTERVAL), "one trailing wake");
        assert!(ov.search_refresh_due(t0 + SEARCH_REFRESH_INTERVAL));
    }

    #[test]
    fn closed_overlays_draw_nothing() {
        let ov = Overlays::default();
        assert!(ov.hint_draw().is_none());
        assert!(ov.palette_draw().is_none());
        assert!(ov.copy_draw().is_none());
        assert!(!ov.owns_keys());
    }
}
