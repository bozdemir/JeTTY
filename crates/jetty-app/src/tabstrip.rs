//! The main window's tab strip under the pointer — a press on a window
//! control, a tab, its "×", the "+" or empty strip; the double-click; a tab
//! held along the strip (drag to reorder) or pulled off it (tear out) — as
//! decisions over the strip's hit geometry (`jetty_render::TabBar`), the way
//! `gridmouse` holds the grid's.
//!
//! Nothing here touches a window or a tab: the window event handler performs
//! what these return, so the gestures are testable without either.

use std::time::{Duration, Instant};

use jetty_render::{CtrlHover, Rect, TabBar};

use crate::app::TabId;
use crate::input;

/// The longest gap between the two presses of a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// The double-click's distance (logical px) between the presses.
pub(crate) const DOUBLE_CLICK_SLOP: f32 = 5.0;
/// Pointer travel (logical px) along the strip before a held tab follows it
/// into another slot: a click's jitter never reorders.
pub(crate) const REORDER_SLOP: f32 = 4.0;

/// What a left press on the strip landed on: a double-click is two quick
/// presses on the SAME one ([`double_click`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripTarget {
    /// A tab's body (a double-click renames it).
    Tab(TabId),
    /// Empty strip space (a double-click maximizes).
    Empty,
}

/// A strip press, kept to complete a double-click.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StripClick {
    at: Instant,
    x: f32,
    y: f32,
    target: StripTarget,
}

/// Whether a strip press on `target` at `(x, y)` completes a double-click with
/// the `last` one: within ~400 ms and `slop` px (5 logical), on the same
/// target. A quick second click on "+" lands on the tab the first one opened
/// (its cell covers the old "+"); renaming that tab swallowed the next command
/// typed into its title.
pub(crate) fn double_click(
    last: Option<StripClick>,
    now: Instant,
    x: f32,
    y: f32,
    target: StripTarget,
    slop: f32,
) -> bool {
    last.is_some_and(|c| {
        c.target == target
            && now.duration_since(c.at) <= DOUBLE_CLICK
            && (x - c.x).abs() <= slop
            && (y - c.y).abs() <= slop
    })
}

/// What a left press on the strip asks the window for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripPress {
    /// A window control: "?", "⚙", "─", "▢" or "✕".
    Control(CtrlHover),
    /// Tab `i`'s "×": ask before closing it.
    CloseTab(usize),
    /// The "+": a new tab.
    NewTab,
    /// A double-click on tab `i`: rename it inline.
    Rename(usize),
    /// A single press on tab `i`: select it and hold it — along the strip it
    /// follows the pointer, pulled off the strip it tears out ([`TabDrag`]).
    Hold(usize),
    /// Empty strip: a double-click maximizes / restores, a single press
    /// moves the window.
    Empty { double: bool },
    /// The second click of a double-click on the tab being renamed: its edit
    /// stays as it is.
    Nothing,
}

