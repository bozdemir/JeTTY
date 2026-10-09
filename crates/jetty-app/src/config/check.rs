//! Config checks that turn silent fallbacks into warnings a user can act on:
//! unknown keys and invalid values with the closest valid spelling ("did you
//! mean …?"), enum-like strings checked against the parsers the app reads them
//! with (any letter case is accepted), and numbers outside their range.
//!
//! Each check takes its knowledge from the code it guards, so none of it can
//! drift from what JeTTY accepts: the key lists come from the config structs
//! themselves (serde hands a derived `Deserialize` its field names), the enum
//! values from the app's own parsers, and the ranges from the load-time clamps
//! (`Config::sanitized`).

use super::{get_path, set_path, short_value, to_table, BackdropConfig, Config, CursorConfig, EffectsConfig, KeyBindings};

// ── "Did you mean …?" ────────────────────────────────────────────────────────

/// Lower case with every separator dropped: `Font-Size`, `font size` and
/// `fontsize` all read `fontsize`.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '_' | '-' | '.' | ' ')).flat_map(char::to_lowercase).collect()
}

/// The words of a key or value (`theme_light` → `theme`, `light`).
fn words(s: &str) -> Vec<String> {
    s.split(['_', '-', '.', ' ']).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Optimal-string-alignment distance: Levenshtein plus adjacent transpositions
/// (`clipbaord` is one edit from `clipboard`).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// The candidate `word` most likely means, or `None` when nothing is close:
/// another spelling (`fontsize`, `Font-Size`), the first word(s) of exactly
/// one candidate (`scrollback` → `scrollback_lines`), a typo within a few
/// edits (`opacty`, `dropdwn`), the same words in another order
/// (`theme_light`), a key put in the wrong table (`crt_enabled` →
/// `effects.crt_enabled`), a key missing a word (`show_hud` →
/// `show_perf_hud`) or a unique abbreviation (`drop` → `dropdown`). Ties go to
/// the earlier candidate.
pub(crate) fn closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let w = squash(word);
    if w.is_empty() {
        return None;
    }
    if let Some(c) = candidates.iter().find(|c| squash(c) == w) {
        return Some(c);
    }
    // The only candidate with this property, if exactly one has it.
    let unique = |pred: &dyn Fn(&str) -> bool| -> Option<&'a str> {
        let mut hits = candidates.iter().filter(|c| pred(c));
        let first = hits.next()?;
        hits.next().is_none().then_some(*first)
    };
    let ww = words(word);
    if let Some(c) = unique(&|c| words(c).starts_with(&ww)) {
        return Some(c);
    }
    let budget = match w.chars().count() {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };
    let typo = candidates
        .iter()
        .map(|c| (edit_distance(&w, &squash(c)), *c))
        .filter(|&(d, _)| d <= budget)
        .min_by_key(|&(d, _)| d);
    if let Some((_, c)) = typo {
        return Some(c);
    }
    let leaf = |s: &str| squash(s.rsplit('.').next().unwrap_or(s));
    // Two or more words, all in one candidate (`theme_light`, `show_hud`,
    // `crt_enabled` for `effects.crt_enabled`)…
    (ww.len() >= 2)
        .then(|| unique(&|c| ww.iter().all(|x| words(c).contains(x))))
        .flatten()
        // …the same key name in another table (`cursor.opacity`)…
        .or_else(|| unique(&|c| c.contains('.') != word.contains('.') && leaf(c) == leaf(word)))
        // …or a unique abbreviation (`drop`, `scrollback`).
        .or_else(|| unique(&|c| w.len() >= 3 && squash(c).starts_with(&w)))
}

// ── The schema: every key config.toml knows ──────────────────────────────────

/// Captures the field names serde hands a derived `Deserialize` impl's
/// `deserialize_struct` — every key the struct reads — then bails out.
struct FieldNames(&'static [&'static str]);

impl<'de> serde::Deserializer<'de> for &mut FieldNames {
    type Error = serde::de::value::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom("not a struct"))
    }

    fn deserialize_struct<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Self::Error> {
        self.0 = fields;
        Err(serde::de::Error::custom("field names taken"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map enum identifier ignored_any
    }
}

/// The keys a config struct reads, straight from its `Deserialize` impl.
pub(crate) fn fields_of<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    let mut names = FieldNames(&[]);
    let _ = T::deserialize(&mut names);
    names.0
}

