//! User-imported theme loading.
//!
//! Reads `~/.config/jetty/themes/*.toml`, parses each into a `jetty_core::Theme`,
//! and merges them with the 46 built-ins into the runtime registry (a user theme
//! whose `name` matches a built-in REPLACES it in place; new names append). Parsing
//! lives here (jetty-app already carries serde/toml/dirs); jetty-core holds only the
//! parsed data + the registry.
//!
//! Never panics: a malformed file (bad TOML, bad hex, wrong palette length, missing
//! a required field) is reported — with the line and column, or the key of the bad
//! value — and skipped, or, when an earlier version of it loaded in this session,
//! that version is kept (a theme on screen survives a typo mid-edit); the other
//! themes still load. A key close to a real one (`selection_backgrond`) is
//! reported too. `read_dir` order is unspecified, so files are SORTED before
//! loading, making "duplicate user name → last wins" deterministic.
//!
//! ## Schema (`~/.config/jetty/themes/<id>.toml`)
//! ```toml
//! name         = "my_theme"     # optional (defaults to the file stem)
//! display_name = "My Theme"     # optional (defaults to a title-cased name)
//! background   = "#1e1e2e"      # required   (alias: bg)
//! foreground   = "#cdd6f4"      # required   (alias: fg)
//! cursor       = "#f5e0dc"      # required
//! cursor_text  = "#1e1e2e"      # optional: glyph under a block cursor (default: background)
//! selection_foreground = "#…"   # optional (alias: selection_fg): every selected glyph's
//!                               # color (default: own color, unless unreadable on the highlight)
//! selection_background = "#…"   # optional (alias: selection_bg): the selection highlight
//!                               # (default: 1/3 background + 2/3 ANSI blue)
//! accent       = "#89b4fa"      # optional: the UI accent — menu hover, focused/active marks,
//!                               # the welcome logo (default: ANSI blue; a faint one is shaded)
//! # 16 ANSI colors — EITHER a flat 16-array `palette = [...]` (REQUIRED unless the
//! # named tables below are given; if BOTH are present, `palette` WINS):
//! palette = ["#45475a", "#f38ba8", ...]   # exactly 16 hex colors
//! # …OR named tables (standard TOML — one key per line):
//! # [normal]
//! # black="#…"  red="#…"  green="#…"  yellow="#…"
//! # blue="#…"   magenta="#…"  cyan="#…"  white="#…"
//! # [bright]
//! # black="#…"  …  white="#…"
//! ```
//! Hex accepts `#rrggbb`, `#rgb`, or the same without the leading `#`. `opacity` is
//! a GLOBAL setting (config `opacity`), not per-theme — an `opacity` key is
//! accepted-and-ignored for forward-compat. A plain-color `selection = "#…"` (the
//! older spelling) is read as `selection_background`; any other `selection` value
//! (a table, say) is still ignored.

use std::borrow::Cow;

use serde::Deserialize;

/// Raw parsed theme file. Unknown keys (`opacity`, …) are ignored by serde (no
/// `deny_unknown_fields`), so they are accepted-and-ignored.
#[derive(Debug, Deserialize)]
struct ThemeToml {
    name: Option<String>,
    display_name: Option<String>,
    #[serde(alias = "bg")]
    background: Option<String>,
    #[serde(alias = "fg")]
    foreground: Option<String>,
    cursor: Option<String>,
    cursor_text: Option<String>,
    #[serde(alias = "selection_fg")]
    selection_foreground: Option<String>,
    #[serde(alias = "selection_bg")]
    selection_background: Option<String>,
    /// The older `selection` key: any TOML value, so a table there (another
    /// terminal's format) never fails the file; only a color string is used.
    selection: Option<toml::Value>,
    accent: Option<String>,
    palette: Option<Vec<String>>,
    normal: Option<AnsiTable>,
    bright: Option<AnsiTable>,
}

