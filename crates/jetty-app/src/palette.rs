//! Command-palette action registry + fuzzy filter.
//!
//! The registry is a plain `Vec<PaletteEntry>` (a title, static keywords, and a
//! [`PaletteCmd`] tag — NO closures, which would fight the `&mut self` +
//! `event_loop` borrows in `app.rs`). `App::run_palette_cmd` matches the tag and
//! calls the EXISTING app action, so there is zero logic duplication. The registry
//! is built FRESH on open (it is ~50 short entries) and dropped on close, so the
//! dynamic theme/tab/detach entries stay current with no per-frame or auto-repeat
//! cost. Filtering runs only on a keystroke via [`filter`], which both the app and
//! the `jetty-shot` self-test share.

use jetty_core::fuzzy_match;

/// A palette action. Index-bearing variants (`SetTheme`/`SelectTab`/`Reattach`)
/// carry the resolved index at build time; `run_palette_cmd` `.get()`-guards them
/// so an index that went stale between open and Enter is a clean no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteCmd {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    DetachTab,
    OpenSettings,
    FontUp,
    FontDown,
    FontReset,
    OpacityUp,
    OpacityDown,
    ToggleCrt,
    ToggleCrtRoll,
    ToggleCrtFlicker,
    ToggleCrtJitter,
    ToggleCaretFlash,
    ToggleCaretGlow,
    TogglePerfHud,
    ShowWelcome,
    Search,
    HintMode,
    CopyMode,
    PrevPrompt,
    NextPrompt,
    Copy,
    Paste,
    ToggleLaunchAtLogin,
    ResetKeybindings,
    Hide,
    Quit,
    SetTheme(usize),
    SelectTab(usize),
    Reattach(usize),
}

/// One registry row: the human-facing `title` (fuzzy-matched + highlighted),
/// extra `keywords` matched separately (never highlighted), and the action tag.
pub struct PaletteEntry {
    pub title: String,
    pub keywords: &'static str,
    pub cmd: PaletteCmd,
}

/// A filtered result: the (owned) title, the matched TITLE character indices for
/// the highlight, and the resolved command to run on Enter.
#[derive(Clone)]
pub struct PaletteHit {
    pub title: String,
    pub indices: Vec<usize>,
    pub cmd: PaletteCmd,
}

/// Build the full palette registry: the fixed static actions, then one entry per
/// theme (`Theme: {display}`), per open tab (`Switch to tab: {title}`), and per
/// detached window (`Reattach: {title}`, only when there are any).
pub fn build_registry(
    themes: &[(String, String)],
    tabs: &[String],
    detached: &[String],
) -> Vec<PaletteEntry> {
    let statics: [(&str, &str, PaletteCmd); 30] = [
        ("New tab", "create open window shell", PaletteCmd::NewTab),
        ("Close tab", "kill remove", PaletteCmd::CloseTab),
        ("Next tab", "cycle switch forward", PaletteCmd::NextTab),
        ("Previous tab", "cycle switch back", PaletteCmd::PrevTab),
        ("Detach tab to new window", "float pop out", PaletteCmd::DetachTab),
        ("Open Settings…", "preferences config panel options", PaletteCmd::OpenSettings),
        ("Increase font size", "bigger zoom larger text", PaletteCmd::FontUp),
        ("Decrease font size", "smaller zoom text", PaletteCmd::FontDown),
        ("Reset font size", "default zoom text", PaletteCmd::FontReset),
        ("Increase opacity", "less transparent solid", PaletteCmd::OpacityUp),
        ("Decrease opacity", "more transparent see through", PaletteCmd::OpacityDown),
        ("Toggle CRT effect", "retro scanline glow", PaletteCmd::ToggleCrt),
        ("Toggle CRT roll", "retro animate", PaletteCmd::ToggleCrtRoll),
        ("Toggle CRT flicker", "retro animate", PaletteCmd::ToggleCrtFlicker),
        ("Toggle CRT jitter", "retro animate", PaletteCmd::ToggleCrtJitter),
        ("Toggle caret flash", "cursor blink", PaletteCmd::ToggleCaretFlash),
        ("Toggle caret glow", "cursor bloom", PaletteCmd::ToggleCaretGlow),
        ("Toggle performance HUD", "fps stats perf meter", PaletteCmd::TogglePerfHud),
        ("Show welcome screen", "splash about neofetch", PaletteCmd::ShowWelcome),
        ("Search scrollback…", "find grep filter", PaletteCmd::Search),
        ("Hint mode: label URLs/paths", "hint link url path hash copy open keyboard", PaletteCmd::HintMode),
        ("Copy-mode: keyboard select", "copy mode select vi cursor yank keyboard", PaletteCmd::CopyMode),
        ("Jump to previous prompt", "osc133 shell up", PaletteCmd::PrevPrompt),
        ("Jump to next prompt", "osc133 shell down", PaletteCmd::NextPrompt),
        ("Copy selection", "clipboard yank", PaletteCmd::Copy),
        ("Paste", "clipboard insert", PaletteCmd::Paste),
        ("Toggle launch at login", "autostart startup boot", PaletteCmd::ToggleLaunchAtLogin),
        ("Reset keybindings to defaults", "shortcut hotkey rebind reset keys", PaletteCmd::ResetKeybindings),
        ("Hide window", "summon dismiss minimize", PaletteCmd::Hide),
        ("Quit JeTTY", "exit close all", PaletteCmd::Quit),
    ];

    let mut v: Vec<PaletteEntry> =
        Vec::with_capacity(statics.len() + themes.len() + tabs.len() + detached.len());
    for (title, keywords, cmd) in statics {
        v.push(PaletteEntry { title: title.to_string(), keywords, cmd });
    }
    for (i, (_name, display)) in themes.iter().enumerate() {
        v.push(PaletteEntry {
            title: format!("Theme: {display}"),
            keywords: "theme colour color scheme palette",
            cmd: PaletteCmd::SetTheme(i),
        });
    }
    for (i, title) in tabs.iter().enumerate() {
        v.push(PaletteEntry {
            title: format!("Switch to tab: {title}"),
            keywords: "tab go window",
            cmd: PaletteCmd::SelectTab(i),
        });
    }
    for (i, title) in detached.iter().enumerate() {
        v.push(PaletteEntry {
            title: format!("Reattach: {title}"),
            keywords: "attach dock window",
            cmd: PaletteCmd::Reattach(i),
        });
    }
    v
}

