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

/// A palette action. `SetTheme` carries the theme index (themes never move
/// while the palette is open); `SelectTab` / `Reattach` carry the STABLE id of
/// their target tab (a main-window tab, or the tab a detached window holds), so
/// a tab closing or moving between open and Enter can never retarget the action
/// — an id that vanished is a clean no-op.
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
    RunSelection,
    PrevPrompt,
    NextPrompt,
    Copy,
    Paste,
    ToggleLaunchAtLogin,
    ResetKeybindings,
    ResetInputModes,
    ToggleFullscreen,
    Hide,
    Quit,
    /// Step to the next / previous theme in the registry order (wrapping), or
    /// a random other one — picked and saved like a `SetTheme`.
    NextTheme,
    PrevTheme,
    RandomTheme,
    /// Toggle `follow_system_theme`.
    ToggleFollowSystemTheme,
    /// Step `minimum_contrast` through off → 3 → 4.5 → 7 → off.
    CycleMinimumContrast,
    SetTheme(usize),
    SelectTab(u64),
    Reattach(u64),
    /// Apply `effects::effect_presets()[i]`.
    EffectsPreset(usize),
    // ── Chrome (visuals v2) ──
    /// `tab_style` = the config string.
    SetTabStyle(&'static str),
    /// `tab_close_button` = the config string.
    SetCloseButton(&'static str),
    /// `window_border` = the config string.
    SetWindowBorder(&'static str),
    ToggleProgressBar,
    ToggleSmartTitles,
    ToggleTabBarOpacity,
    /// The per-tab color of the tab the palette was opened over (palette index
    /// 1..=6; `None` removes it).
    SetTabColor(Option<u8>),
    /// Open Settings at a control ("Settings › Effects › Bloom"), by its id.
    SettingsAt(&'static str),
}

/// The chrome entries: tab style / close buttons / window border pickers, the
/// progress / smart-title / translucent-bar toggles and the per-tab colors.
fn chrome_entries() -> Vec<PaletteEntry> {
    let mut v = Vec::new();
    for s in jetty_render::TabStyle::ALL {
        v.push(PaletteEntry {
            title: format!("Tab style: {}", s.display_name()),
            keywords: "tabs look bar appearance chrome",
            cmd: PaletteCmd::SetTabStyle(s.to_config()),
        });
    }
    for m in jetty_render::CloseButton::ALL {
        v.push(PaletteEntry {
            title: format!("Tab close buttons: {}", m.display_name()),
            keywords: "tabs x close button hover",
            cmd: PaletteCmd::SetCloseButton(m.to_config()),
        });
    }
    for b in crate::tabmeta::WindowBorder::ALL {
        v.push(PaletteEntry {
            title: format!("Window border: {}", b.display_name()),
            keywords: "focus ring outline frame edge accent",
            cmd: PaletteCmd::SetWindowBorder(b.to_config()),
        });
    }
    v.push(PaletteEntry {
        title: "Toggle tab progress bars".to_string(),
        keywords: "osc 9;4 progress cargo claude build percent",
        cmd: PaletteCmd::ToggleProgressBar,
    });
    v.push(PaletteEntry {
        title: "Toggle smart tab titles".to_string(),
        keywords: "tab title cwd directory command name auto",
        cmd: PaletteCmd::ToggleSmartTitles,
    });
    v.push(PaletteEntry {
        title: "Toggle translucent tab bar".to_string(),
        keywords: "tab bar opacity transparent see through",
        cmd: PaletteCmd::ToggleTabBarOpacity,
    });
    for (i, name) in jetty_render::TAB_COLORS {
        v.push(PaletteEntry {
            title: format!("Tab color: {name}"),
            keywords: "tab colour color label tint",
            cmd: PaletteCmd::SetTabColor(Some(i)),
        });
    }
    v.push(PaletteEntry {
        title: "Tab color: None".to_string(),
        keywords: "tab colour color remove clear",
        cmd: PaletteCmd::SetTabColor(None),
    });
    v
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
/// detached window (`Reattach: {title}`, only when there are any). `tabs` and
/// `detached` are `(stable tab id, title)` pairs.
pub fn build_registry(
    themes: &[(String, String)],
    tabs: &[(u64, String)],
    detached: &[(u64, String)],
) -> Vec<PaletteEntry> {
    let statics: [(&str, &str, PaletteCmd); 33] = [
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
        (
            "Run selection in new tab",
            "execute run selected command tab shell browser",
            PaletteCmd::RunSelection,
        ),
        ("Toggle launch at login", "autostart startup boot", PaletteCmd::ToggleLaunchAtLogin),
        ("Reset keybindings to defaults", "shortcut hotkey rebind reset keys", PaletteCmd::ResetKeybindings),
        (
            "Reset keyboard & mouse modes",
            "stuck keys kitty protocol mouse reporting focus bracketed paste crashed program fix tab",
            PaletteCmd::ResetInputModes,
        ),
        ("Toggle fullscreen", "full screen maximize f11 whole monitor", PaletteCmd::ToggleFullscreen),
        ("Hide window", "summon dismiss minimize", PaletteCmd::Hide),
        ("Quit JeTTY", "exit close all", PaletteCmd::Quit),
    ];

    let mut v: Vec<PaletteEntry> =
        Vec::with_capacity(statics.len() + themes.len() + tabs.len() + detached.len());
    for (title, keywords, cmd) in statics {
        v.push(PaletteEntry { title: title.to_string(), keywords, cmd });
    }
    v.extend(chrome_entries());
    // Theme navigation and the appearance switches, ahead of the per-theme
    // rows (arrowing over those previews them live; Esc reverts, Enter keeps).
    let theme_ux: [(&str, &str, PaletteCmd); 5] = [
        ("Next theme", "theme cycle colour color scheme forward", PaletteCmd::NextTheme),
        ("Previous theme", "theme cycle colour color scheme back", PaletteCmd::PrevTheme),
        ("Random theme", "theme shuffle surprise colour color scheme", PaletteCmd::RandomTheme),
        (
            "Toggle follow system light/dark theme",
            "appearance dark mode light mode night day system auto theme",
            PaletteCmd::ToggleFollowSystemTheme,
        ),
        (
            "Cycle minimum contrast (off / 3 / 4.5 / 7)",
            "contrast readability accessibility wcag legible text color",
            PaletteCmd::CycleMinimumContrast,
        ),
    ];
    for (title, keywords, cmd) in theme_ux {
        v.push(PaletteEntry { title: title.to_string(), keywords, cmd });
    }
    for (i, (_name, display)) in themes.iter().enumerate() {
        v.push(PaletteEntry {
            title: format!("Theme: {display}"),
            keywords: "theme colour color scheme palette",
            cmd: PaletteCmd::SetTheme(i),
        });
    }
    for (i, p) in crate::effects::effect_presets().iter().enumerate() {
        v.push(PaletteEntry {
            title: format!("Effects preset: {}", p.name),
            keywords: "effects look crt retro style preset scanline glow phosphor",
            cmd: PaletteCmd::EffectsPreset(i),
        });
    }
    // Deep links into Settings, one per control ("Settings › Effects › Bloom"),
    // straight from the control table — a new setting gets one by itself.
    for link in crate::settings_ui::deep_links() {
        v.push(PaletteEntry { title: link.title, keywords: link.keywords, cmd: PaletteCmd::SettingsAt(link.id) });
    }
    for (id, title) in tabs {
        v.push(PaletteEntry {
            title: format!("Switch to tab: {title}"),
            keywords: "tab go window",
            cmd: PaletteCmd::SelectTab(*id),
        });
    }
    for (id, title) in detached {
        v.push(PaletteEntry {
            title: format!("Reattach: {title}"),
            keywords: "attach dock window",
            cmd: PaletteCmd::Reattach(*id),
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
        let tabs = vec![(1, "Tab 1".to_string()), (2, "Tab 2".to_string())];
        build_registry(&themes, &tabs, &[])
    }

    #[test]
    fn registry_has_all_themes_and_one_per_tab() {
        let themes = jetty_core::theme_list();
        let tabs = vec![(7, "Tab 1".to_string()), (9, "Tab 2".to_string())];
        let r = build_registry(&themes, &tabs, &[]);
        let theme_entries = r.iter().filter(|e| e.title.starts_with("Theme: ")).count();
        assert_eq!(theme_entries, jetty_core::theme_count());
        let tab_entries = r.iter().filter(|e| e.title.starts_with("Switch to tab: ")).count();
        assert_eq!(tab_entries, 2);
        // No Reattach entries when there are no detached windows.
        assert!(!r.iter().any(|e| e.title.starts_with("Reattach: ")));
        // Tab entries carry the tab's STABLE id, never its position.
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::SelectTab(9)));
        assert!(!r.iter().any(|e| e.cmd == PaletteCmd::SelectTab(1)));
        let r = build_registry(&themes, &tabs, &[(12, "Tab 3".to_string())]);
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::Reattach(12) && e.title == "Reattach: Tab 3"));
    }

    #[test]
    fn registry_contains_toggle_fullscreen() {
        let r = reg();
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::ToggleFullscreen));
        // The palette is the discoverability path for a chord that is dead on
        // macOS keyboards without standard function keys.
        let hits = filter(&r, "fullscr");
        assert_eq!(hits[0].cmd, PaletteCmd::ToggleFullscreen, "top hit for 'fullscr'");
        assert_eq!(hits[0].title, "Toggle fullscreen");
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
    fn registry_contains_run_selection_and_ranks_it() {
        let r = reg();
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::RunSelection));
        // The browser-gesture entry must be the top hit for its own words.
        let hits = filter(&r, "run sel");
        assert_eq!(hits[0].cmd, PaletteCmd::RunSelection, "top hit for 'run sel'");
        assert_eq!(hits[0].title, "Run selection in new tab");
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
    fn registry_contains_reset_input_modes_and_ranks_it() {
        // The way out of a crashed program's keyboard/mouse modes is found by the
        // words a user would type in that situation.
        let r = reg();
        for q in ["reset keyboard", "mouse modes", "stuck keys"] {
            let hits = filter(&r, q);
            assert_eq!(hits[0].cmd, PaletteCmd::ResetInputModes, "top hit for {q:?}");
        }
    }

    /// One "Effects preset: …" entry per preset, in preset order, each carrying
    /// its index; typing a preset's name finds it first.
    #[test]
    fn registry_contains_every_effects_preset() {
        let r = reg();
        let presets = crate::effects::effect_presets();
        let entries: Vec<&PaletteEntry> =
            r.iter().filter(|e| e.title.starts_with("Effects preset: ")).collect();
        assert_eq!(entries.len(), presets.len());
        for (i, (e, p)) in entries.iter().zip(presets).enumerate() {
            assert_eq!(e.title, format!("Effects preset: {}", p.name));
            assert_eq!(e.cmd, PaletteCmd::EffectsPreset(i));
        }
        for q in ["green phosphor", "e-ink", "retro crt"] {
            let hits = filter(&r, q);
            assert!(
                matches!(hits[0].cmd, PaletteCmd::EffectsPreset(_)),
                "top hit for {q:?} is {:?}",
                hits[0].title
            );
        }
    }

    #[test]
    fn registry_has_the_chrome_commands() {
        let r = reg();
        for s in jetty_render::TabStyle::ALL {
            assert!(r.iter().any(|e| e.cmd == PaletteCmd::SetTabStyle(s.to_config())), "{s:?}");
        }
        for (i, _) in jetty_render::TAB_COLORS {
            assert!(r.iter().any(|e| e.cmd == PaletteCmd::SetTabColor(Some(i))));
        }
        assert!(r.iter().any(|e| e.cmd == PaletteCmd::SetTabColor(None)));
        assert_eq!(filter(&r, "tab color red")[0].cmd, PaletteCmd::SetTabColor(Some(1)));
        assert_eq!(filter(&r, "powerline")[0].cmd, PaletteCmd::SetTabStyle("powerline"));
        assert_eq!(filter(&r, "window border focus")[0].cmd, PaletteCmd::SetWindowBorder("focus"));
        assert_eq!(filter(&r, "progress bars")[0].cmd, PaletteCmd::ToggleProgressBar);
        // Every config string a command carries parses back to itself.
        for e in &r {
            match &e.cmd {
                PaletteCmd::SetTabStyle(s) => assert_eq!(jetty_render::TabStyle::from_config(s).to_config(), *s),
                PaletteCmd::SetCloseButton(s) => {
                    assert_eq!(jetty_render::CloseButton::from_config(s).to_config(), *s)
                }
                _ => {}
            }
        }
        // Titles stay unique (the palette lists them side by side).
        let mut titles: Vec<&str> = r.iter().map(|e| e.title.as_str()).collect();
        titles.sort_unstable();
        let n = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), n, "duplicate palette titles");
    }

    #[test]
    fn theme_navigation_and_appearance_commands_are_found_by_their_words() {
        let r = reg();
        for (q, cmd) in [
            ("next theme", PaletteCmd::NextTheme),
            ("previous theme", PaletteCmd::PrevTheme),
            ("random theme", PaletteCmd::RandomTheme),
            ("follow system", PaletteCmd::ToggleFollowSystemTheme),
            ("dark mode", PaletteCmd::ToggleFollowSystemTheme),
            ("minimum contrast", PaletteCmd::CycleMinimumContrast),
            ("readability", PaletteCmd::CycleMinimumContrast),
        ] {
            let hits = filter(&r, q);
            assert_eq!(hits.first().map(|h| &h.cmd), Some(&cmd), "top hit for {q:?}");
        }
        // They sit before the per-theme rows (an arrow-down from them enters
        // the previewed list), each listed once.
        let pos = |c: &PaletteCmd| r.iter().position(|e| &e.cmd == c).unwrap();
        let first_theme_row = r.iter().position(|e| matches!(e.cmd, PaletteCmd::SetTheme(_))).unwrap();
        assert!(pos(&PaletteCmd::NextTheme) < first_theme_row);
        assert_eq!(r.iter().filter(|e| e.cmd == PaletteCmd::RandomTheme).count(), 1);
    }

    #[test]
    fn settings_deep_links_rank_first_for_their_control_name() {
        // Typing a control's name finds its "Settings › Tab › Control" link
        // first — even where an action's keywords mention the same word
        // ("Toggle caret glow" carries "bloom").
        let r = reg();
        for (q, id) in [("bloom", "effects.crt_bloom"), ("vignette", "effects.crt_vignette"), ("scrollback", "scrollback_lines")] {
            let hits = filter(&r, q);
            assert_eq!(hits[0].cmd, PaletteCmd::SettingsAt(id), "top hit for {q:?}");
        }
        assert!(r.iter().any(|e| e.title == "Settings › Effects › Bloom"));
    }

    #[test]
    fn no_match_query_returns_empty() {
        let r = reg();
        assert!(filter(&r, "zzqxq").is_empty());
    }
}
