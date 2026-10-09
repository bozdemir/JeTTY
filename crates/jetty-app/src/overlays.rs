//! Per-window overlay state: the scrollback-search bar, the keyboard-shortcuts
//! help, the command palette, hint mode and keyboard copy-mode.
//!
//! The main window owns one [`Overlays`] (`App::ov`) and every detached window
//! owns its own (`DetachedWindow::ov`), so an overlay opened in a window acts on
//! THAT window's terminal and is drawn in that window. Operations name their
//! window with a [`Surface`].
//!
//! Every layer of a window — these overlays, its menus, and in the main window
//! the confirmations, the tab rename and the welcome splash — sits in ONE order
//! ([`Layer`]): keys, pointer presses and the draw pass all meet the layers in
//! it, and whatever opens takes the keyboard by its rule
//! ([`Layer::stays_under`], applied by `App::take_keyboard`), so the layer
//! drawn on top is always the one the input reaches.

use std::time::Instant;

/// A window's input layers in THE one order, top first. The key routing, the
/// pointer routing and the draw pass all follow it (the draw paints it
/// bottom-up). Opening a layer closes every open one that may not stay under
/// it ([`Layer::stays_under`]), so the pairs that can be open together are
/// few, and each of them is routed top first on every path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Layer {
    /// The quit / close-tab confirmation (main window): Enter / Esc, every
    /// other key and every press swallowed.
    Confirm,
    /// The command palette: every key and press.
    Palette,
    /// The keyboard-shortcuts help: every press, Esc and its scroll keys (the
    /// rest reach the shell).
    Help,
    /// A context or tab menu: its keys and every press.
    Menu,
    /// The inline tab rename (main window): every key.
    Rename,
    /// Hint mode: every key and press.
    Hint,
    /// Copy-mode: every key; a press leaves it.
    Copy,
    /// The scrollback-search bar: every key, presses on its panel.
    Search,
    /// The welcome splash (main window): gone at the first key or click.
    Welcome,
}

impl Layer {
    /// Whether this open layer stays open when `owner` opens and takes the
    /// keyboard. A confirmation keeps everything (it is the top layer, and
    /// cancelling it hands the window back as it was) and nothing closes it.
    /// The search bar stays under the palette, a menu and the tab rename, and
    /// copy-mode under a menu (its Copy row copies copy-mode's selection): each
    /// of them is above the one it covers on every path and hands it back on
    /// close. Every other open layer closes — the search bar would keep taking
    /// keys under the help's scrim, hint mode would swallow the clicks meant
    /// for a menu, and the welcome splash is gone once anything takes over.
    pub fn stays_under(self, owner: Layer) -> bool {
        use Layer::*;
        matches!(
            (owner, self),
            (_, Confirm) | (Confirm, _) | (Palette | Menu | Rename, Search) | (Menu, Copy)
        )
    }
}

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

/// Rows a list moves for one wheel event, as a fraction to accumulate
/// (positive = down, toward the end; winit's positive delta is up): a wheel
/// notch — a `LineDelta` of 1, fractional on an X11 touchpad — is one row; a
/// touchpad's `PixelDelta` (Wayland, macOS) is one row per `row_px`
/// (physical) of travel.
pub(crate) fn wheel_rows(delta: winit::event::MouseScrollDelta, row_px: f32) -> f32 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => -y,
        winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / row_px.max(1.0),
    }
}

/// Owned command-palette draw data, captured before the render borrow:
/// `(query, visible rows as (title, matched-char indices, selected), total,
/// first_visible)`.
pub(crate) type PaletteDrawData = (String, Vec<(String, Vec<usize>, bool)>, usize, usize);

/// Hint-mode overlay draw data captured before the mutable render borrow: the
/// visible `(label, vp_row, col_start)` chips + the typed prefix.
pub(crate) type HintDrawData = (Vec<(String, usize, usize)>, String);