/// One 8-color ANSI table (`[normal]` or `[bright]`).
#[derive(Debug, Deserialize)]
struct AnsiTable {
    black: String,
    red: String,
    green: String,
    yellow: String,
    blue: String,
    magenta: String,
    cyan: String,
    white: String,
}

impl AnsiTable {
    /// The 8 colors; `table` (`normal` / `bright`) names a bad one's key.
    fn to_rows(&self, table: &str) -> Result<[[u8; 3]; 8], String> {
        let c = |name: &str, v: &str| hex(&format!("{table}.{name}"), v);
        Ok([
            c("black", &self.black)?,
            c("red", &self.red)?,
            c("green", &self.green)?,
            c("yellow", &self.yellow)?,
            c("blue", &self.blue)?,
            c("magenta", &self.magenta)?,
            c("cyan", &self.cyan)?,
            c("white", &self.white)?,
        ])
    }
}

/// Every key a theme file may set: what [`ThemeToml`] reads, its aliases, and
/// `opacity` (accepted and ignored — opacity is a global setting).
const KNOWN_KEYS: &[&str] = &[
    "name",
    "display_name",
    "background",
    "bg",
    "foreground",
    "fg",
    "cursor",
    "cursor_text",
    "selection_foreground",
    "selection_fg",
    "selection_background",
    "selection_bg",
    "selection",
    "accent",
    "palette",
    "normal",
    "bright",
    "opacity",
];

/// [`parse_hex`] for the theme key `key`, which a bad value's error names.
fn hex(key: &str, value: &str) -> Result<[u8; 3], String> {
    parse_hex(value).map_err(|e| format!("`{key}`: {e}"))
}

/// Parse a hex color (`#rrggbb`, `#rgb`, `rrggbb`, or `rgb`) to `[r, g, b]`.
fn parse_hex(s: &str) -> Result<[u8; 3], String> {
    let h = s.trim().trim_start_matches('#');
    // Validate BEFORE slicing: the length match below counts BYTES, so a non-ASCII
    // value with a matching byte length (`"#aç"`, `"#1é1e2"`) would byte-slice
    // through a UTF-8 char and panic — crashing the whole terminal on startup or
    // on a theme hot-reload. `from_str_radix` would also accept a leading `+`
    // (`"#+f+f+f"`). Requiring ASCII hex digits rules out both.
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("bad hex color {s:?} (want #rrggbb or #rgb)"));
    }
    let byte = |two: &str| u8::from_str_radix(two, 16).map_err(|_| format!("bad hex color {s:?}"));
    match h.len() {
        6 => Ok([byte(&h[0..2])?, byte(&h[2..4])?, byte(&h[4..6])?]),
        3 => {
            // #rgb → #rrggbb (each nibble doubled).
            let nib = |c: &str| {
                u8::from_str_radix(c, 16)
                    .map(|v| v * 17)
                    .map_err(|_| format!("bad hex color {s:?}"))
            };
            Ok([nib(&h[0..1])?, nib(&h[1..2])?, nib(&h[2..3])?])
        }
        _ => Err(format!("bad hex color {s:?} (want #rrggbb or #rgb)")),
    }
}