/// The tables `config.toml` has, each with the keys it reads.
pub(crate) fn tables() -> [(&'static str, &'static [&'static str]); 4] {
    [
        ("effects", fields_of::<EffectsConfig>()),
        ("backdrop", fields_of::<BackdropConfig>()),
        ("cursor", fields_of::<CursorConfig>()),
        ("keys", fields_of::<KeyBindings>()),
    ]
}

/// Every key `config.toml` knows as a dotted path (`opacity`,
/// `effects.crt_bloom`, `keys.copy`), top-level keys first.
pub(crate) fn known_keys() -> Vec<String> {
    let tables = tables();
    let mut out: Vec<String> = fields_of::<Config>()
        .iter()
        .filter(|k| !tables.iter().any(|(t, _)| t == *k))
        .map(|k| k.to_string())
        .collect();
    for (t, keys) in tables {
        out.extend(keys.iter().map(|k| format!("{t}.{k}")));
    }
    out
}

/// " — did you mean `x`?" for an unknown key at `path`, saying where the
/// suggestion lives when that is another table; "" when nothing is close.
fn key_hint(path: &str, known: &[String]) -> String {
    let refs: Vec<&str> = known.iter().map(String::as_str).collect();
    let Some(s) = closest(path, &refs) else { return String::new() };
    let table = |p: &str| p.split_once('.').map(|(t, _)| t.to_string());
    match (table(path) == table(s), table(s)) {
        (true, _) => format!(" — did you mean `{s}`?"),
        (false, None) => format!(" — did you mean the top-level `{s}`?"),
        (false, Some(t)) => format!(" — did you mean `{}` in `[{t}]`?", &s[t.len() + 1..]),
    }
}

/// Warn about every key `user` sets that JeTTY does not read — a typo
/// (`fontsize`), a key in the wrong table, a stale key — naming the closest
/// valid key. A whole unknown table is one warning. Keys reported as invalid
/// already (`invalid`) are skipped.
pub(super) fn unknown_keys(user: &toml::Table, invalid: &[Vec<String>], warnings: &mut Vec<String>) {
    let top = fields_of::<Config>();
    let tables = tables();
    let mut known: Option<Vec<String>> = None;
    for (k, v) in user {
        if !top.contains(&k.as_str()) {
            if let toml::Value::Table(_) = v {
                let names: Vec<&str> = tables.iter().map(|(t, _)| *t).collect();
                let hint = closest(k, &names).map(|t| format!(" — did you mean `[{t}]`?")).unwrap_or_default();
                warnings.push(format!("unknown table `[{k}]` is ignored{hint}"));
            } else {
                let hint = key_hint(k, known.get_or_insert_with(known_keys));
                warnings.push(format!("unknown key `{k}` is ignored{hint}"));
            }
            continue;
        }
        let (Some((_, keys)), toml::Value::Table(sub)) = (tables.iter().find(|(t, _)| t == k), v) else {
            continue;
        };
        for k2 in sub.keys() {
            let path = [k.clone(), k2.clone()];
            if keys.contains(&k2.as_str()) || invalid.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            let dotted = format!("{k}.{k2}");
            let hint = key_hint(&dotted, known.get_or_insert_with(known_keys));
            warnings.push(format!("unknown key `{dotted}` is ignored{hint}"));
        }
    }
}

// ── Enum-like values ─────────────────────────────────────────────────────────

/// Words that mean "off" for a key whose default is its off state.
const OFF_WORDS: &[&str] = &["off", "none", "false", "no", "disabled"];

/// What the app's lenient `parse` makes of `raw`: `Some(canonical spelling)`
/// when it recognizes it — in any letter case, with blanks around it or `-` /
/// a space for `_` — or `None` when it would silently read it as the default
/// (`unknown` is what it makes of a word it does not know). `default_words`
/// are further spellings of that default (`"none"` for an off switch).
fn recognize(raw: &str, parse: fn(&str) -> &'static str, default_words: &[&str]) -> Option<&'static str> {
    let unknown = parse("\u{1}");
    let loose = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    for cand in [raw, raw.trim(), loose.as_str()] {
        let v = parse(cand);
        if v != unknown {
            return Some(v);
        }
    }
    (loose == unknown || default_words.contains(&loose.as_str())).then_some(unknown)
}

/// The values of a cycling enum, from `first` round to it again.
fn cycle<T: Copy + PartialEq>(first: T, next: fn(T) -> T, name: fn(T) -> &'static str) -> Vec<&'static str> {
    let mut out = vec![name(first)];
    let mut x = next(first);
    while x != first && out.len() < 64 {
        out.push(name(x));
        x = next(x);
    }
    out
}

/// One enum-like key: where it is, its values (in Settings / cycle order) and
/// how the app reads it.
pub(crate) struct Choice {
    pub path: &'static [&'static str],
    pub values: fn() -> Vec<&'static str>,
    /// The canonical value `raw` reads as — `None` when the app does not
    /// recognize it.
    pub canon: fn(&str) -> Option<&'static str>,
}

fn backdrop_shape(s: &str) -> &'static str {
    match jetty_render::BackdropShape::parse(s) {
        jetty_render::BackdropShape::Linear => "linear",
        jetty_render::BackdropShape::Radial => "radial",
    }
}

fn backdrop_fit(s: &str) -> &'static str {
    use jetty_render::BackdropFit as F;
    match F::parse(s) {
        F::Cover => "cover",
        F::Contain => "contain",
        F::Stretch => "stretch",
        F::Center => "center",
        F::Tile => "tile",
    }
}

fn backdrop_pattern(s: &str) -> &'static str {
    use jetty_render::BackdropPattern as P;
    match P::parse(s) {
        P::Stars => "stars",
        P::Aurora => "aurora",
        P::Grid => "grid",
        P::Synthwave => "synthwave",
    }
}

