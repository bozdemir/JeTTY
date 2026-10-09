//! Per-tab chrome state — unseen-activity badges, OSC 9;4 progress, the
//! per-tab color and the smart-title inputs — and the pure rules that drive
//! it. Kept apart from `Tab` (which owns a live PTY) so every rule is
//! unit-tested; `Tab::meta` carries it, so it travels with a tab between the
//! main window and a detached one.

use std::path::Path;

use jetty_core::Progress;
use jetty_render::{TabActivity, TabDeco};

/// Where a tab's title comes from when the user has not renamed it (config
/// `tab_title`). A manual rename always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TabTitleMode {
    /// The program's OSC 0/2 title, else "Tab N" (the pre-v2 behavior).
    #[default]
    Osc,
    /// The program's OSC title; without one, the running command (shell
    /// integration's OSC 133 C … D) or the shell's directory ("~" at home).
    Auto,
}

impl TabTitleMode {
    /// Config string → mode; unknown values read as [`TabTitleMode::Osc`].
    pub(crate) fn from_config(s: &str) -> TabTitleMode {
        match s {
            "auto" => TabTitleMode::Auto,
            _ => TabTitleMode::Osc,
        }
    }

    pub(crate) fn to_config(self) -> &'static str {
        match self {
            TabTitleMode::Osc => "osc",
            TabTitleMode::Auto => "auto",
        }
    }
}

/// The window border (config `window_border`): a thin ring on the window's
/// own rounded shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WindowBorder {
    /// No ring (the pre-v2 look).
    #[default]
    None,
    /// A ring in the accent (or the active tab's color) while the window has
    /// keyboard focus; nothing otherwise.
    Focus,
    /// Always a ring: the accent / tab color while focused, the muted border
    /// shade while not.
    Always,
}

impl WindowBorder {
    pub(crate) const ALL: [WindowBorder; 3] = [WindowBorder::None, WindowBorder::Focus, WindowBorder::Always];

    /// Config string → mode; unknown values read as [`WindowBorder::None`].
    pub(crate) fn from_config(s: &str) -> WindowBorder {
        match s {
            "focus" => WindowBorder::Focus,
            "always" => WindowBorder::Always,
            _ => WindowBorder::None,
        }
    }

    pub(crate) fn to_config(self) -> &'static str {
        match self {
            WindowBorder::None => "none",
            WindowBorder::Focus => "focus",
            WindowBorder::Always => "always",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            WindowBorder::None => "None",
            WindowBorder::Focus => "Focus ring",
            WindowBorder::Always => "Always",
        }
    }

    /// The ring color for a window (`None` = draw no ring): `lit` (the accent or
    /// the active tab's color) while focused, `dim` (the palette's border
    /// shade) while not — or nothing, depending on the mode. A fullscreen
    /// window has no edge to mark.
    pub(crate) fn ring_color(self, focused: bool, fullscreen: bool, lit: [u8; 3], dim: [u8; 3]) -> Option<[u8; 3]> {
        if fullscreen {
            return None;
        }
        match (self, focused) {
            (WindowBorder::None, _) | (WindowBorder::Focus, false) => None,
            (_, true) => Some(lit),
            (WindowBorder::Always, false) => Some(dim),
        }
    }
}

/// The chrome settings as one value (the `tab_style`, `tab_close_button`,
/// `tab_bar_opacity`, `progress_bar`, `window_border` and `tab_title` keys), so
/// a reload, a palette command and Settings apply them through one setter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChromeSettings {
    pub(crate) tab_style: jetty_render::TabStyle,
    pub(crate) close_button: jetty_render::CloseButton,
    pub(crate) bar_opacity: bool,
    pub(crate) progress_bar: bool,
    pub(crate) window_border: WindowBorder,
    pub(crate) title_mode: TabTitleMode,
}