/// Title-case a snake_case id: `my_cool_theme` → `My Cool Theme`.
fn title_case(id: &str) -> String {
    id.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convert a parsed `ThemeToml` into a `jetty_core::Theme`. `stem` is the file stem,
/// used for the default `name`. Returns a human-readable error (for skip+log) when a
/// required field is missing, the palette is not exactly 16 colors, or any hex is bad.
fn theme_from_toml(t: ThemeToml, stem: &str) -> Result<jetty_core::Theme, String> {
    let name = t.name.unwrap_or_else(|| stem.to_string());
    if name.is_empty() {
        return Err("empty theme name".to_string());
    }
    let display_name = t.display_name.unwrap_or_else(|| title_case(&name));

    let bg3 = hex("background", t.background.as_deref().ok_or("missing `background`")?)?;
    let fg = hex("foreground", t.foreground.as_deref().ok_or("missing `foreground`")?)?;
    let cursor = hex("cursor", t.cursor.as_deref().ok_or("missing `cursor`")?)?;
    let cursor_text = t.cursor_text.as_deref().map(|v| hex("cursor_text", v)).transpose()?;
    let selection_fg = t.selection_foreground.as_deref().map(|v| hex("selection_foreground", v)).transpose()?;
    let accent = t.accent.as_deref().map(|v| hex("accent", v)).transpose()?;
    // `selection_background` wins; the older plain-color `selection` is lenient
    // (it used to be ignored, so a bad value there must not drop the theme now).
    let selection_bg = match (t.selection_background.as_deref(), &t.selection) {
        (Some(s), _) => Some(hex("selection_background", s)?),
        (None, Some(toml::Value::String(s))) => parse_hex(s).ok(),
        _ => None,
    };

    // Palette: flat 16-array WINS when present; else the named [normal]/[bright]
    // tables; else it is a required-field error.
    let palette: [[u8; 3]; 16] = if let Some(list) = t.palette {
        if list.len() != 16 {
            return Err(format!("`palette` must have exactly 16 colors (got {})", list.len()));
        }
        let mut p = [[0u8; 3]; 16];
        for (i, value) in list.iter().enumerate() {
            p[i] = hex(&format!("palette[{i}]"), value)?;
        }
        p
    } else if let (Some(normal), Some(bright)) = (t.normal.as_ref(), t.bright.as_ref()) {
        let n = normal.to_rows("normal")?;
        let b = bright.to_rows("bright")?;
        let mut p = [[0u8; 3]; 16];
        p[..8].copy_from_slice(&n);
        p[8..].copy_from_slice(&b);
        p
    } else {
        return Err("missing `palette` (or complete `[normal]`+`[bright]` tables)".to_string());
    };

    Ok(jetty_core::Theme {
        name: Cow::Owned(name),
        display_name: Cow::Owned(display_name),
        // Opacity is a GLOBAL config setting, applied at render time — a theme file's
        // bg is always fully opaque here (alpha 255).
        bg: [bg3[0], bg3[1], bg3[2], 255],
        fg,
        cursor,
        cursor_text,
        selection_fg,
        accent,
        selection_bg,
        palette,
    })
}

/// The last version of each user theme file that loaded, by path. A save that
/// does not load — a typo mid-edit in the theme on screen — keeps this one,
/// with a warning, instead of dropping the theme (the whole terminal used to
/// flash to the fallback theme until the next good save). A deleted file is
/// forgotten.
static LAST_GOOD: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, jetty_core::Theme>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Read and convert one theme file. `Err` says why it cannot be used, with the
/// line and column of a TOML error and the key of a bad value. Keys that are
/// close to a real key (a typo: `selection_backgrond`) are reported in
/// `warnings`; other unknown keys (`author`) are accepted silently.
fn load_theme_file(
    path: &std::path::Path,
    stem: &str,
    file: &str,
    warnings: &mut Vec<String>,
) -> Result<jetty_core::Theme, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed: ThemeToml =
        toml::from_str(&content).map_err(|e| crate::config::describe_toml_error(&e, &content))?;
    if let Ok(table) = toml::from_str::<toml::Table>(&content) {
        for key in table.keys().filter(|k| !KNOWN_KEYS.contains(&k.as_str())) {
            if let Some(near) = crate::config::check::closest(key, KNOWN_KEYS) {
                warnings.push(format!(
                    "theme file themes/{file}: unknown key `{key}` is ignored — did you mean `{near}`?"
                ));
            }
        }
    }
    theme_from_toml(parsed, stem)
}