/// Every enum-like key, each read by the very parser the app uses.
pub(crate) fn choices() -> Vec<Choice> {
    use crate::app::{SummonEffect, WindowMode};
    use crate::motion::{CommandPulse, CursorShapePref, GuideMode, ReduceMotion, VisualBell};
    use crate::tabmeta::{TabTitleMode, WindowBorder};
    use jetty_render::{BackdropMode, CloseButton, TabStyle};
    vec![
        Choice {
            path: &["window_mode"],
            values: || WindowMode::ORDER.iter().map(|m| m.to_config()).collect(),
            canon: |s| recognize(s, |x| WindowMode::from_config(x).to_config(), &[]),
        },
        Choice {
            path: &["summon_effect"],
            values: || SummonEffect::ORDER.iter().map(|e| e.to_config()).collect(),
            canon: |s| recognize(s, |x| SummonEffect::from_config(x).to_config(), &[]),
        },
        Choice {
            path: &["tab_bar_position"],
            values: || vec!["top", "bottom"],
            // The app reads exactly `"bottom"` as bottom, anything else as top.
            canon: |s| recognize(s, |x| if x == "bottom" { "bottom" } else { "top" }, &[]),
        },
        Choice {
            path: &["scrollbar"],
            values: || {
                use super::ScrollbarMode as M;
                [M::Always, M::Auto, M::Never].iter().map(|m| m.as_str()).collect()
            },
            canon: |s| recognize(s, |x| super::ScrollbarMode::parse(x).as_str(), &[]),
        },
        Choice {
            path: &["tab_style"],
            values: || TabStyle::ALL.iter().map(|t| t.to_config()).collect(),
            canon: |s| recognize(s, |x| TabStyle::from_config(x).to_config(), &[]),
        },
        Choice {
            path: &["tab_close_button"],
            values: || CloseButton::ALL.iter().map(|c| c.to_config()).collect(),
            canon: |s| recognize(s, |x| CloseButton::from_config(x).to_config(), &[]),
        },
        Choice {
            path: &["window_border"],
            values: || WindowBorder::ALL.iter().map(|b| b.to_config()).collect(),
            canon: |s| recognize(s, |x| WindowBorder::from_config(x).to_config(), OFF_WORDS),
        },
        Choice {
            path: &["tab_title"],
            values: || [TabTitleMode::Osc, TabTitleMode::Auto].iter().map(|m| m.to_config()).collect(),
            canon: |s| recognize(s, |x| TabTitleMode::from_config(x).to_config(), &[]),
        },
        Choice {
            path: &["reduce_motion"],
            values: || cycle(ReduceMotion::default(), ReduceMotion::next, ReduceMotion::as_str),
            canon: |s| recognize(s, |x| ReduceMotion::parse(x).as_str(), OFF_WORDS),
        },
        Choice {
            path: &["visual_bell"],
            values: || cycle(VisualBell::default(), VisualBell::next, VisualBell::as_str),
            canon: |s| recognize(s, |x| VisualBell::parse(x).as_str(), OFF_WORDS),
        },
        Choice {
            path: &["command_pulse"],
            values: || cycle(CommandPulse::default(), CommandPulse::next, CommandPulse::as_str),
            canon: |s| recognize(s, |x| CommandPulse::parse(x).as_str(), OFF_WORDS),
        },
        Choice {
            path: &["cursor", "shape"],
            values: || cycle(CursorShapePref::default(), CursorShapePref::next, CursorShapePref::as_str),
            canon: |s| recognize(s, |x| CursorShapePref::parse(x).as_str(), &[]),
        },
        Choice {
            path: &["cursor", "unfocused"],
            values: || vec!["hollow", "unchanged", "none"],
            canon: |s| recognize(s, |x| crate::motion::unfocused_str(crate::motion::parse_unfocused(x)), &[]),
        },
        Choice {
            path: &["cursor", "color"],
            values: || vec!["theme", "cell", "auto"],
            canon: |s| {
                recognize(s, |x| crate::motion::cursor_color_str(crate::motion::parse_cursor_color(x)), &[])
            },
        },
        Choice {
            path: &["cursor", "guide"],
            values: || cycle(GuideMode::default(), GuideMode::next, GuideMode::as_str),
            canon: |s| recognize(s, |x| GuideMode::parse(x).as_str(), OFF_WORDS),
        },
        Choice {
            path: &["backdrop", "mode"],
            values: || vec!["none", "theme", "gradient", "image", "pattern"],
            canon: |s| recognize(s, |x| BackdropMode::parse(x).as_str(), OFF_WORDS),
        },
        Choice {
            path: &["backdrop", "shape"],
            values: || vec!["linear", "radial"],
            canon: |s| recognize(s, backdrop_shape, &[]),
        },
        Choice {
            path: &["backdrop", "fit"],
            values: || vec!["cover", "contain", "stretch", "center", "tile"],
            canon: |s| recognize(s, backdrop_fit, &[]),
        },
        Choice {
            path: &["backdrop", "pattern"],
            values: || vec!["stars", "aurora", "grid", "synthwave"],
            canon: |s| recognize(s, backdrop_pattern, &[]),
        },
        Choice {
            path: &["copy_on_select"],
            values: || vec!["primary", "clipboard", "both", "off"],
            canon: |s| recognize(s, |x| crate::clipboard::CopyOnSelect::parse(x).as_str(), &[]),
        },
        Choice {
            path: &["macos_option_as_alt"],
            values: || vec!["none", "left", "right", "both"],
            canon: |s| recognize(s, |x| crate::input::OptionAsAlt::parse(x).as_str(), OFF_WORDS),
        },
    ]
}