impl ChromeSettings {
    pub(crate) fn from_config(cfg: &crate::config::Config) -> ChromeSettings {
        ChromeSettings {
            tab_style: jetty_render::TabStyle::from_config(&cfg.tab_style),
            close_button: jetty_render::CloseButton::from_config(&cfg.tab_close_button),
            bar_opacity: cfg.tab_bar_opacity,
            progress_bar: cfg.progress_bar,
            window_border: WindowBorder::from_config(&cfg.window_border),
            title_mode: TabTitleMode::from_config(&cfg.tab_title),
        }
    }
}

/// The window ring's color for a window whose active tab has per-tab color
/// `tab_color`: that color (kept ≥3:1 readable) or the accent while focused,
/// the palette's muted border shade while not (in `Always` mode). `None` = no
/// ring this frame. Shared by the main window, detached windows and jetty-shot.
pub(crate) fn ring_rgb(
    border: WindowBorder,
    focused: bool,
    fullscreen: bool,
    theme: &jetty_core::Theme,
    tab_color: Option<u8>,
) -> Option<[u8; 3]> {
    if border == WindowBorder::None {
        return None; // the common case: not even the palette lookup
    }
    let ui = jetty_render::UiPalette::cached(theme);
    let lit = tab_color
        .and_then(|c| jetty_render::tab_color_rgb(theme, c))
        .map(|c| ui.readable(c, jetty_render::UiPalette::ACCENT_FLOOR))
        .unwrap_or(ui.accent);
    border.ring_color(focused, fullscreen, lit, ui.border)
}

/// How many times a tab re-tries to name the foreground process of a command
/// whose OSC 133 C arrived before the shell forked it (bash prints its C from
/// PS0, zsh from preexec — both before the fork). Each retry rides a drain that
/// happens anyway, or one of a short backoff of timed wakes
/// ([`title_recheck_delay`]) for a silent command — bounded, never a poll.
pub(crate) const FG_RETRIES: u8 = 4;

/// Delay of the next timed foreground re-check with `retries_left` tries left:
/// 60, 120, 240, 480 ms (≈0.9 s in all, then it gives up until the next mark).
pub(crate) fn title_recheck_delay(retries_left: u8) -> std::time::Duration {
    let step = FG_RETRIES.saturating_sub(retries_left).min(4);
    std::time::Duration::from_millis(60u64 << step)
}

/// A tab's chrome state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TabMeta {
    /// Unseen activity, shown as a badge while the tab is INACTIVE; consumed
    /// when it is shown as the active tab (and when it moves windows).
    pub(crate) activity: TabActivity,
    /// The OSC 9;4 progress its program reports.
    pub(crate) progress: Option<Progress>,
    /// The per-tab color: an ANSI palette index 1..=6 (follows the theme).
    pub(crate) color: Option<u8>,
    /// The program's OSC 0/2 title (sanitized and clipped); `None` = never set
    /// or reset. Kept even under a manual rename so `tab_title` changes and a
    /// later reset can fall back correctly.
    pub(crate) osc_title: Option<String>,
    /// The smart title in [`TabTitleMode::Auto`]: the running command, else the
    /// shell's directory. `None` until first derived.
    pub(crate) smart_title: Option<String>,
    /// See [`FG_RETRIES`].
    pub(crate) fg_retries: u8,
}

impl TabMeta {
    /// The tab became the visible tab of another window (detach / reattach):
    /// its unseen activity is consumed there. Its color, progress and titles
    /// travel with it.
    pub(crate) fn moved_to_window(&mut self) {
        self.activity = TabActivity::None;
    }

    /// What the tab bar draws for this tab beyond its title.
    pub(crate) fn deco(&self) -> TabDeco {
        TabDeco { activity: self.activity, progress: self.progress, color: self.color }
    }
}

/// The badge of an INACTIVE tab after one of its commands completed (OSC 133
/// D): Done or Failed, never weakening a stronger badge (Failed > Bell > Done >
/// Output). A completion with an unknown exit status counts as done.
pub(crate) fn activity_after_completion(current: TabActivity, exit_code: Option<i32>) -> TabActivity {
    let failed = matches!(exit_code, Some(c) if c != 0);
    current.max(if failed { TabActivity::Failed } else { TabActivity::Done })
}