/// Read `<dir>/*.toml` (the user themes dir) into a `Vec<Theme>`, plus a warning
/// per problem. Never panics: a malformed file is skipped (and reported) — or,
/// when an earlier version of it loaded in this session, that version is kept
/// (see [`LAST_GOOD`]); a missing directory yields `[]`. Files are sorted by
/// path so duplicate names resolve deterministically (last wins).
pub fn load_user_themes_from(dir: &std::path::Path) -> (Vec<jetty_core::Theme>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut last_good = LAST_GOOD.lock().unwrap_or_else(|p| p.into_inner());
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            // `.tmp.` = an editor's / our own atomic-save temp file mid-write.
            .filter(|p| {
                p.extension().and_then(|x| x.to_str()) == Some("toml")
                    && !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(".tmp."))
            })
            .collect(),
        Err(_) => {
            // No themes dir → no user themes (and none to keep).
            last_good.retain(|p, _| p.parent() != Some(dir));
            return (Vec::new(), warnings);
        }
    };
    files.sort(); // deterministic load order (read_dir order is unspecified)
    last_good.retain(|p, _| p.parent() != Some(dir) || files.contains(p));

    let mut out: Vec<jetty_core::Theme> = Vec::new();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("theme")
            .to_string();
        let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let theme = match load_theme_file(&path, &stem, &file, &mut warnings) {
            Ok(theme) => {
                last_good.insert(path.clone(), theme.clone());
                theme
            }
            Err(e) => match last_good.get(&path) {
                Some(kept) => {
                    warnings.push(format!(
                        "theme file themes/{file}: {e} — keeping the last version that loaded"
                    ));
                    kept.clone()
                }
                None => {
                    warnings.push(format!("theme file themes/{file} skipped: {e}"));
                    continue;
                }
            },
        };
        // Duplicate user name → last wins (drop the earlier one), reported.
        if let Some(pos) = out.iter().position(|t| t.name == theme.name) {
            warnings.push(format!("two theme files are named {:?} — themes/{file} wins", theme.name));
            out.remove(pos);
        }
        out.push(theme);
    }
    (out, warnings)
}

/// A fingerprint of the user theme files in `dir` (each `*.toml`'s name and
/// content): equal fingerprints mean no theme file changed — so a reload that is
/// only the echo of JeTTY's own config save need not re-show theme warnings.
pub fn fingerprint_of(dir: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &files {
        f.hash(&mut h);
        std::fs::read(f).ok().hash(&mut h);
    }
    h.finish()
}

/// [`fingerprint_of`] the user themes dir.
pub fn fingerprint() -> u64 {
    fingerprint_of(&crate::config::Config::dir().join("themes"))
}

/// The user themes from `~/.config/jetty/themes/` (see [`load_user_themes_from`]).
pub fn load_user_themes() -> (Vec<jetty_core::Theme>, Vec<String>) {
    load_user_themes_from(&crate::config::Config::dir().join("themes"))
}

/// `" — did you mean "x"?"` naming the theme closest to `name` — a theme name
/// that found no theme — or `""` when none is close.
pub fn name_hint(name: &str) -> String {
    let list = jetty_core::theme_list();
    let ids: Vec<&str> = list.iter().map(|(id, _)| id.as_str()).collect();
    crate::config::check::closest(name, &ids).map(|s| format!(" — did you mean {s:?}?")).unwrap_or_default()
}

/// Merge the built-ins (PRESETS order) with `user` themes: a user theme whose `name`
/// matches a built-in REPLACES it in place; a new name appends. Pure + testable.
fn merge_into_builtins(user: Vec<jetty_core::Theme>) -> Vec<jetty_core::Theme> {
    let mut merged = jetty_core::builtins();
    for u in user {
        if let Some(slot) = merged.iter_mut().find(|t| t.name == u.name) {
            *slot = u; // shadow the built-in in place (keeps its ordered position)
        } else {
            merged.push(u); // new theme appends after the built-ins
        }
    }
    merged
}