/// Check every enum-like value `user` sets against the parser the app reads
/// it with. A value it recognizes in another spelling (`"Dropdown"`, an alias
/// like `"bar"`) is stored in its canonical spelling, so every reader sees the
/// value meant. One it does not recognize — which it would silently read as
/// the default — is reported with the closest valid value and replaced by
/// `base`'s (the default at startup, the live value on a reload), like any
/// other invalid value; its path joins `invalid`.
pub(super) fn check_choices(
    user: &toml::Table,
    cfg: &mut Config,
    base: &Config,
    fallback: &str,
    warnings: &mut Vec<String>,
    invalid: &mut Vec<Vec<String>>,
) {
    let mut fixed: Option<toml::Table> = None;
    let mut base_t: Option<toml::Table> = None;
    for c in choices() {
        let path: Vec<String> = c.path.iter().map(|s| s.to_string()).collect();
        if invalid.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let Some(toml::Value::String(raw)) = get_path(user, &path) else { continue };
        let t = fixed.get_or_insert_with(|| to_table(cfg));
        match (c.canon)(raw) {
            Some(canon) => {
                if get_path(t, &path).and_then(toml::Value::as_str) != Some(canon) {
                    set_path(t, &path, toml::Value::String(canon.to_string()));
                }
            }
            None => {
                let values = (c.values)();
                let hint = closest(raw, &values).map(|v| format!(" — did you mean `{v}`?")).unwrap_or_default();
                let list = values.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ");
                warnings.push(format!(
                    "`{} = {}` is invalid{hint} (expected one of {list}) — {fallback}",
                    path.join("."),
                    short_value(&toml::Value::String(raw.clone())),
                ));
                let b = base_t.get_or_insert_with(|| to_table(base));
                if let Some(v) = get_path(b, &path) {
                    set_path(t, &path, v.clone());
                }
                invalid.push(path);
            }
        }
    }
    if let Some(t) = fixed {
        if let Ok(c) = toml::Value::Table(t).try_into::<Config>() {
            *cfg = c;
        }
    }
}