/// The title a NOT manually renamed tab displays: the program's OSC title,
/// else — in [`TabTitleMode::Auto`] — the smart title, else the "Tab N"
/// default.
pub(crate) fn display_title(mode: TabTitleMode, osc: Option<&str>, smart: Option<&str>, default: &str) -> String {
    match (osc, mode, smart) {
        (Some(t), _, _) => t.to_string(),
        (None, TabTitleMode::Auto, Some(s)) if !s.is_empty() => s.to_string(),
        _ => default.to_string(),
    }
}

/// The smart title: the running command's name when one runs, else the last
/// component of the shell's directory — "~" for the home directory itself, "/"
/// for the root. `None` when neither is known.
pub(crate) fn smart_title(running: Option<&str>, cwd: Option<&Path>, home: Option<&Path>) -> Option<String> {
    if let Some(cmd) = running.map(str::trim).filter(|c| !c.is_empty()) {
        return Some(cmd.to_string());
    }
    let cwd = cwd?;
    if home.is_some_and(|h| h == cwd) {
        return Some("~".to_string());
    }
    match cwd.file_name() {
        Some(name) => Some(name.to_string_lossy().into_owned()),
        None => Some(cwd.to_string_lossy().into_owned()).filter(|s| !s.is_empty()),
    }
}

/// Most bytes a paste grows a tab's rename buffer to: the 1 KiB a program's
/// OSC 0/2 title is clipped to. A title is one short line, and the whole of it
/// becomes the window's title too.
pub(crate) const RENAME_PASTE_MAX: usize = 1024;

/// Paste `clip` into a tab's rename buffer: its first non-blank line, control
/// characters dropped, the buffer stopping at [`RENAME_PASTE_MAX`] bytes (on a
/// character boundary).
pub(crate) fn paste_into_title(buf: &mut String, clip: &str) {
    let line = clip.trim_start().lines().next().unwrap_or("");
    for c in line.chars().filter(|c| !c.is_control()) {
        if buf.len() + c.len_utf8() > RENAME_PASTE_MAX {
            break;
        }
        buf.push(c);
    }
}

/// The mouse wheel over the tab strip flips through the tabs instead of
/// scrolling the grid below it: one notch (3 lines) per tab — a touchpad's
/// fractional deltas accumulate to the same — wheel up / left = the previous
/// tab, down / right = the next.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TabWheel {
    acc: f32,
}

impl TabWheel {
    /// Wheel lines that make one tab step (one mouse-wheel notch).
    const LINES_PER_TAB: f32 = 3.0;

    /// Feed a wheel delta — `lines` vertical (positive = up), `cols`
    /// horizontal (positive = left), as `input::wheel_lines` / `wheel_columns`
    /// measure them — and return the tab steps it makes now (negative = toward
    /// the first tab). A reversal drops the remainder of the old direction.
    pub(crate) fn add(&mut self, lines: f32, cols: f32) -> i32 {
        let d = lines + cols;
        if !d.is_finite() || d == 0.0 {
            return 0;
        }
        if self.acc != 0.0 && self.acc.signum() != d.signum() {
            self.acc = 0.0;
        }
        self.acc += d;
        let steps = (self.acc / Self::LINES_PER_TAB).trunc();
        self.acc -= steps * Self::LINES_PER_TAB;
        -(steps as i32)
    }
}

/// The tab a wheel step from `active` lands on among `n` tabs: clamped at the
/// first / last tab (a spun wheel stops at the end instead of wrapping).
pub(crate) fn wheel_tab_target(active: usize, n: usize, steps: i32) -> usize {
    let last = n.saturating_sub(1) as i64;
    (active as i64 + i64::from(steps)).clamp(0, last) as usize
}

