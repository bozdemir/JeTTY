//! README's keybindings table must say what the built-in default keymap does.
//!
//! The table sits between `<!-- keybindings:start` and `<!-- keybindings:end -->`
//! in README.md; each row lists its shortcuts in backticks and the action's
//! `[keys]` name(s) in the last column. Every bindable action must have a row with
//! exactly its default chords, and every name in the table must be a real action —
//! so a changed default, a new action or a renamed one fails here until the README
//! is updated. And every `[keys]` example in README.md and docs/configuration.md
//! must compile without a warning.

use std::collections::BTreeSet;

use jetty_app::config::KeyBindings;
use jetty_app::keymap::{BindableAction, KeyMap};

/// Rows the README already documents with a NEWER default than this build's keymap
/// carries, as `(name, chords this build still has)`. While the keymap still has
/// exactly those old chords the row is skipped; once it gets the new ones the test
/// fails until the entry is removed — so the list can't outlive its reason.
const PENDING: &[(&str, &[&str])] = &[];

/// `BindableAction` → its `[keys]` name (`SelectTab1` → `select_tab_1`), derived
/// from the `Debug` name so this test needs no private API.
fn keys_name(a: BindableAction) -> String {
    let mut out = String::new();
    for (i, ch) in format!("{a:?}").chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_digit() {
            out.push('_');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// The backticked items of a cell, with `a … b` ranges over a trailing digit
/// expanded (`Ctrl+1` … `Ctrl+9`, `select_tab_1` … `select_tab_9`).
fn items(cell: &str) -> Vec<String> {
    let ticked: Vec<String> = cell
        .split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect();
    if ticked.len() == 2 && cell.contains('…') {
        let (a, b) = (&ticked[0], &ticked[1]);
        let (pa, da) = a.split_at(a.len() - 1);
        let (pb, db) = b.split_at(b.len() - 1);
        if pa == pb {
            if let (Ok(lo), Ok(hi)) = (da.parse::<u8>(), db.parse::<u8>()) {
                return (lo..=hi).map(|d| format!("{pa}{d}")).collect();
            }
        }
    }
    ticked
}

/// name → chords documented in the README table.
fn readme_table() -> Vec<(String, BTreeSet<String>)> {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("README.md");
    let start = readme.find("<!-- keybindings:start").expect("keybindings:start marker");
    let end = readme.find("<!-- keybindings:end -->").expect("keybindings:end marker");
    let mut out = Vec::new();
    for line in readme[start..end].lines().skip(1) {
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
        if cells.len() != 3 || cells[0].contains("---") || cells[0].trim() == "Shortcut" {
            continue;
        }
        let chords = items(cells[0]);
        let names = items(cells[2]);
        assert!(!names.is_empty(), "README row without a [keys] name: {line}");
        if names.len() == 1 {
            out.push((names[0].clone(), chords.into_iter().collect()));
        } else {
            assert_eq!(
                chords.len(),
                names.len(),
                "README row must pair its shortcuts and names one to one: {line}"
            );
            for (n, c) in names.into_iter().zip(chords) {
                out.push((n, BTreeSet::from([c])));
            }
        }
    }
    out
}

#[test]
fn readme_keybindings_table_matches_the_default_keymap() {
    let km = KeyMap::defaults();
    let table = readme_table();
    let mut problems = Vec::new();
    let pending_old = |name: &str| PENDING.iter().find(|(n, _)| *n == name).map(|(_, old)| *old);

    for a in BindableAction::ALL {
        let name = keys_name(a);
        let actual: BTreeSet<String> = km.pretty_chords(a).into_iter().collect();
        let documented = table.iter().find(|(n, _)| *n == name).map(|(_, c)| c);
        match (documented, pending_old(&name)) {
            (None, _) => problems.push(format!("`{name}` ({actual:?}) has no README row")),
            (Some(doc), Some(old)) => {
                let old: BTreeSet<String> = old.iter().map(|s| s.to_string()).collect();
                if *doc == actual {
                    problems.push(format!("`{name}` now matches the README — remove it from PENDING"));
                } else if actual != old {
                    problems.push(format!("`{name}`: README says {doc:?}, keymap has {actual:?}"));
                }
            }
            (Some(doc), None) => {
                if *doc != actual {
                    problems.push(format!("`{name}`: README says {doc:?}, keymap has {actual:?}"));
                }
            }
        }
    }
    let known: BTreeSet<String> = BindableAction::ALL.iter().map(|a| keys_name(*a)).collect();
    for (name, _) in &table {
        let pending_absent = pending_old(name).is_some_and(|old| old.is_empty());
        if !known.contains(name) && !pending_absent {
            problems.push(format!("README documents `{name}`, which is not a bindable action"));
        }
    }
    assert!(problems.is_empty(), "README keybindings table is out of date:\n{}", problems.join("\n"));
}

/// Every `[keys]` example in README.md and docs/configuration.md, as `(where,
/// TOML)`: the lines under a fenced block's `[keys]` header, and each inline
/// `` `name = "…"` `` / `` `[keys] name = […]` `` naming a bindable action (a
/// chord string or a list — `run_selection = false` is the top-level switch).
fn documented_keys_examples() -> Vec<(String, String)> {
    let known: BTreeSet<String> = BindableAction::ALL.iter().map(|a| keys_name(*a)).collect();
    let mut out = Vec::new();
    for file in ["README.md", "docs/configuration.md"] {
        let text = std::fs::read_to_string(format!("{}/../../{file}", env!("CARGO_MANIFEST_DIR"))).expect(file);
        let (mut fenced, mut block) = (false, None::<(String, String)>);
        for (n, line) in text.lines().enumerate() {
            let at = format!("{file}:{}", n + 1);
            let t = line.trim();
            if t.starts_with("```") || (fenced && t.starts_with('[')) {
                out.extend(block.take());
                fenced ^= t.starts_with("```");
            }
            if fenced {
                if t == "[keys]" {
                    block = Some((at, String::new()));
                } else if let Some((_, body)) = &mut block {
                    body.push_str(line);
                    body.push('\n');
                }
                continue;
            }
            for span in line.split('`').skip(1).step_by(2) {
                let span = span.strip_prefix("[keys] ").unwrap_or(span);
                let Some((name, value)) = span.split_once(" = ") else { continue };
                if known.contains(name) && value.starts_with(['"', '[']) {
                    out.push((at.clone(), span.to_string()));
                }
            }
        }
    }
    out
}

/// A `[keys]` example a user copies must work as written: README's
/// `new_tab = "Ctrl+T"` was rejected (Ctrl+letter is a control byte) — and
/// took New tab's default chord with it.
#[test]
fn documented_keys_examples_compile_without_warnings() {
    let known: BTreeSet<String> = BindableAction::ALL.iter().map(|a| keys_name(*a)).collect();
    let examples = documented_keys_examples();
    assert!(examples.len() >= 5, "the examples were not found: {examples:?}");
    for (at, example) in &examples {
        let table: toml::Table =
            toml::from_str(example).unwrap_or_else(|e| panic!("{at}: `{example}` is not TOML: {e}"));
        for name in table.keys() {
            assert!(known.contains(name), "{at}: `{name}` is not a bindable action");
        }
        let bindings: KeyBindings = toml::from_str(example).unwrap_or_else(|e| panic!("{at}: `{example}`: {e}"));
        let km = KeyMap::compile(&bindings);
        assert!(km.warnings().is_empty(), "{at}: `{}` → {:?}", example.trim(), km.warnings());
    }
}

#[test]
fn keys_name_matches_the_config_spelling() {
    assert_eq!(keys_name(BindableAction::SelectTab1), "select_tab_1");
    assert_eq!(keys_name(BindableAction::ToggleFullscreen), "toggle_fullscreen");
    assert_eq!(keys_name(BindableAction::OpenPalette), "open_palette");
}