/// " — did you mean `x`?" for serde's "unknown variant `y`, expected one of
/// `a`, `b`" (a strict enum such as `effects.crt_phosphor`), else "".
pub(super) fn variant_hint(msg: &str) -> String {
    let Some(rest) = msg.strip_prefix("unknown variant `") else { return String::new() };
    let Some((got, rest)) = rest.split_once('`') else { return String::new() };
    let Some((_, list)) = rest.split_once("expected ") else { return String::new() };
    let values: Vec<&str> = list.split('`').skip(1).step_by(2).collect();
    closest(got, &values).map(|v| format!(" — did you mean `{v}`?")).unwrap_or_default()
}

/// Warn about `backdrop.colors` entries that are not colors (the gradient
/// skips them) and about stops past the fourth (never drawn).
pub(super) fn color_warnings(user: &toml::Table, invalid: &[Vec<String>], warnings: &mut Vec<String>) {
    let path = ["backdrop".to_string(), "colors".to_string()];
    if invalid.iter().any(|p| path.starts_with(p)) {
        return;
    }
    let Some(toml::Value::Array(colors)) = get_path(user, &path) else { return };
    let mut good = 0;
    for c in colors {
        if c.as_str().and_then(jetty_render::parse_hex_color).is_some() {
            good += 1;
        } else {
            warnings.push(format!(
                "`backdrop.colors` entry {} is not a color (\"#rrggbb\" or \"#rgb\") — skipped",
                short_value(c)
            ));
        }
    }
    if good > 4 {
        warnings.push(format!("`backdrop.colors` has {good} colors — only the first 4 are used"));
    }
}

// ── Ranges ───────────────────────────────────────────────────────────────────

/// Keys whose load-time sanitizing is a normalization, not a range: an angle
/// of -90 is 270, nothing to warn about.
const NORMALIZED: &[&str] = &["backdrop.angle"];