/// Fuzzy-filter `registry` by `query`, returning resolved hits ranked best-first.
///
/// Each entry is scored against its title AND its keywords separately; the row's
/// score is the MAX of the two, but the highlight indices come ONLY from the
/// title match (empty when the row matched solely on keywords). An empty query
/// returns every entry in registry order (no scoring). Ties break on registry
/// order (a stable sort keyed by the original index), so the result is
/// deterministic.
pub fn filter(registry: &[PaletteEntry], query: &str) -> Vec<PaletteHit> {
    if query.is_empty() {
        return registry
            .iter()
            .map(|e| PaletteHit { title: e.title.clone(), indices: Vec::new(), cmd: e.cmd.clone() })
            .collect();
    }
    let mut scored: Vec<(i32, usize, PaletteHit)> = Vec::new();
    for (i, e) in registry.iter().enumerate() {
        let title_m = fuzzy_match(query, &e.title);
        let kw_m = fuzzy_match(query, e.keywords);
        let best = match (title_m.as_ref().map(|m| m.score), kw_m.map(|m| m.score)) {
            (None, None) => continue,
            (a, b) => a.unwrap_or(i32::MIN).max(b.unwrap_or(i32::MIN)),
        };
        // Highlight indices come ONLY from the title match.
        let indices = title_m.map(|m| m.indices).unwrap_or_default();
        scored.push((best, i, PaletteHit { title: e.title.clone(), indices, cmd: e.cmd.clone() }));
    }
    // Score desc, then registry order (stable tiebreak on the original index).
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Vec<PaletteEntry> {
        let themes = jetty_core::theme_list();
        let tabs = vec!["Tab 1".to_string(), "Tab 2".to_string()];
        build_registry(&themes, &tabs, &[])
    }

    #[test]
    fn registry_has_all_themes_and_one_per_tab() {
        let themes = jetty_core::theme_list();
        let tabs = vec!["Tab 1".to_string(), "Tab 2".to_string()];
        let r = build_registry(&themes, &tabs, &[]);
        let theme_entries = r.iter().filter(|e| e.title.starts_with("Theme: ")).count();
        assert_eq!(theme_entries, jetty_core::theme_count());
        let tab_entries = r.iter().filter(|e| e.title.starts_with("Switch to tab: ")).count();
        assert_eq!(tab_entries, 2);
        // No Reattach entries when there are no detached windows.
        assert!(!r.iter().any(|e| e.title.starts_with("Reattach: ")));
        // Dynamic indices are captured at build time.
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::SelectTab(1)));
    }

    #[test]
    fn empty_query_returns_all_in_registry_order() {
        let r = reg();
        let hits = filter(&r, "");
        assert_eq!(hits.len(), r.len());
        for (h, e) in hits.iter().zip(r.iter()) {
            assert_eq!(h.title, e.title);
            assert!(h.indices.is_empty(), "empty query must not highlight");
        }
    }

    #[test]
    fn title_vs_keyword_max_and_title_only_highlight() {
        // "Paste" has title "Paste", keywords "clipboard insert". Query "clip"
        // matches the KEYWORDS, not the title → included with EMPTY highlight.
        let r = vec![PaletteEntry {
            title: "Paste".to_string(),
            keywords: "clipboard insert",
            cmd: PaletteCmd::Paste,
        }];
        let hits = filter(&r, "clip");
        assert_eq!(hits.len(), 1, "keyword match must include the row");
        assert!(hits[0].indices.is_empty(), "keyword-only match must not highlight the title");

        // A title match DOES highlight, and the score is the max of the two.
        let hits = filter(&r, "past");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].indices, vec![0, 1, 2, 3], "title match highlights its chars");
    }

    #[test]
    fn ranking_is_deterministic_and_prefix_first() {
        // "new" should rank "New tab" (prefix) above weaker matches, and equal
        // scores keep registry order (stable).
        let r = reg();
        let hits = filter(&r, "new");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].title, "New tab", "prefix match ranks first");
    }

    #[test]
    fn no_match_query_returns_empty() {
        let r = reg();
        assert!(filter(&r, "zzqxq").is_empty());
    }
}