/// The user's home directory, read once (smart titles show it as "~").
pub(crate) fn home_dir() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(dirs::home_dir).as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetty_core::ProgressState;
    use std::path::PathBuf;

    #[test]
    fn completions_badge_with_precedence() {
        use TabActivity::*;
        assert_eq!(activity_after_completion(None, Some(0)), Done);
        assert_eq!(activity_after_completion(None, Option::None), Done, "unknown exit = done");
        assert_eq!(activity_after_completion(None, Some(2)), Failed);
        assert_eq!(activity_after_completion(Output, Some(0)), Done, "done outranks output");
        assert_eq!(activity_after_completion(Bell, Some(0)), Bell, "a bell is not hidden by a success");
        assert_eq!(activity_after_completion(Bell, Some(1)), Failed, "failure outranks the bell");
        assert_eq!(activity_after_completion(Failed, Some(0)), Failed, "a later success never clears a failure");
        assert_eq!(activity_after_completion(Done, Some(130)), Failed);
    }

    #[test]
    fn title_precedence_osc_then_smart_then_default() {
        use TabTitleMode::*;
        assert_eq!(display_title(Osc, Some("vim"), Some("src"), "Tab 2"), "vim");
        assert_eq!(display_title(Auto, Some("vim"), Some("src"), "Tab 2"), "vim", "a program's title wins");
        assert_eq!(display_title(Osc, None, Some("src"), "Tab 2"), "Tab 2", "osc mode = today");
        assert_eq!(display_title(Auto, None, Some("src"), "Tab 2"), "src");
        assert_eq!(display_title(Auto, None, None, "Tab 2"), "Tab 2");
        assert_eq!(display_title(Auto, None, Some(""), "Tab 2"), "Tab 2");
        for m in [Osc, Auto] {
            assert_eq!(TabTitleMode::from_config(m.to_config()), m);
        }
        assert_eq!(TabTitleMode::from_config("bogus"), Osc);
    }

    #[test]
    fn a_paste_renames_with_one_clean_line() {
        let pasted = |buf: &str, clip: &str| {
            let mut b = buf.to_string();
            paste_into_title(&mut b, clip);
            b
        };
        assert_eq!(pasted("build ", "logs\nmore\n"), "build logs", "the first line only");
        assert_eq!(pasted("", "\n\n  api server\r\nx"), "api server", "its first non-blank line");
        assert_eq!(pasted("a", "\x1b[31mred\x07\tx"), "a[31mredx", "no control character gets in");
        assert_eq!(pasted("t", ""), "t");
        // A huge one-line clipboard stops at the cap, on a character boundary.
        let long = pasted("", &"ş".repeat(4096));
        assert!(long.len() <= RENAME_PASTE_MAX && long.len() > RENAME_PASTE_MAX - 2, "{}", long.len());
        assert!(long.chars().all(|c| c == 'ş'));
        assert_eq!(pasted(&long, "more"), long, "a full buffer takes no more");
    }

    #[test]
    fn smart_title_prefers_the_running_command_then_the_directory() {
        let home = PathBuf::from("/home/u");
        let h = Some(home.as_path());
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(smart_title(Some("cargo"), Some(&p("/home/u/proj")), h).as_deref(), Some("cargo"));
        assert_eq!(smart_title(None, Some(&p("/home/u/proj")), h).as_deref(), Some("proj"));
        assert_eq!(smart_title(Some("  "), Some(&p("/home/u/proj")), h).as_deref(), Some("proj"));
        assert_eq!(smart_title(None, Some(&home), h).as_deref(), Some("~"));
        assert_eq!(smart_title(None, Some(&p("/")), h).as_deref(), Some("/"));
        assert_eq!(smart_title(None, Some(&p("/home/u/")), h).as_deref(), Some("~"), "trailing slash");
        assert_eq!(smart_title(None, None, h), None);
        assert_eq!(smart_title(None, Some(&p("/srv/www")), None).as_deref(), Some("www"));
    }

    #[test]
    fn color_and_progress_survive_a_detach_and_reattach() {
        // The exact transfer the app performs: the tab is TAKEN out of the
        // main window's list (detached::take_tab), becomes the visible tab of
        // its own window (moved_to_window), then is pushed back and becomes
        // the active tab (reattach_index) — its color and progress ride along,
        // its unseen badge is consumed.
        let tab = TabMeta {
            activity: TabActivity::Failed,
            progress: Some(Progress { state: ProgressState::Normal, value: Some(40) }),
            color: Some(5),
            osc_title: Some("build".into()),
            smart_title: Some("proj".into()),
            fg_retries: 0,
        };
        let mut main = vec![TabMeta::default(), tab.clone(), TabMeta::default()];
        let mut detached = crate::detached::take_tab(&mut main, 1).expect("detach");
        detached.moved_to_window();
        assert_eq!(detached.color, Some(5));
        assert_eq!(detached.progress, tab.progress);
        assert_eq!(detached.activity, TabActivity::None);
        main.push(detached);
        let at = crate::detached::reattach_index(main.len());
        main[at].moved_to_window();
        assert_eq!(main[at].color, Some(5), "the color survives the round trip");
        assert_eq!(main[at].osc_title.as_deref(), Some("build"));
        assert_eq!(main[at].deco().color, Some(5));
        assert_eq!(main[at].deco().progress, tab.progress);
    }

    #[test]
    fn a_wheel_notch_over_the_tab_strip_is_one_tab() {
        let mut w = TabWheel::default();
        // A mouse notch is LineDelta 1.0 = 3 lines: down → next, up → previous.
        assert_eq!(w.add(-3.0, 0.0), 1);
        assert_eq!(w.add(3.0, 0.0), -1);
        assert_eq!(w.add(-9.0, 0.0), 3, "three notches in one event");
        // A touchpad's small pixel deltas accumulate to a step.
        let steps: i32 = (0..10).map(|_| w.add(-0.5, 0.0)).sum();
        assert_eq!(steps, 1, "5 lines of small deltas = one tab (2 left over)");
        // Reversing drops the old direction's remainder: no lurch.
        assert_eq!(w.add(1.0, 0.0), 0);
        assert_eq!(w.add(2.0, 0.0), -1);
        // Horizontal scrolling counts too (left = previous).
        assert_eq!(w.add(0.0, 3.0), -1);
        assert_eq!(w.add(0.0, -3.0), 1);
        assert_eq!(w.add(f32::NAN, 0.0), 0);
        assert_eq!(w.add(0.0, 0.0), 0);
    }

    #[test]
    fn the_wheel_stops_at_the_first_and_last_tab() {
        assert_eq!(wheel_tab_target(2, 5, 1), 3);
        assert_eq!(wheel_tab_target(2, 5, -1), 1);
        assert_eq!(wheel_tab_target(4, 5, 1), 4, "no wrap past the last tab");
        assert_eq!(wheel_tab_target(0, 5, -3), 0, "no wrap before the first");
        assert_eq!(wheel_tab_target(1, 5, 10), 4);
        assert_eq!(wheel_tab_target(0, 1, 1), 0);
        assert_eq!(wheel_tab_target(0, 0, 1), 0);
    }

    #[test]
    fn foreground_rechecks_back_off_and_stop() {
        let delays: Vec<u64> = (1..=FG_RETRIES).rev().map(|r| title_recheck_delay(r).as_millis() as u64).collect();
        assert_eq!(delays, vec![60, 120, 240, 480]);
        assert!(delays.iter().sum::<u64>() < 1000, "bounded: under a second of wakes in all");
    }

    #[test]
    fn window_border_ring_colors() {
        let (lit, dim) = ([1, 2, 3], [9, 9, 9]);
        use WindowBorder::*;
        assert_eq!(None.ring_color(true, false, lit, dim), Option::None);
        assert_eq!(Focus.ring_color(true, false, lit, dim), Some(lit));
        assert_eq!(Focus.ring_color(false, false, lit, dim), Option::None);
        assert_eq!(Always.ring_color(true, false, lit, dim), Some(lit));
        assert_eq!(Always.ring_color(false, false, lit, dim), Some(dim));
        assert_eq!(Always.ring_color(true, true, lit, dim), Option::None, "no ring in fullscreen");
        for b in WindowBorder::ALL {
            assert_eq!(WindowBorder::from_config(b.to_config()), b);
        }
        assert_eq!(WindowBorder::from_config("thick"), None);
    }
}