/// A value for a message: floats in their shortest form (`0.1`, `48`).
fn show(v: &toml::Value) -> String {
    match v {
        toml::Value::Float(f) => super::clean_float(*f).to_string(),
        toml::Value::Array(a) => format!("[{}]", a.iter().map(show).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

fn not_finite(v: &toml::Value) -> bool {
    match v {
        toml::Value::Float(f) => !f.is_finite(),
        toml::Value::Array(a) => a.iter().any(not_finite),
        _ => false,
    }
}

/// The range the load-time sanitizing keeps the number at `path` in, found by
/// sanitizing an extreme value either way (so it is always the real clamp).
fn bounds(path: &[String]) -> Option<(String, String)> {
    let defaults = to_table(&Config::default());
    let probe = |v: toml::Value| -> Option<toml::Value> {
        let mut t = defaults.clone();
        set_path(&mut t, path, v);
        let cfg: Config = toml::Value::Table(t).try_into().ok()?;
        let got = get_path(&to_table(&cfg.sanitized()), path)?.clone();
        match got {
            toml::Value::Array(a) => a.into_iter().next(),
            v => Some(v),
        }
    };
    let (lo, hi) = match get_path(&defaults, path)? {
        toml::Value::Float(_) => (toml::Value::Float(-1e30), toml::Value::Float(1e30)),
        toml::Value::Integer(_) => (toml::Value::Integer(0), toml::Value::Integer(i64::from(u32::MAX))),
        toml::Value::Array(a) => (
            toml::Value::Array(vec![toml::Value::Float(-1e30); a.len()]),
            toml::Value::Array(vec![toml::Value::Float(1e30); a.len()]),
        ),
        _ => return None,
    };
    Some((show(&probe(lo)?), show(&probe(hi)?)))
}

/// Warn about every number `user` sets that loading had to change: out of its
/// range (clamped) or not a finite number (`nan`, `inf`: the default). `raw`
/// is the config as parsed, `clean` the same sanitized.
pub(super) fn range_warnings(
    user: &toml::Table,
    raw: &Config,
    clean: &Config,
    invalid: &[Vec<String>],
    warnings: &mut Vec<String>,
) {
    for change in super::diff_configs(raw, clean) {
        let key = change.path.join(".");
        if NORMALIZED.contains(&key.as_str()) || invalid.iter().any(|p| change.path.starts_with(p)) {
            continue;
        }
        let (Some(written), Some(now)) = (get_path(user, &change.path), change.value.as_ref()) else {
            continue;
        };
        let (wrote, now) = (short_value(written), show(now));
        warnings.push(if not_finite(written) {
            format!("`{key} = {wrote}` is not a finite number — using {now}")
        } else {
            let range = bounds(&change.path).map(|(lo, hi)| format!(" ({lo}–{hi})")).unwrap_or_default();
            format!("`{key} = {wrote}` is out of range{range} — using {now}")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_finds_typos_spellings_and_misplaced_keys() {
        let keys = known_keys();
        let k: Vec<&str> = keys.iter().map(String::as_str).collect();
        for (typo, want) in [
            ("fontsize", "font_size"),
            ("Font-Size", "font_size"),
            ("opacty", "opacity"),
            ("opactiy", "opacity"),
            ("colour_emoji", "color_emoji"),
            ("theme_light", "light_theme"),
            ("crt_enabled", "effects.crt_enabled"),
            ("effects.crt_enabld", "effects.crt_enabled"),
            ("efects.crt_enabled", "effects.crt_enabled"),
            ("keys.new_tabb", "keys.new_tab"),
            ("keys.select_tab", "keys.select_tab_1"),
            ("cursor.opacity", "opacity"),
            ("show_hud", "show_perf_hud"),
            ("scrollback", "scrollback_lines"),
            ("hotreload", "hot_reload"),
        ] {
            assert_eq!(closest(typo, &k), Some(want), "{typo}");
        }
        for nothing in ["font", "zzz", "line_spacing", ""] {
            assert_eq!(closest(nothing, &k), None, "{nothing}");
        }
        let modes = ["center", "dropdown", "fullscreen"];
        assert_eq!(closest("dropdwn", &modes), Some("dropdown"));
        assert_eq!(closest("drop", &modes), Some("dropdown"));
        assert_eq!(closest("full screen", &modes), Some("fullscreen"));
        assert_eq!(closest("sideways", &modes), None);
        assert_eq!(closest("clipbaord", &["primary", "clipboard", "both", "off"]), Some("clipboard"));
    }

    #[test]
    fn the_schema_is_the_structs_and_covers_every_table() {
        let top = fields_of::<Config>();
        assert!(top.contains(&"opacity") && top.contains(&"keys") && top.contains(&"effects"));
        assert_eq!(top.len(), to_table(&Config::default()).len() + 1, "every field but the empty [keys]");
        // Every table field of Config has its key list here — a new `[table]`
        // must be added to `tables()`, or its keys would read as unknown.
        let defaults = to_table(&Config::default());
        for f in top {
            let is_table = *f == "keys" || defaults.get(*f).is_some_and(toml::Value::is_table);
            assert_eq!(tables().iter().any(|(t, _)| t == f), is_table, "{f}");
        }
        for (t, keys) in tables() {
            if t == "keys" {
                assert!(keys.contains(&"copy") && keys.contains(&"select_tab_9"), "{keys:?}");
                continue;
            }
            let d = defaults[t].as_table().unwrap();
            let mut a: Vec<&str> = d.keys().map(String::as_str).collect();
            let mut b = keys.to_vec();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "[{t}]");
        }
    }

    #[test]
    fn every_choice_value_is_what_its_parser_reads() {
        let defaults = to_table(&Config::default());
        for c in choices() {
            let path: Vec<String> = c.path.iter().map(|s| s.to_string()).collect();
            let values = (c.values)();
            assert!(!values.is_empty(), "{path:?}");
            let default = get_path(&defaults, &path).and_then(toml::Value::as_str).unwrap();
            assert!(values.contains(&default), "{path:?}: the default {default} is a value");
            for v in &values {
                assert_eq!((c.canon)(v), Some(*v), "{path:?}: {v} must read as itself");
                assert_eq!((c.canon)(&v.to_uppercase()), Some(*v), "{path:?}: any case");
            }
            let mut uniq = values.clone();
            uniq.sort_unstable();
            uniq.dedup();
            assert_eq!(uniq.len(), values.len(), "{path:?}: distinct values");
            assert_eq!((c.canon)("certainly-not-a-value"), None, "{path:?}");
        }
    }

    #[test]
    fn serde_variant_errors_get_a_suggestion() {
        let msg = "unknown variant `ambr`, expected one of `off`, `amber`, `green`";
        assert_eq!(variant_hint(msg), " — did you mean `amber`?");
        assert_eq!(variant_hint("unknown variant `zzzz`, expected one of `off`, `amber`"), "");
        assert_eq!(variant_hint("invalid type: string, expected f32"), "");
    }
}