/// Decide a left press at `(x, y)` on the strip laid out as `bar` (its rects
/// where the drawn bar has them; `tab_rects[i]` is tab `ids[i]`). `renaming`
/// is the tab being renamed inline, `last` the previous strip press — taken,
/// and set again by a press that can begin a double-click (a tab, empty
/// strip). `slop` is the double-click distance in physical px.
#[allow(clippy::too_many_arguments)]
pub(crate) fn press(
    bar: &TabBar,
    ids: &[TabId],
    renaming: Option<TabId>,
    last: &mut Option<StripClick>,
    now: Instant,
    x: f32,
    y: f32,
    slop: f32,
) -> StripPress {
    // Taken here: a press on a window control, "+" or a "×" leaves no click
    // for a double-click to complete.
    let prev = last.take();
    let hit = |r: &Rect| input::point_in(r, x, y);
    // The window controls first (the rightmost region).
    for (r, c) in [
        (&bar.help_rect, CtrlHover::Help),
        (&bar.settings_rect, CtrlHover::Settings),
        (&bar.close_rect, CtrlHover::Close),
        (&bar.max_rect, CtrlHover::Max),
        (&bar.min_rect, CtrlHover::Min),
    ] {
        if hit(r) {
            return StripPress::Control(c);
        }
    }
    // A "×" before the tab body it sits on.
    if let Some(i) = bar.close_rects.iter().position(hit) {
        return StripPress::CloseTab(i);
    }
    if hit(&bar.plus_rect) {
        return StripPress::NewTab;
    }
    if let Some(i) = bar.tab_rects.iter().position(hit) {
        let Some(&id) = ids.get(i) else { return StripPress::Nothing };
        let target = StripTarget::Tab(id);
        if double_click(prev, now, x, y, target, slop) {
            // A double-click on the tab already being renamed must not reset
            // its edit buffer (it would discard the user's typing).
            return if renaming == Some(id) { StripPress::Nothing } else { StripPress::Rename(i) };
        }
        *last = Some(StripClick { at: now, x, y, target });
        return StripPress::Hold(i);
    }
    let double = double_click(prev, now, x, y, StripTarget::Empty, slop);
    if !double {
        *last = Some(StripClick { at: now, x, y, target: StripTarget::Empty });
    }
    StripPress::Empty { double }
}

/// A tab held on the strip since a press on it ([`StripPress::Hold`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabDrag {
    /// The held tab, by identity: the drag ends with that tab, never with
    /// another tab's removal.
    pub tab: TabId,
    /// Pulled off the strip (`detached::tearing`): a release detaches it.
    pub tearing: bool,
    /// The press's x until the pointer travels [`REORDER_SLOP`] along the
    /// strip from it; from then on the tab follows the pointer.
    press_x: Option<f32>,
}

/// What the window does after [`TabDrag::moved`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DragMove {
    /// `tearing` flipped: swap the pointer (grabbing while tearing).
    pub tear_changed: bool,
    /// Move the held tab into the slot under the pointer ([`reorder_slot`]).
    pub reorder: bool,
}

impl TabDrag {
    /// Hold `tab`, pressed at `x`.
    pub(crate) fn new(tab: TabId, x: f32) -> Self {
        TabDrag { tab, tearing: false, press_x: Some(x) }
    }

    /// The pointer moved to `x` with the tab held — off the strip when
    /// `tearing` (and detaching is possible). `slop` is [`REORDER_SLOP`] in
    /// physical px.
    pub(crate) fn moved(&mut self, x: f32, tearing: bool, slop: f32) -> DragMove {
        let tear_changed = tearing != self.tearing;
        self.tearing = tearing;
        if self.press_x.is_some_and(|px| (x - px).abs() >= slop) {
            self.press_x = None;
        }
        DragMove { tear_changed, reorder: !tearing && self.press_x.is_none() }
    }
}

