//! User-imported theme loading.
//!
//! Reads `~/.config/jetty/themes/*.toml`, parses each into a `jetty_core::Theme`,
//! and merges them with the 22 built-ins into the runtime registry (a user theme
//! whose `name` matches a built-in REPLACES it in place; new names append). Parsing
//! lives here (jetty-app already carries serde/toml/dirs); jetty-core holds only the
//! parsed data + the registry.
//!
//! Never panics: a malformed file (bad TOML, bad hex, wrong palette length, missing
//! a required field) is skipped-and-logged; the other themes still load. `read_dir`
//! order is unspecified, so files are SORTED before loading, making "duplicate user
//! name → last wins" deterministic.
//!
//! ## Schema (`~/.config/jetty/themes/<id>.toml`)
//! ```toml
//! name         = "my_theme"     # optional (defaults to the file stem)
//! display_name = "My Theme"     # optional (defaults to a title-cased name)
//! background   = "#1e1e2e"      # required   (alias: bg)
//! foreground   = "#cdd6f4"      # required   (alias: fg)
//! cursor       = "#f5e0dc"      # required
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
//! a GLOBAL setting (config `opacity`), not per-theme — an `opacity`/`selection` key
//! is accepted-and-ignored for forward-compat.

use std::borrow::Cow;

use serde::Deserialize;

/// Raw parsed theme file. Unknown keys (`opacity`, `selection`, …) are ignored by
/// serde (no `deny_unknown_fields`), so they are accepted-and-ignored.
#[derive(Debug, Deserialize)]
struct ThemeToml {
    name: Option<String>,
    display_name: Option<String>,
    #[serde(alias = "bg")]
    background: Option<String>,
    #[serde(alias = "fg")]
    foreground: Option<String>,
    cursor: Option<String>,
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
    fn to_rows(&self) -> Result<[[u8; 3]; 8], String> {
        Ok([
            parse_hex(&self.black)?,
            parse_hex(&self.red)?,
            parse_hex(&self.green)?,
            parse_hex(&self.yellow)?,
            parse_hex(&self.blue)?,
            parse_hex(&self.magenta)?,
            parse_hex(&self.cyan)?,
            parse_hex(&self.white)?,
        ])
    }
}

/// Parse a hex color (`#rrggbb`, `#rgb`, `rrggbb`, or `rgb`) to `[r, g, b]`.
fn parse_hex(s: &str) -> Result<[u8; 3], String> {
    let h = s.trim().trim_start_matches('#');
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

    let bg3 = parse_hex(t.background.as_deref().ok_or("missing `background`")?)?;
    let fg = parse_hex(t.foreground.as_deref().ok_or("missing `foreground`")?)?;
    let cursor = parse_hex(t.cursor.as_deref().ok_or("missing `cursor`")?)?;

    // Palette: flat 16-array WINS when present; else the named [normal]/[bright]
    // tables; else it is a required-field error.
    let palette: [[u8; 3]; 16] = if let Some(list) = t.palette {
        if list.len() != 16 {
            return Err(format!("`palette` must have exactly 16 colors (got {})", list.len()));
        }
        let mut p = [[0u8; 3]; 16];
        for (i, hex) in list.iter().enumerate() {
            p[i] = parse_hex(hex)?;
        }
        p
    } else if let (Some(normal), Some(bright)) = (t.normal.as_ref(), t.bright.as_ref()) {
        let n = normal.to_rows()?;
        let b = bright.to_rows()?;
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
        palette,
    })
}

/// Read `~/.config/jetty/themes/*.toml` into a `Vec<Theme>`. Never panics: a
/// malformed file is skipped-and-logged; a missing directory yields `[]`. Files are
/// sorted by path so duplicate names resolve deterministically (last wins).
pub fn load_user_themes() -> Vec<jetty_core::Theme> {
    let dir = crate::config::Config::dir().join("themes");
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect(),
        Err(_) => return Vec::new(), // no themes dir → no user themes
    };
    files.sort(); // deterministic load order (read_dir order is unspecified)

    let mut out: Vec<jetty_core::Theme> = Vec::new();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("theme")
            .to_string();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("jetty: skipping theme {}: {e}", path.display());
                continue;
            }
        };
        let parsed: ThemeToml = match toml::from_str(&content) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("jetty: skipping theme {}: {e}", path.display());
                continue;
            }
        };
        match theme_from_toml(parsed, &stem) {
            Ok(theme) => {
                // Duplicate user name → last wins (drop the earlier one), logged.
                if let Some(pos) = out.iter().position(|t| t.name == theme.name) {
                    eprintln!(
                        "jetty: duplicate user theme name {:?} (later file wins)",
                        theme.name
                    );
                    out.remove(pos);
                }
                out.push(theme);
            }
            Err(e) => eprintln!("jetty: skipping theme {}: {e}", path.display()),
        }
    }
    out
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
/// on disk. Called once at startup and on every hot-reload of `themes/`.
pub fn rebuild_registry() {
    jetty_core::set_registry(merge_into_builtins(load_user_themes()));
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn merge_shadows_builtin_and_appends_new() {
        let dracula = jetty_core::Theme {
            name: Cow::Owned("dracula".to_string()),
            display_name: Cow::Owned("My Dracula".to_string()),
            bg: [1, 2, 3, 255],
            fg: [4, 5, 6],
            cursor: [7, 8, 9],
            palette: [[0, 0, 0]; 16],
        };
        let novel = jetty_core::Theme {
            name: Cow::Owned("novel".to_string()),
            display_name: Cow::Owned("Novel".to_string()),
            bg: [9, 9, 9, 255],
            fg: [4, 5, 6],
            cursor: [7, 8, 9],
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