/// What the theme on screen does after the palette selection changed (see
/// [`Overlays::theme_preview_step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemePreview {
    /// Leave it.
    Keep,
    /// Show theme `i` — `theme_idx` only: nothing is chosen or saved.
    Show(usize),
    /// Show the chosen theme again (the preview ended without a pick).
    Restore,
}

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
    /// A live theme preview is on screen: the selection was navigated onto a
    /// `Theme: …` row (see [`Overlays::theme_preview_step`]). Ends by Enter on a
    /// theme row (kept) or any other close (the chosen theme comes back).
    pub theme_preview: bool,
    /// Wheel travel toward the next palette row (see [`Overlays::palette_wheel`]).
    pub palette_wheel_acc: crate::input::ScrollAccumulator,
    /// Active hint-mode state (primary screen only). `Some` while the labelled
    /// URL/path/hash/IPv4 chips are shown; the tokens are scanned ONCE on enter.
    pub hint_mode: Option<HintState>,
    /// Active copy-mode state: a keyboard vi-cursor over the viewport +
    /// scrollback. `Some` while active; the shell cursor is suppressed and the
    /// terminal selection drives the highlight.
    pub copy_mode: Option<crate::copymode::CopyMode>,
}

impl Overlays {
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
        self.palette_wheel_acc.reset();
        true
    }

    /// Move the palette selection for one wheel event: a notch is one row, a
    /// touchpad one row per `row_px` (physical) of travel, accumulated across
    /// events (see [`wheel_rows`]). Whether the selection moved.
    pub fn palette_wheel(&mut self, delta: winit::event::MouseScrollDelta, row_px: f32) -> bool {
        let n = self.palette_wheel_acc.add(wheel_rows(delta, row_px));
        let before = self.palette_selected;
        if n != 0 {
            self.palette_move(n as isize);
        }
        self.palette_selected != before
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

    /// The palette selection just changed — by NAVIGATION (`navigated`: arrows,
    /// Page keys, the wheel) or by a refilter (typing). What the theme on screen
    /// does: a preview starts only by navigating onto a `Theme: …` row (typing a
    /// query never flashes themes by); once live it follows the selection, and a
    /// selection off the theme rows ends it, showing the chosen theme again.
    pub fn theme_preview_step(&mut self, navigated: bool) -> ThemePreview {
        let selected = match self.palette_filtered.get(self.palette_selected).map(|h| &h.cmd) {
            Some(crate::palette::PaletteCmd::SetTheme(i)) => Some(*i),
            _ => None,
        };
        match (self.theme_preview, selected) {
            (false, Some(i)) if navigated => {
                self.theme_preview = true;
                ThemePreview::Show(i)
            }
            (false, _) => ThemePreview::Keep,
            (true, Some(i)) => ThemePreview::Show(i),
            (true, None) => {
                self.theme_preview = false;
                ThemePreview::Restore
            }
        }
    }

    /// The palette is closing. `kept`: the command run on close (Enter / a
    /// click) picks the previewed theme row, which keeps it. Returns whether the
    /// chosen theme must be shown again (a preview ended without a pick: Esc, a
    /// click outside, another command).
    pub fn end_theme_preview(&mut self, kept: bool) -> bool {
        std::mem::take(&mut self.theme_preview) && !kept
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

    /// Copy-mode cursor + pill state: `(row, col, what the pill says)`.
    pub fn copy_draw(&self) -> Option<(usize, usize, jetty_render::CopySelect)> {
        self.copy_mode.as_ref().map(|c| (c.row, c.col, c.pill()))
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

    /// A palette listing `New tab`, `Theme: A`, `Theme: B`, `Quit` (in that
    /// order), opened with the selection on the first row.
    fn theme_rows_palette() -> Overlays {
        use crate::palette::{PaletteCmd as C, PaletteEntry};
        let reg = vec![
            PaletteEntry { title: "New tab".into(), keywords: "", cmd: C::NewTab },
            PaletteEntry { title: "Theme: A".into(), keywords: "", cmd: C::SetTheme(4) },
            PaletteEntry { title: "Theme: B".into(), keywords: "", cmd: C::SetTheme(9) },
            PaletteEntry { title: "Quit".into(), keywords: "", cmd: C::Quit },
        ];
        Overlays {
            palette_open: true,
            palette_filtered: crate::palette::filter(&reg, ""),
            palette_registry: reg,
            ..Default::default()
        }
    }

    #[test]
    fn arrowing_over_theme_rows_previews_them_and_leaving_restores() {
        let mut ov = theme_rows_palette();
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Keep, "on `New tab`: nothing");
        ov.palette_move(1);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Show(4));
        assert!(ov.theme_preview);
        ov.palette_move(1);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Show(9));
        ov.palette_move(1);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Restore, "off the theme rows");
        assert!(!ov.theme_preview);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Keep);
    }

    #[test]
    fn typing_never_starts_a_preview_but_a_live_one_follows_the_selection() {
        let mut ov = theme_rows_palette();
        // A refilter that lands on a theme row (typing "theme") shows nothing.
        ov.palette_selected = 1;
        assert_eq!(ov.theme_preview_step(false), ThemePreview::Keep);
        // Once arrowed into, a later refilter keeps the preview in step.
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Show(4));
        ov.palette_selected = 2;
        assert_eq!(ov.theme_preview_step(false), ThemePreview::Show(9));
        ov.palette_selected = 0;
        assert_eq!(ov.theme_preview_step(false), ThemePreview::Restore);
    }

    #[test]
    fn esc_reverts_and_enter_on_the_theme_keeps_it() {
        let mut ov = theme_rows_palette();
        ov.palette_move(2);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Show(9));
        assert!(ov.end_theme_preview(false), "Esc / click outside / other command: restore");
        assert!(!ov.theme_preview);
        assert!(!ov.end_theme_preview(false), "nothing to restore twice");

        let mut ov = theme_rows_palette();
        ov.palette_move(1);
        assert_eq!(ov.theme_preview_step(true), ThemePreview::Show(4));
        assert!(!ov.end_theme_preview(true), "Enter on the theme row keeps it");
        assert!(!ov.theme_preview);

        // No preview at all: closing restores nothing.
        let mut ov = theme_rows_palette();
        assert!(!ov.end_theme_preview(false));
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

    /// The palette moves one row per wheel notch and one row per row of
    /// touchpad travel. It used to step one row per EVENT: an X11 touchpad's
    /// fractional `LineDelta`s rounded to 0 (a slow swipe never moved), and a
    /// Wayland / macOS touchpad's stream of small `PixelDelta`s raced through
    /// the list a row per event.
    #[test]
    fn the_palette_wheel_moves_by_travel_not_by_event() {
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta::{LineDelta, PixelDelta};
        let px = |y: f64| PixelDelta(PhysicalPosition::new(0.0, y));
        let mut ov = Overlays { palette_open: true, palette_filtered: hits(40), ..Default::default() };
        assert!(ov.palette_wheel(LineDelta(0.0, -1.0), 28.0), "a notch down");
        assert_eq!(ov.palette_selected, 1);
        for _ in 0..3 {
            assert!(!ov.palette_wheel(LineDelta(0.0, -0.25), 28.0), "a quarter line: not yet");
        }
        assert!(ov.palette_wheel(LineDelta(0.0, -0.25), 28.0), "a whole line of X11 touchpad travel");
        assert_eq!(ov.palette_selected, 2);
        for _ in 0..3 {
            assert!(!ov.palette_wheel(px(-7.0), 28.0), "7 px of a 28-px row: not yet");
        }
        assert!(ov.palette_wheel(px(-7.0), 28.0), "a whole row of pixel travel");
        assert_eq!(ov.palette_selected, 3);
        assert!(ov.palette_wheel(LineDelta(0.0, 2.0), 28.0), "two notches up");
        assert_eq!(ov.palette_selected, 1);
        // Closing drops a half-finished swipe.
        ov.palette_wheel(px(-20.0), 28.0);
        ov.close_palette();
        let mut ov = Overlays { palette_open: true, palette_filtered: hits(40), ..ov };
        assert!(!ov.palette_wheel(px(-10.0), 28.0), "no remainder carried into the next open");
    }

    #[test]
    fn wheel_rows_follow_winit_signs() {
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta::{LineDelta, PixelDelta};
        assert_eq!(wheel_rows(LineDelta(0.0, 1.0), 20.0), -1.0, "wheel up: toward the top");
        assert_eq!(wheel_rows(LineDelta(0.0, -3.0), 20.0), 3.0);
        assert_eq!(wheel_rows(PixelDelta(PhysicalPosition::new(0.0, -50.0)), 20.0), 2.5);
        assert_eq!(wheel_rows(PixelDelta(PhysicalPosition::new(0.0, 10.0)), 0.0), -10.0, "a zero pitch is 1 px");
    }

    #[test]
    fn closed_overlays_draw_nothing() {
        let ov = Overlays::default();
        assert!(ov.hint_draw().is_none());
        assert!(ov.palette_draw().is_none());
        assert!(ov.copy_draw().is_none());
    }

    const LAYERS: [Layer; 9] = [
        Layer::Confirm,
        Layer::Palette,
        Layer::Help,
        Layer::Menu,
        Layer::Rename,
        Layer::Hint,
        Layer::Copy,
        Layer::Search,
        Layer::Welcome,
    ];

    /// A window's open layers after `owner` opens: `App::take_keyboard`'s rule
    /// as a pure state machine.
    fn open(layers: &[Layer], owner: Layer) -> Vec<Layer> {
        let mut v: Vec<Layer> = layers.iter().copied().filter(|l| *l != owner && l.stays_under(owner)).collect();
        v.push(owner);
        v.sort();
        v
    }

    /// Whatever opens is the layer the input reaches — the top of the open
    /// ones, under a confirmation at most — after ANY sequence of opens: every
    /// pair that can be open together is ordered by `Layer`, which the keys,
    /// the pointer and the draw pass all follow.
    #[test]
    fn whatever_opens_is_on_top_after_any_sequence() {
        let mut seen = 0;
        for a in LAYERS {
            for b in LAYERS {
                for c in LAYERS {
                    let mut layers = Vec::new();
                    for owner in [a, b, c] {
                        layers = open(&layers, owner);
                        let top = match owner {
                            Layer::Confirm => layers.first(),
                            _ => layers.iter().find(|l| **l != Layer::Confirm),
                        };
                        assert_eq!(top, Some(&owner), "{a:?} {b:?} {c:?}: {layers:?}");
                        seen += 1;
                    }
                }
            }
        }
        assert_eq!(seen, 3 * 9 * 9 * 9);
    }

    /// The search bar never sits under the help: typing would edit a query the
    /// help's scrim hides (and scroll the terminal behind it). Whichever opens
    /// second wins.
    #[test]
    fn the_help_and_the_search_bar_take_the_window_from_each_other() {
        assert_eq!(open(&open(&[], Layer::Search), Layer::Help), [Layer::Help]);
        assert_eq!(open(&open(&[], Layer::Help), Layer::Search), [Layer::Search]);
        // Hint mode / copy-mode start over the help (closing it) instead of
        // silently refusing the chord.
        assert_eq!(open(&[Layer::Help], Layer::Hint), [Layer::Hint]);
        assert_eq!(open(&[Layer::Help, Layer::Search], Layer::Copy), [Layer::Copy]);
    }

    /// The layers that cover the search bar on every path hand it back; a
    /// menu also keeps copy-mode (its Copy row copies copy-mode's selection).
    #[test]
    fn the_search_bar_stays_under_the_palette_a_menu_and_a_rename() {
        for owner in [Layer::Palette, Layer::Menu, Layer::Rename, Layer::Confirm] {
            assert_eq!(open(&[Layer::Search], owner), [owner, Layer::Search], "{owner:?}");
        }
        assert_eq!(open(&[Layer::Copy, Layer::Search], Layer::Menu), [Layer::Menu, Layer::Copy, Layer::Search]);
        assert_eq!(open(&[Layer::Copy], Layer::Palette), [Layer::Palette]);
        assert_eq!(open(&[Layer::Hint], Layer::Menu), [Layer::Menu]);
    }

    /// A confirmation keeps the window as it was under it, and nothing that
    /// opens later closes it.
    #[test]
    fn a_confirmation_keeps_everything_and_is_never_closed() {
        let all: Vec<Layer> = LAYERS.iter().copied().filter(|l| *l != Layer::Confirm).collect();
        let mut with = all.clone();
        with.insert(0, Layer::Confirm);
        assert_eq!(open(&all, Layer::Confirm), with);
        for owner in LAYERS {
            assert!(open(&[Layer::Confirm], owner).contains(&Layer::Confirm), "{owner:?}");
        }
    }

    /// The welcome splash is gone once anything takes the window — save a
    /// confirmation, which hands the window back exactly as it was.
    #[test]
    fn every_layer_takes_the_welcome_away() {
        for owner in LAYERS.iter().copied().filter(|l| !matches!(l, Layer::Welcome | Layer::Confirm)) {
            assert!(!open(&[Layer::Welcome], owner).contains(&Layer::Welcome), "{owner:?}");
        }
    }
}