/// The slot the held tab `from` moves into with the pointer at `x`: the drawn
/// tab under it, unless that is its own.
pub(crate) fn reorder_slot(x: f32, tab_rects: &[Rect], from: usize) -> Option<usize> {
    crate::detached::reorder_target(x, tab_rects).filter(|&to| to != from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOP: f32 = 5.0;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    fn ids(n: u64) -> Vec<TabId> {
        (1..=n).map(TabId).collect()
    }

    /// The main strip at 1× for `n` tabs with tab `active` active and the
    /// overflow window kept at `first` (the app's `tab_first`).
    fn strip(width: u32, n: usize, active: usize, first: usize) -> TabBar {
        let tabs: Vec<(String, bool)> = (0..n).map(|i| (String::new(), i == active)).collect();
        jetty_render::build_tab_bar_styled(
            width,
            &tabs,
            &jetty_core::Theme::by_name("catppuccin_mocha"),
            None,
            CtrlHover::None,
            None,
            &mut jetty_render::MonoMeasure(jetty_render::CHROME_ADVANCE),
            jetty_render::ChromeMetrics::DEFAULT,
            &[],
            &jetty_render::TabBarOpts { first, ..Default::default() },
        )
    }

    fn center(r: &Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn a_double_click_is_two_quick_presses_on_the_same_target() {
        let t = Instant::now();
        let tab = |n| StripTarget::Tab(TabId(n));
        let last = Some(StripClick { at: t, x: 100.0, y: 10.0, target: tab(1) });
        assert!(double_click(last, ms(t, 300), 103.0, 12.0, tab(1), SLOP));
        assert!(!double_click(last, ms(t, 401), 100.0, 10.0, tab(1), SLOP), "too slow");
        assert!(!double_click(last, ms(t, 100), 106.0, 10.0, tab(1), SLOP), "too far");
        // The slop is logical: 6 px is near enough at 2× (10 px of slop).
        assert!(double_click(last, ms(t, 100), 106.0, 10.0, tab(1), 10.0));
        // Another target under the same spot: a tab that slid in, the strip.
        assert!(!double_click(last, ms(t, 100), 100.0, 10.0, tab(2), SLOP));
        assert!(!double_click(last, ms(t, 100), 100.0, 10.0, StripTarget::Empty, SLOP));
        let empty = Some(StripClick { at: t, x: 100.0, y: 10.0, target: StripTarget::Empty });
        assert!(double_click(empty, ms(t, 100), 100.0, 10.0, StripTarget::Empty, SLOP));
        assert!(!double_click(empty, ms(t, 100), 100.0, 10.0, tab(1), SLOP));
        assert!(!double_click(None, ms(t, 100), 100.0, 10.0, tab(3), SLOP));
    }

    #[test]
    fn presses_land_on_the_controls_the_crosses_the_plus_and_the_tabs() {
        let t = Instant::now();
        let bar = strip(1000, 3, 0, 0);
        let mut last = None;
        let at = |r: &Rect, last: &mut Option<StripClick>| {
            let (x, y) = center(r);
            press(&bar, &ids(3), None, last, t, x, y, SLOP)
        };
        assert_eq!(at(&bar.help_rect, &mut last), StripPress::Control(CtrlHover::Help));
        assert_eq!(at(&bar.settings_rect, &mut last), StripPress::Control(CtrlHover::Settings));
        assert_eq!(at(&bar.min_rect, &mut last), StripPress::Control(CtrlHover::Min));
        assert_eq!(at(&bar.max_rect, &mut last), StripPress::Control(CtrlHover::Max));
        assert_eq!(at(&bar.close_rect, &mut last), StripPress::Control(CtrlHover::Close));
        assert_eq!(at(&bar.close_rects[1], &mut last), StripPress::CloseTab(1));
        assert_eq!(at(&bar.plus_rect, &mut last), StripPress::NewTab);
        assert_eq!(last, None, "no control, cross or plus begins a double-click");
        // A tab's body: hold it. Empty strip (between the "+" and the "?").
        let body = Rect { w: bar.tab_rects[2].w / 4.0, ..bar.tab_rects[2] };
        assert_eq!(at(&body, &mut last), StripPress::Hold(2));
        let gap = Rect { x: bar.plus_rect.x + bar.plus_rect.w + 5.0, w: 10.0, ..bar.plus_rect };
        assert_eq!(at(&gap, &mut last), StripPress::Empty { double: false });
    }

    #[test]
    fn a_double_click_renames_a_tab_and_maximizes_on_empty_strip() {
        let t = Instant::now();
        let bar = strip(1000, 3, 0, 0);
        let (x, y) = (bar.tab_rects[1].x + 10.0, 10.0);
        let mut last = None;
        assert_eq!(press(&bar, &ids(3), None, &mut last, t, x, y, SLOP), StripPress::Hold(1));
        assert_eq!(press(&bar, &ids(3), None, &mut last, ms(t, 200), x, y, SLOP), StripPress::Rename(1));
        // A third quick click starts over (a single press).
        assert_eq!(press(&bar, &ids(3), None, &mut last, ms(t, 300), x, y, SLOP), StripPress::Hold(1));
        // On the tab already being renamed, a double-click leaves the edit be.
        let renaming = Some(TabId(2));
        assert_eq!(press(&bar, &ids(3), renaming, &mut last, ms(t, 400), x, y, SLOP), StripPress::Nothing);
        let gx = bar.plus_rect.x + bar.plus_rect.w + 20.0;
        let mut last = None;
        assert_eq!(press(&bar, &ids(3), None, &mut last, t, gx, y, SLOP), StripPress::Empty { double: false });
        assert_eq!(press(&bar, &ids(3), None, &mut last, ms(t, 150), gx, y, SLOP), StripPress::Empty { double: true });
    }

    #[test]
    fn a_quick_double_click_on_plus_never_renames_the_tab_it_opened() {
        // Full-width tabs: the tab "+" opens takes the old "+" cell.
        let t = Instant::now();
        let before = strip(1000, 2, 1, 0);
        let (x, y) = center(&before.plus_rect);
        let mut last = None;
        assert_eq!(press(&before, &ids(2), None, &mut last, t, x, y, SLOP), StripPress::NewTab);
        let after = strip(1000, 3, 2, 0);
        assert!(input::point_in(&after.tab_rects[2], x, y), "the new tab covers the old \"+\"");
        assert_eq!(press(&after, &ids(3), None, &mut last, ms(t, 150), x, y, SLOP), StripPress::Hold(2));
    }

    #[test]
    fn clicking_a_drawn_tab_of_an_overflowed_strip_never_reorders_it() {
        // 1000 px Pill at 1×: 12 of 15 tabs drawn, the active last one puts
        // the window at 3..15. Pressing the leftmost drawn tab (3) activated
        // it, the strip re-windowed to 0..12 under the pointer, and a 1 px
        // move "dragged" tab 3 onto whatever tab now sat there — one slot per
        // motion event, down to index 0.
        let t = Instant::now();
        let shown = strip(1000, 15, 14, 0);
        assert_eq!(shown.first, 3);
        let (x, y) = center(&shown.tab_rects[3]);
        let mut last = None;
        assert_eq!(press(&shown, &ids(15), None, &mut last, t, x, y, SLOP), StripPress::Hold(3));
        let mut drag = TabDrag::new(TabId(4), x);
        // Tab 3 is active now: the strip, laid out with the window it kept,
        // still has tab 3 under the pointer.
        let after = strip(1000, 15, 3, shown.first);
        assert!(input::point_in(&after.tab_rects[3], x, y));
        // A press's jitter is no drag; past the slop, over its own slot,
        // nothing moves either.
        assert_eq!(drag.moved(x + 1.0, false, REORDER_SLOP), DragMove::default());
        let step = drag.moved(x + REORDER_SLOP + 1.0, false, REORDER_SLOP);
        assert!(step.reorder);
        assert_eq!(reorder_slot(x + REORDER_SLOP + 1.0, &after.tab_rects, 3), None);
        // Over the next tab it takes that slot.
        let (nx, _) = center(&after.tab_rects[4]);
        assert_eq!(reorder_slot(nx, &after.tab_rects, 3), Some(4));
    }

    #[test]
    fn a_held_tab_tears_off_the_strip_and_comes_back() {
        let mut drag = TabDrag::new(TabId(1), 100.0);
        assert_eq!(drag.moved(100.0, true, REORDER_SLOP), DragMove { tear_changed: true, reorder: false });
        assert!(drag.tearing);
        assert_eq!(drag.moved(160.0, true, REORDER_SLOP), DragMove { tear_changed: false, reorder: false });
        // Back on the strip, already past the slop: it follows the pointer.
        assert_eq!(drag.moved(160.0, false, REORDER_SLOP), DragMove { tear_changed: true, reorder: true });
        assert!(!drag.tearing);
    }
}