/// Rebuild the runtime theme registry from the built-ins + the current user themes
/// on disk. Called once at startup and on every hot-reload of `themes/`. Returns a
/// warning per skipped / shadowed theme file, for the app to show (also logged).
pub fn rebuild_registry() -> Vec<String> {
    let (user, warnings) = load_user_themes();
    jetty_core::set_registry(merge_into_builtins(user));
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_only_with_the_theme_files() {
        let dir = std::env::temp_dir().join(format!("jetty-theme-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.toml"), "bad").unwrap();
        let fp = fingerprint_of(&dir);
        assert_eq!(fingerprint_of(&dir), fp, "stable");
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        assert_eq!(fingerprint_of(&dir), fp, "non-theme files do not count");
        std::fs::write(dir.join("a.toml"), "still bad").unwrap();
        assert_ne!(fingerprint_of(&dir), fp, "an edit (even still broken) counts");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn parse(s: &str, stem: &str) -> Result<jetty_core::Theme, String> {
        let t: ThemeToml = toml::from_str(s).map_err(|e| e.to_string())?;
        theme_from_toml(t, stem)
    }

    #[test]
    fn hex_parsing_forms() {
        assert_eq!(parse_hex("#1e1e2e").unwrap(), [30, 30, 46]);
        assert_eq!(parse_hex("1e1e2e").unwrap(), [30, 30, 46]);
        assert_eq!(parse_hex("#fff").unwrap(), [255, 255, 255]);
        assert_eq!(parse_hex("#000").unwrap(), [0, 0, 0]);
        assert_eq!(parse_hex("#abc").unwrap(), [170, 187, 204]);
        assert!(parse_hex("#12").is_err());
        assert!(parse_hex("#gggggg").is_err());
    }

    #[test]
    fn non_ascii_or_signed_hex_is_an_error_not_a_panic() {
        // Byte lengths 3 and 6 with a multi-byte char used to slice through it
        // and panic ("byte index 2 is not a char boundary") — on startup or a
        // theme hot-reload that killed the terminal with every shell.
        for bad in ["#aç", "#ç1", "#1é1e2", "#€", "ğğğ", "#+f+f+f", "+ff", "#-1-1-1"] {
            assert!(parse_hex(bad).is_err(), "{bad:?} must be rejected, not panic");
        }
        // A whole theme file with such a value is skipped, never panics.
        let toml = r##"
background = "#1é1e2"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]
"##;
        assert!(parse(toml, "x").is_err());
    }

    #[test]
    fn title_case_defaults() {
        assert_eq!(title_case("my_cool_theme"), "My Cool Theme");
        assert_eq!(title_case("dracula"), "Dracula");
        assert_eq!(title_case("ayu-mirage"), "Ayu Mirage");
    }

    #[test]
    fn valid_flat_palette_theme() {
        let toml = r##"
name = "mine"
display_name = "Mine"
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]
"##;
        let t = parse(toml, "file_stem").unwrap();
        assert_eq!(t.name.as_ref(), "mine");
        assert_eq!(t.display_name.as_ref(), "Mine");
        assert_eq!(t.bg, [30, 30, 46, 255]); // always opaque
        assert_eq!(t.fg, [205, 214, 244]);
        assert_eq!(t.cursor, [245, 224, 220]);
        assert_eq!(t.palette[0], [0, 0, 0]);
        assert_eq!(t.palette[15], [15, 15, 15]);
        // Optional keys absent → computed defaults at render time.
        assert_eq!((t.cursor_text, t.selection_fg), (None, None));
    }

    #[test]
    fn optional_cursor_text_and_selection_foreground() {
        let toml = r##"
background = "#101010"
foreground = "#eeeeee"
cursor = "#ffffff"
cursor_text = "#202020"
selection_fg = "#fafafa"
palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]
"##;
        let t = parse(toml, "x").unwrap();
        assert_eq!(t.cursor_text, Some([0x20, 0x20, 0x20]));
        assert_eq!(t.selection_fg, Some([0xfa, 0xfa, 0xfa]), "`selection_fg` alias");
        let bad = toml.replace("#202020", "nope");
        assert!(parse(&bad, "x").is_err(), "a bad optional color is still a bad theme");
    }

    const PAL16: &str = r##"palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]"##;

    fn with_keys(extra: &str) -> Result<jetty_core::Theme, String> {
        let toml = format!(
            "background = \"#101010\"\nforeground = \"#eeeeee\"\ncursor = \"#ffffff\"\n{extra}\n{PAL16}\n"
        );
        parse(&toml, "x")
    }

    #[test]
    fn optional_accent_and_selection_background() {
        let t = with_keys("accent = \"#ff8800\"\nselection_background = \"#334455\"").unwrap();
        assert_eq!(t.accent, Some([0xff, 0x88, 0x00]));
        assert_eq!(t.selection_bg, Some([0x33, 0x44, 0x55]));
        // `selection_bg` alias.
        let t = with_keys("selection_bg = \"#abc\"").unwrap();
        assert_eq!(t.selection_bg, Some([0xaa, 0xbb, 0xcc]));
        // Absent → derived at render time.
        let t = with_keys("").unwrap();
        assert_eq!((t.accent, t.selection_bg), (None, None));
        // A bad value in a documented key is a bad theme, like every other color.
        assert!(with_keys("accent = \"nope\"").is_err());
        assert!(with_keys("selection_background = \"#12\"").is_err());
    }

    #[test]
    fn the_older_selection_key_is_honored_only_as_a_plain_color() {
        // A color string: read as the selection background.
        let t = with_keys("selection = \"#224466\"").unwrap();
        assert_eq!(t.selection_bg, Some([0x22, 0x44, 0x66]));
        // `selection_background` wins over it.
        let t = with_keys("selection_background = \"#010203\"\nselection = \"#224466\"").unwrap();
        assert_eq!(t.selection_bg, Some([1, 2, 3]));
        // It used to be ignored, so a bad value or another shape must not drop
        // the theme: a junk string and an alacritty-style table both load.
        let t = with_keys("selection = \"not a color\"").unwrap();
        assert_eq!(t.selection_bg, None);
        let toml = format!(
            "background = \"#101010\"\nforeground = \"#eeeeee\"\ncursor = \"#ffffff\"\n{PAL16}\n\
             [selection]\nbackground = \"#ffffff\"\ntext = \"#000000\"\n"
        );
        let t = parse(&toml, "x").expect("a [selection] table must not fail the file");
        assert_eq!(t.selection_bg, None);
    }

    #[test]
    fn named_tables_theme_and_aliases() {
        // `bg`/`fg` aliases + [normal]/[bright] tables (no flat palette).
        let toml = r##"
bg = "#101010"
fg = "#eeeeee"
cursor = "#ffffff"
[normal]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#cccccc"
[bright]
black = "#111111"
red = "#ff1111"
green = "#11ff11"
yellow = "#ffff11"
blue = "#1111ff"
magenta = "#ff11ff"
cyan = "#11ffff"
white = "#ffffff"
"##;
        let t = parse(toml, "themed").unwrap();
        assert_eq!(t.name.as_ref(), "themed"); // defaulted from stem
        assert_eq!(t.display_name.as_ref(), "Themed"); // title-cased default
        assert_eq!(t.bg, [16, 16, 16, 255]);
        assert_eq!(t.palette[1], [255, 0, 0]); // normal red
        assert_eq!(t.palette[9], [255, 17, 17]); // bright red
        assert_eq!(t.palette[15], [255, 255, 255]); // bright white
    }

    #[test]
    fn flat_palette_wins_over_named_tables() {
        let toml = r##"
background = "#101010"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#aa0000","#aa0001","#aa0002","#aa0003","#aa0004","#aa0005","#aa0006","#aa0007","#aa0008","#aa0009","#aa000a","#aa000b","#aa000c","#aa000d","#aa000e","#aa000f"]
[normal]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#cccccc"
[bright]
black = "#111111"
red = "#ff1111"
green = "#11ff11"
yellow = "#ffff11"
blue = "#1111ff"
magenta = "#ff11ff"
cyan = "#11ffff"
white = "#ffffff"
"##;
        let t = parse(toml, "x").unwrap();
        assert_eq!(t.palette[0], [0xaa, 0, 0], "flat palette must win when both present");
    }

    #[test]
    fn missing_required_field_is_error() {
        // missing background
        let toml = r##"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]
"##;
        assert!(parse(toml, "x").is_err());
    }

    #[test]
    fn missing_palette_is_error() {
        let toml = r##"
background = "#101010"
foreground = "#eeeeee"
cursor = "#ffffff"
"##;
        assert!(parse(toml, "x").is_err(), "a theme without a palette is unusable → skip");
    }

    #[test]
    fn wrong_palette_length_is_error() {
        let toml = r##"
background = "#101010"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#000000","#010101","#020202"]
"##;
        assert!(parse(toml, "x").is_err());
    }

    #[test]
    fn bad_hex_is_error() {
        let toml = r##"
background = "not-a-color"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]
"##;
        assert!(parse(toml, "x").is_err());
    }

    #[test]
    fn theme_dir_problems_are_reported_and_good_files_still_load() {
        let dir = std::env::temp_dir().join(format!("jetty-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let palette = r##"palette = ["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]"##;
        let good = format!("background = \"#101010\"\nforeground = \"#eeeeee\"\ncursor = \"#ffffff\"\n{palette}\n");
        std::fs::write(dir.join("a_good.toml"), &good).unwrap();
        std::fs::write(dir.join("b_badhex.toml"), good.replace("#101010", "#aç")).unwrap();
        std::fs::write(dir.join("c_notoml.toml"), "background = = 1\n").unwrap();
        // An in-flight atomic-save temp file is not a theme.
        std::fs::write(dir.join(".a_good.toml.tmp.42.toml"), "garbage").unwrap();
        let (themes, warnings) = load_user_themes_from(&dir);
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name.as_ref(), "a_good");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("b_badhex.toml"), "{warnings:?}");
        assert!(warnings[1].contains("c_notoml.toml"), "{warnings:?}");
        // A missing dir is simply no user themes.
        let (none, w) = load_user_themes_from(&dir.join("nope"));
        assert!(none.is_empty() && w.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn theme_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jetty-themes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn theme_text(bg: &str) -> String {
        format!("name = \"mine\"\nbackground = \"{bg}\"\nforeground = \"#eeeeee\"\ncursor = \"#ffffff\"\n{PAL16}\n")
    }

    #[test]
    fn a_theme_file_broken_mid_edit_keeps_its_last_good_version() {
        // Saving a theme that is in use with a typo (an editor that saves on
        // every pause) dropped it from the registry: the whole terminal flashed
        // to the fallback theme until the next good save.
        let dir = theme_dir("last-good");
        let path = dir.join("mine.toml");
        std::fs::write(&path, theme_text("#401010")).unwrap();
        let (themes, w) = load_user_themes_from(&dir);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(themes[0].bg, [0x40, 0x10, 0x10, 255]);
        std::fs::write(&path, theme_text("#40101")).unwrap();
        let (themes, w) = load_user_themes_from(&dir);
        assert_eq!(themes.len(), 1, "the last version that loaded stays");
        assert_eq!((themes[0].name.as_ref(), themes[0].bg), ("mine", [0x40, 0x10, 0x10, 255]));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("themes/mine.toml") && w[0].contains("keeping the last version that loaded"), "{w:?}");
        assert!(w[0].contains("`background`"), "names the key: {w:?}");
        // Fixed: the new version.
        std::fs::write(&path, theme_text("#102030")).unwrap();
        let (themes, w) = load_user_themes_from(&dir);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(themes[0].bg, [0x10, 0x20, 0x30, 255]);
        // Deleted: gone (that is deliberate) — and not resurrected by a broken
        // file of the same name later.
        std::fs::remove_file(&path).unwrap();
        assert!(load_user_themes_from(&dir).0.is_empty());
        std::fs::write(&path, theme_text("#nothex")).unwrap();
        let (themes, w) = load_user_themes_from(&dir);
        assert!(themes.is_empty());
        assert!(w[0].contains("skipped"), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_errors_say_where() {
        let dir = theme_dir("where");
        std::fs::write(dir.join("a.toml"), "background = \"#101010\nforeground = \"#eeeeee\"\n").unwrap();
        std::fs::write(dir.join("b.toml"), theme_text("#101010").replace("\"#030303\"", "\"#03030\"")).unwrap();
        std::fs::write(dir.join("c.toml"), theme_text("#101010").replace("palette = [", "palette = [1, ")).unwrap();
        let (_, w) = load_user_themes_from(&dir);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].starts_with("theme file themes/a.toml skipped: line 1, column"), "{w:?}");
        assert!(w[1].contains("`palette[3]`: bad hex color \"#03030\""), "{w:?}");
        assert!(w[2].contains("line 5, column"), "a wrong type has a place too: {w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_misspelled_theme_key_is_reported_and_the_theme_still_loads() {
        let dir = theme_dir("typo");
        let text = theme_text("#101010") + "selection_backgrond = \"#334455\"\nauthor = \"me\"\n";
        std::fs::write(dir.join("mine.toml"), text).unwrap();
        let (themes, w) = load_user_themes_from(&dir);
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].selection_bg, None, "the misspelled key does nothing");
        // Only a key close to a real one is worth a warning (`author` is not).
        assert_eq!(
            w,
            ["theme file themes/mine.toml: unknown key `selection_backgrond` is ignored — did you mean `selection_background`?"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_theme_name_that_finds_nothing_gets_the_closest_one() {
        assert_eq!(name_hint("draculaa"), " — did you mean \"dracula\"?");
        assert_eq!(name_hint("solarized_lite"), " — did you mean \"solarized_light\"?");
        assert_eq!(name_hint("tokyonight storm"), " — did you mean \"tokyo_night_storm\"?");
        assert_eq!(name_hint("qqqq"), "");
    }

    #[test]
    fn the_known_theme_keys_cover_the_file_format() {
        for f in crate::config::check::fields_of::<ThemeToml>() {
            assert!(KNOWN_KEYS.contains(f), "{f} is read but not listed");
        }
    }

    #[test]
    fn merge_shadows_builtin_and_appends_new() {
        let dracula = jetty_core::Theme {
            name: Cow::Owned("dracula".to_string()),
            display_name: Cow::Owned("My Dracula".to_string()),
            bg: [1, 2, 3, 255],
            fg: [4, 5, 6],
            cursor: [7, 8, 9],
            cursor_text: None,
            selection_fg: None,
            accent: None,
            selection_bg: None,
            palette: [[0, 0, 0]; 16],
        };
        let novel = jetty_core::Theme {
            name: Cow::Owned("novel".to_string()),
            display_name: Cow::Owned("Novel".to_string()),
            bg: [9, 9, 9, 255],
            fg: [4, 5, 6],
            cursor: [7, 8, 9],
            cursor_text: None,
            selection_fg: None,
            accent: None,
            selection_bg: None,
            palette: [[0, 0, 0]; 16],
        };
        let merged = merge_into_builtins(vec![dracula, novel]);
        // Length = builtins + 1 appended.
        assert_eq!(merged.len(), jetty_core::theme::PRESETS.len() + 1);
        // dracula shadowed IN PLACE (keeps its ordered index 3), new theme appended.
        let di = jetty_core::theme::PRESETS.iter().position(|&n| n == "dracula").unwrap();
        assert_eq!(merged[di].display_name.as_ref(), "My Dracula");
        assert_eq!(merged[di].bg, [1, 2, 3, 255]);
        assert_eq!(merged.last().unwrap().name.as_ref(), "novel");
    }
}
