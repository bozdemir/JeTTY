use std::borrow::Cow;
use std::sync::RwLock;

/// Ordered list of built-in theme preset names. Indices are stable — new themes
/// are only ever APPENDED — so they can serve as the ordered `theme_idx` the
/// Settings list and the command palette's `Theme: …` entries index.
///
/// These are widely-loved community terminal palettes used verbatim (exact
/// hex), not hand-tuned approximations; each fn names its upstream source.
pub const PRESETS: [&str; 46] = [
    "catppuccin_mocha", "tokyo_night", "gruvbox_dark", "dracula", "onyx", "nord",
    "solarized_dark", "solarized_light", "one_dark", "monokai", "monokai_pro",
    "everforest_dark", "rose_pine", "kanagawa", "material_dark", "ayu_dark", "ayu_mirage",
    "tomorrow_night", "oceanic_next", "github_dark", "palenight", "catppuccin_macchiato",
    // v2 (2026-10): light / retro / popular additions.
    "catppuccin_latte", "tokyo_night_storm", "rose_pine_dawn", "gruvbox_light", "synthwave_84",
    "phosphor_green", "phosphor_amber", "kanagawa_dragon", "github_light", "night_owl", "carbonfox",
    "catppuccin_frappe", "rose_pine_moon", "alucard", "flexoki_light", "everforest_light",
    "poimandres", "melange_dark", "tokyo_night_moon", "tokyo_night_day", "kanagawa_lotus",
    "iceberg", "flexoki_dark", "dayfox",
];

/// Terminal color theme: background, foreground, cursor, and the 16-color ANSI palette.
///
/// `name`/`display_name` are `Cow<'static, str>` so the 46 built-ins stay
/// zero-allocation (`Cow::Borrowed`) while user-imported themes own their strings
/// (`Cow::Owned`). Comparisons/formatting go through `Deref<Target=str>`.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: Cow<'static, str>,         // stable id used in config + PRESETS (snake_case)
    pub display_name: Cow<'static, str>, // human-facing label shown in the Settings theme list
    pub bg: [u8; 4],           // background RGBA (alpha < 255 => transparent)
    pub fg: [u8; 3],           // default foreground
    pub cursor: [u8; 3],       // cursor block color
    /// Color of the glyph under a solid block cursor. `None` = the theme bg (the
    /// classic inverted cell), falling back to fg if bg is too close to `cursor`.
    pub cursor_text: Option<[u8; 3]>,
    /// Color of every selected glyph. `None` = keep each glyph's own color unless
    /// it would be unreadable on the selection highlight.
    pub selection_fg: Option<[u8; 3]>,
    /// The UI accent (menu hover, active handles, the welcome logo, …). `None` =
    /// derived: ANSI blue (`palette[4]`), or bright blue / a shade of it when that
    /// is too faint on the UI surface (see jetty-render's `UiPalette`).
    pub accent: Option<[u8; 3]>,
    /// The selection highlight painted under selected cells. `None` = derived
    /// (1/3 bg + 2/3 ANSI blue).
    pub selection_bg: Option<[u8; 3]>,
    pub palette: [[u8; 3]; 16], // standard ANSI 0..=15
}

/// Runtime theme registry: the ORDERED merged list of built-ins + user-imported
/// themes, populated once at startup and on every hot-reload by jetty-app
/// (`themes::rebuild_registry`). `by_name`/`theme_at`/`theme_index`/`theme_count`
/// consult it so a user theme can shadow a built-in and the picker/cycle can index
/// it. Empty until seeded (e.g. a bare `jetty-core` unit test or `jetty-shot`
/// before `rebuild_registry`), in which case every accessor falls back to the
/// hardcoded 46 built-ins — so the crate is always usable with no registry set.
static REGISTRY: RwLock<Vec<Theme>> = RwLock::new(Vec::new());

/// Replace the whole registry with `entries` (built-ins first, then user themes; a
/// user theme whose `name` equals a built-in has already REPLACED it in place by the
/// caller's merge). Called once at startup and on every hot-reload.
pub fn set_registry(entries: Vec<Theme>) {
    *REGISTRY.write().unwrap() = entries;
}

/// The 46 built-in themes in `PRESETS` order (each a fresh, owned `Theme`). Used by
/// jetty-app to seed the registry (built-ins + user themes).
pub fn builtins() -> Vec<Theme> {
    PRESETS.iter().map(|&n| builtin_by_name(n)).collect()
}

/// Number of themes available: the registry length, or the built-in count when the
/// registry is empty (never zero — `PRESETS` has 46). The picker/cycle bound.
pub fn theme_count() -> usize {
    let n = REGISTRY.read().unwrap().len();
    if n == 0 { PRESETS.len() } else { n }
}

/// Resolve the theme at ordered index `idx`, cloning. NEVER panics on a stale/out-of-
/// range index (the list is dynamic — a custom theme can be deleted between frames):
/// falls back to `catppuccin_mocha`. Reads the registry, or the built-ins when empty.
/// Does NOT call `by_name` (which also locks) — avoids a reentrant read lock.
pub fn theme_at(idx: usize) -> Theme {
    let reg = REGISTRY.read().unwrap();
    if reg.is_empty() {
        PRESETS.get(idx).map(|&n| builtin_by_name(n)).unwrap_or_else(catppuccin_mocha)
    } else {
        reg.get(idx).cloned().unwrap_or_else(catppuccin_mocha)
    }
}

/// Ordered index of the theme named `name`, or `None` when absent. Consults the
/// registry (user shadow included), else the built-in `PRESETS` order. An exact
/// id wins; otherwise the name matches loosely — in any letter case, with `-`
/// or spaces for `_`, or as the display name the Settings gallery shows
/// (`"Solarized Light"`, `"tokyo-night"`, `"Dracula"`).
pub fn theme_index(name: &str) -> Option<usize> {
    let reg = REGISTRY.read().unwrap();
    let key = loose_name(name);
    if reg.is_empty() {
        PRESETS.iter().position(|&n| n == name).or_else(|| {
            PRESETS.iter().position(|&n| {
                !key.is_empty() && (loose_name(n) == key || loose_name(&builtin_by_name(n).display_name) == key)
            })
        })
    } else {
        reg.iter().position(|t| t.name.as_ref() == name).or_else(|| {
            reg.iter()
                .position(|t| !key.is_empty() && (loose_name(&t.name) == key || loose_name(&t.display_name) == key))
        })
    }
}

/// A theme name with case and separators dropped, for [`theme_index`]'s loose
/// match: `Solarized Light`, `solarized-light` and `solarized_light` agree.
fn loose_name(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Ordered `(name, display_name)` pairs for the picker/cycle. Built-ins then user
/// themes when the registry is seeded; the 46 built-ins otherwise.
pub fn theme_list() -> Vec<(String, String)> {
    let reg = REGISTRY.read().unwrap();
    if reg.is_empty() {
        PRESETS
            .iter()
            .map(|&n| {
                let t = builtin_by_name(n);
                (t.name.into_owned(), t.display_name.into_owned())
            })
            .collect()
    } else {
        reg.iter()
            .map(|t| (t.name.to_string(), t.display_name.to_string()))
            .collect()
    }
}

/// Resolve a built-in theme by name (pure match, NO registry lock). Unknown names
/// fall back to catppuccin_mocha. Kept separate from `by_name` so registry-holding
/// callers can resolve a built-in without re-entering the `RwLock`.
fn builtin_by_name(name: &str) -> Theme {
    match name {
            "catppuccin_mocha" => catppuccin_mocha(),
            "tokyo_night" => tokyo_night(),
            "gruvbox_dark" => gruvbox_dark(),
            "dracula" => dracula(),
            "onyx" => onyx(),
            "nord" => nord(),
            "solarized_dark" => solarized_dark(),
            "solarized_light" => solarized_light(),
            "one_dark" => one_dark(),
            "monokai" => monokai(),
            "monokai_pro" => monokai_pro(),
            "everforest_dark" => everforest_dark(),
            "rose_pine" => rose_pine(),
            "kanagawa" => kanagawa(),
            "material_dark" => material_dark(),
            "ayu_dark" => ayu_dark(),
            "ayu_mirage" => ayu_mirage(),
            "tomorrow_night" => tomorrow_night(),
            "oceanic_next" => oceanic_next(),
            "github_dark" => github_dark(),
            "palenight" => palenight(),
            "catppuccin_macchiato" => catppuccin_macchiato(),
            "catppuccin_latte" => catppuccin_latte(),
            "tokyo_night_storm" => tokyo_night_storm(),
            "rose_pine_dawn" => rose_pine_dawn(),
            "gruvbox_light" => gruvbox_light(),
            "synthwave_84" => synthwave_84(),
            "phosphor_green" => phosphor_green(),
            "phosphor_amber" => phosphor_amber(),
            "kanagawa_dragon" => kanagawa_dragon(),
            "github_light" => github_light(),
            "night_owl" => night_owl(),
            "carbonfox" => carbonfox(),
            "catppuccin_frappe" => catppuccin_frappe(),
            "rose_pine_moon" => rose_pine_moon(),
            "alucard" => alucard(),
            "flexoki_light" => flexoki_light(),
            "everforest_light" => everforest_light(),
            "poimandres" => poimandres(),
            "melange_dark" => melange_dark(),
            "tokyo_night_moon" => tokyo_night_moon(),
            "tokyo_night_day" => tokyo_night_day(),
            "kanagawa_lotus" => kanagawa_lotus(),
            "iceberg" => iceberg(),
            "flexoki_dark" => flexoki_dark(),
            "dayfox" => dayfox(),
            _ => catppuccin_mocha(),
        }
}

impl Theme {
    /// Resolve a theme by name string. Consults the runtime registry FIRST (a user
    /// theme whose `name` matches shadows the built-in), then the hardcoded 46
    /// built-ins, then any other spelling [`theme_index`] accepts (`"Solarized
    /// Light"`), then falls back to catppuccin_mocha. Cheap in the common built-in
    /// case; a user theme clones its owned strings.
    pub fn by_name(name: &str) -> Theme {
        if let Some(t) = REGISTRY.read().unwrap().iter().find(|t| t.name.as_ref() == name) {
            return t.clone();
        }
        if PRESETS.contains(&name) {
            return builtin_by_name(name);
        }
        // (The registry's read lock above is released: `theme_index` and
        // `theme_at` take it again.)
        theme_index(name).map_or_else(catppuccin_mocha, theme_at)
    }

    /// A guaranteed-visible error red for the OSC 133 failed-command marker.
    ///
    /// Raw ANSI red (`palette[1]`) is low-contrast on several presets (the
    /// Solarized family), so this picks whichever of ANSI red (`palette[1]`) or
    /// ANSI bright red (`palette[9]`) reads best against the background: the
    /// brighter red on a dark background, the deeper red on a light one. Returned
    /// with full alpha so the left-edge accent bar is crisp on every theme.
    pub fn failed_marker_color(&self) -> [u8; 4] {
        let lum = |c: [u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
        let bg_l = lum([self.bg[0], self.bg[1], self.bg[2]]);
        let red = self.palette[1];
        let bright = self.palette[9];
        // Light background (luma > ~140): the darker red contrasts more; dark
        // background: the brighter red pops.
        let base = if bg_l > 140.0 {
            if lum(red) <= lum(bright) { red } else { bright }
        } else if lum(bright) >= lum(red) {
            bright
        } else {
            red
        };
        [base[0], base[1], base[2], 255]
    }
}

/// Catppuccin Mocha — the soothing pastel dark theme (catppuccin.com). Default.
pub fn catppuccin_mocha() -> Theme {
    Theme {
        name: Cow::Borrowed("catppuccin_mocha"),
        display_name: Cow::Borrowed("Catppuccin Mocha"),
        bg: [30, 30, 46, 255],   // base   #1e1e2e
        fg: [205, 214, 244],     // text   #cdd6f4
        cursor: [245, 224, 220], // rosewater #f5e0dc
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [69, 71, 90],    // 0  surface1 #45475a
            [243, 139, 168], // 1  red      #f38ba8
            [166, 227, 161], // 2  green    #a6e3a1
            [249, 226, 175], // 3  yellow   #f9e2af
            [137, 180, 250], // 4  blue     #89b4fa
            [245, 194, 231], // 5  pink     #f5c2e7
            [148, 226, 213], // 6  teal     #94e2d5
            [186, 194, 222], // 7  subtext1 #bac2de
            [88, 91, 112],   // 8  surface2 #585b70
            [243, 139, 168], // 9  red      #f38ba8
            [166, 227, 161], // 10 green    #a6e3a1
            [249, 226, 175], // 11 yellow   #f9e2af
            [137, 180, 250], // 12 blue     #89b4fa
            [245, 194, 231], // 13 pink     #f5c2e7
            [148, 226, 213], // 14 teal     #94e2d5
            [166, 173, 200], // 15 subtext0 #a6adc8
        ],
    }
}

/// Tokyo Night — the popular dark blue scheme (enkia/tokyonight). Brights per the
/// current upstream extras — https://github.com/folke/tokyonight.nvim/blob/main/extras/kitty/tokyonight_night.conf
/// (Apache-2.0); they used to repeat the normals.
pub fn tokyo_night() -> Theme {
    Theme {
        name: Cow::Borrowed("tokyo_night"),
        display_name: Cow::Borrowed("Tokyo Night"),
        bg: [26, 27, 38, 255],   // #1a1b26
        fg: [192, 202, 245],     // #c0caf5
        cursor: [192, 202, 245], // #c0caf5
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [21, 22, 30],    // 0  #15161e
            [247, 118, 142], // 1  #f7768e
            [158, 206, 106], // 2  #9ece6a
            [224, 175, 104], // 3  #e0af68
            [122, 162, 247], // 4  #7aa2f7
            [187, 154, 247], // 5  #bb9af7
            [125, 207, 255], // 6  #7dcfff
            [169, 177, 214], // 7  #a9b1d6
            [65, 72, 104],   // 8  #414868
            [255, 137, 157], // 9  #ff899d
            [159, 224, 68],  // 10 #9fe044
            [250, 186, 74],  // 11 #faba4a
            [141, 176, 255], // 12 #8db0ff
            [199, 169, 255], // 13 #c7a9ff
            [164, 218, 255], // 14 #a4daff
            [192, 202, 245], // 15 #c0caf5
        ],
    }
}

/// Gruvbox Dark — Pavel Pertsev's retro-groove color scheme.
pub fn gruvbox_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("gruvbox_dark"),
        display_name: Cow::Borrowed("Gruvbox Dark"),
        bg: [40, 40, 40, 255],
        fg: [235, 219, 178],
        cursor: [251, 241, 199],
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [40, 40, 40],    // 0  black (dark0)
            [204, 36, 29],   // 1  red
            [152, 151, 26],  // 2  green
            [215, 153, 33],  // 3  yellow
            [69, 133, 136],  // 4  blue
            [177, 98, 134],  // 5  magenta
            [104, 157, 106], // 6  cyan
            [168, 153, 132], // 7  white (light4)
            [146, 131, 116], // 8  bright black (gray)
            [251, 73, 52],   // 9  bright red
            [184, 187, 38],  // 10 bright green
            [250, 189, 47],  // 11 bright yellow
            [131, 165, 152], // 12 bright blue
            [211, 134, 155], // 13 bright magenta
            [142, 192, 124], // 14 bright cyan
            [235, 219, 178], // 15 bright white (fg1)
        ],
    }
}

/// Dracula — the famous dark theme (draculatheme.com).
pub fn dracula() -> Theme {
    Theme {
        name: Cow::Borrowed("dracula"),
        display_name: Cow::Borrowed("Dracula"),
        bg: [40, 42, 54, 255],   // #282a36
        fg: [248, 248, 242],     // #f8f8f2
        cursor: [248, 248, 242], // #f8f8f2
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [33, 34, 44],    // 0  #21222c
            [255, 85, 85],   // 1  #ff5555
            [80, 250, 123],  // 2  #50fa7b
            [241, 250, 140], // 3  #f1fa8c
            [189, 147, 249], // 4  #bd93f9
            [255, 121, 198], // 5  #ff79c6
            [139, 233, 253], // 6  #8be9fd
            [248, 248, 242], // 7  #f8f8f2
            [98, 114, 164],  // 8  #6272a4
            [255, 110, 110], // 9  #ff6e6e
            [105, 255, 148], // 10 #69ff94
            [255, 255, 165], // 11 #ffffa5
            [214, 172, 255], // 12 #d6acff
            [255, 146, 223], // 13 #ff92df
            [164, 255, 255], // 14 #a4ffff
            [255, 255, 255], // 15 #ffffff
        ],
    }
}

/// Onyx — a clean near-black theme (One Dark-inspired accents on a deep neutral
/// background), matching the soft dark terminal look.
pub fn onyx() -> Theme {
    Theme {
        name: Cow::Borrowed("onyx"),
        display_name: Cow::Borrowed("Onyx"),
        bg: [22, 22, 26, 255],   // #16161a
        fg: [200, 200, 205],     // #c8c8cd
        cursor: [97, 175, 239],  // #61afef
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [58, 60, 66],    // 0  #3a3c42
            [224, 108, 117], // 1  red     #e06c75
            [152, 195, 121], // 2  green   #98c379
            [229, 192, 123], // 3  yellow  #e5c07b
            [97, 175, 239],  // 4  blue    #61afef
            [198, 120, 221], // 5  magenta #c678dd
            [86, 182, 194],  // 6  cyan    #56b6c2
            [171, 178, 191], // 7  white   #abb2bf
            [92, 99, 112],   // 8  br blk  #5c6370
            [224, 108, 117], // 9  #e06c75
            [152, 195, 121], // 10 #98c379
            [229, 192, 123], // 11 #e5c07b
            [97, 175, 239],  // 12 #61afef
            [198, 120, 221], // 13 #c678dd
            [86, 182, 194],  // 14 #56b6c2
            [220, 223, 228], // 15 br wht  #dcdfe4
        ],
    }
}


/// Nord — nordtheme.com (exact hex).
pub fn nord() -> Theme {
    Theme {
        name: Cow::Borrowed("nord"),
        display_name: Cow::Borrowed("Nord"),
        bg: [46, 52, 64, 255],   // #2e3440
        fg: [216, 222, 233],     // #d8dee9
        cursor: [236, 239, 244], // #eceff4
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [59, 66, 82], // 0  black      #3b4252
            [191, 97, 106], // 1  red        #bf616a
            [163, 190, 140], // 2  green      #a3be8c
            [235, 203, 139], // 3  yellow     #ebcb8b
            [129, 161, 193], // 4  blue       #81a1c1
            [180, 142, 173], // 5  magenta    #b48ead
            [136, 192, 208], // 6  cyan       #88c0d0
            [229, 233, 240], // 7  white      #e5e9f0
            [76, 86, 106], // 8  br black   #4c566a
            [191, 97, 106], // 9  br red     #bf616a
            [163, 190, 140], // 10 br green   #a3be8c
            [235, 203, 139], // 11 br yellow  #ebcb8b
            [129, 161, 193], // 12 br blue    #81a1c1
            [180, 142, 173], // 13 br magenta #b48ead
            [143, 188, 187], // 14 br cyan    #8fbcbb
            [236, 239, 244], // 15 br white   #eceff4
        ],
    }
}

/// Solarized Dark — ethanschoonover.com/solarized (exact hex).
pub fn solarized_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("solarized_dark"),
        display_name: Cow::Borrowed("Solarized Dark"),
        bg: [0, 43, 54, 255],   // #002b36
        fg: [131, 148, 150],     // #839496
        cursor: [131, 148, 150], // #839496
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [7, 54, 66], // 0  black      #073642
            [220, 50, 47], // 1  red        #dc322f
            [133, 153, 0], // 2  green      #859900
            [181, 137, 0], // 3  yellow     #b58900
            [38, 139, 210], // 4  blue       #268bd2
            [211, 54, 130], // 5  magenta    #d33682
            [42, 161, 152], // 6  cyan       #2aa198
            [238, 232, 213], // 7  white      #eee8d5
            [0, 43, 54], // 8  br black   #002b36
            [203, 75, 22], // 9  br red     #cb4b16
            [88, 110, 117], // 10 br green   #586e75
            [101, 123, 131], // 11 br yellow  #657b83
            [131, 148, 150], // 12 br blue    #839496
            [108, 113, 196], // 13 br magenta #6c71c4
            [147, 161, 161], // 14 br cyan    #93a1a1
            [253, 246, 227], // 15 br white   #fdf6e3
        ],
    }
}

/// Solarized Light — ethanschoonover.com/solarized (exact hex).
pub fn solarized_light() -> Theme {
    Theme {
        name: Cow::Borrowed("solarized_light"),
        display_name: Cow::Borrowed("Solarized Light"),
        bg: [253, 246, 227, 255],   // #fdf6e3
        fg: [101, 123, 131],     // #657b83
        cursor: [101, 123, 131], // #657b83
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [7, 54, 66], // 0  black      #073642
            [220, 50, 47], // 1  red        #dc322f
            [133, 153, 0], // 2  green      #859900
            [181, 137, 0], // 3  yellow     #b58900
            [38, 139, 210], // 4  blue       #268bd2
            [211, 54, 130], // 5  magenta    #d33682
            [42, 161, 152], // 6  cyan       #2aa198
            [238, 232, 213], // 7  white      #eee8d5
            [0, 43, 54], // 8  br black   #002b36
            [203, 75, 22], // 9  br red     #cb4b16
            [88, 110, 117], // 10 br green   #586e75
            [101, 123, 131], // 11 br yellow  #657b83
            [131, 148, 150], // 12 br blue    #839496
            [108, 113, 196], // 13 br magenta #6c71c4
            [147, 161, 161], // 14 br cyan    #93a1a1
            [253, 246, 227], // 15 br white   #fdf6e3
        ],
    }
}

/// One Dark — Atom One Dark (exact hex).
pub fn one_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("one_dark"),
        display_name: Cow::Borrowed("One Dark"),
        bg: [33, 37, 43, 255],   // #21252b
        fg: [171, 178, 191],     // #abb2bf
        cursor: [171, 178, 191], // #abb2bf
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [33, 37, 43], // 0  black      #21252b
            [224, 108, 117], // 1  red        #e06c75
            [152, 195, 121], // 2  green      #98c379
            [229, 192, 123], // 3  yellow     #e5c07b
            [97, 175, 239], // 4  blue       #61afef
            [198, 120, 221], // 5  magenta    #c678dd
            [86, 182, 194], // 6  cyan       #56b6c2
            [171, 178, 191], // 7  white      #abb2bf
            [118, 118, 118], // 8  br black   #767676
            [224, 108, 117], // 9  br red     #e06c75
            [152, 195, 121], // 10 br green   #98c379
            [229, 192, 123], // 11 br yellow  #e5c07b
            [97, 175, 239], // 12 br blue    #61afef
            [198, 120, 221], // 13 br magenta #c678dd
            [86, 182, 194], // 14 br cyan    #56b6c2
            [171, 178, 191], // 15 br white   #abb2bf
        ],
    }
}

/// Monokai — Monokai Classic (exact hex).
pub fn monokai() -> Theme {
    Theme {
        name: Cow::Borrowed("monokai"),
        display_name: Cow::Borrowed("Monokai"),
        bg: [39, 40, 34, 255],   // #272822
        fg: [253, 255, 241],     // #fdfff1
        cursor: [192, 193, 181], // #c0c1b5
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [39, 40, 34], // 0  black      #272822
            [249, 38, 114], // 1  red        #f92672
            [166, 226, 46], // 2  green      #a6e22e
            [230, 219, 116], // 3  yellow     #e6db74
            [253, 151, 31], // 4  blue       #fd971f
            [174, 129, 255], // 5  magenta    #ae81ff
            [102, 217, 239], // 6  cyan       #66d9ef
            [253, 255, 241], // 7  white      #fdfff1
            [110, 112, 102], // 8  br black   #6e7066
            [249, 38, 114], // 9  br red     #f92672
            [166, 226, 46], // 10 br green   #a6e22e
            [230, 219, 116], // 11 br yellow  #e6db74
            [253, 151, 31], // 12 br blue    #fd971f
            [174, 129, 255], // 13 br magenta #ae81ff
            [102, 217, 239], // 14 br cyan    #66d9ef
            [253, 255, 241], // 15 br white   #fdfff1
        ],
    }
}

/// Monokai Pro — monokai.pro (exact hex).
pub fn monokai_pro() -> Theme {
    Theme {
        name: Cow::Borrowed("monokai_pro"),
        display_name: Cow::Borrowed("Monokai Pro"),
        bg: [45, 42, 46, 255],   // #2d2a2e
        fg: [252, 252, 250],     // #fcfcfa
        cursor: [193, 192, 192], // #c1c0c0
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [45, 42, 46], // 0  black      #2d2a2e
            [255, 97, 136], // 1  red        #ff6188
            [169, 220, 118], // 2  green      #a9dc76
            [255, 216, 102], // 3  yellow     #ffd866
            [252, 152, 103], // 4  blue       #fc9867
            [171, 157, 242], // 5  magenta    #ab9df2
            [120, 220, 232], // 6  cyan       #78dce8
            [252, 252, 250], // 7  white      #fcfcfa
            [114, 112, 114], // 8  br black   #727072
            [255, 97, 136], // 9  br red     #ff6188
            [169, 220, 118], // 10 br green   #a9dc76
            [255, 216, 102], // 11 br yellow  #ffd866
            [252, 152, 103], // 12 br blue    #fc9867
            [171, 157, 242], // 13 br magenta #ab9df2
            [120, 220, 232], // 14 br cyan    #78dce8
            [252, 252, 250], // 15 br white   #fcfcfa
        ],
    }
}

/// Everforest Dark — sainnhe/everforest (exact hex). Bright red…cyan (9–14) are
/// the dark-medium accents, == the normals as in everforest.vim's terminal block
/// (https://github.com/sainnhe/everforest/blob/master/colors/everforest.vim, MIT);
/// they used to be the LIGHT variant's accents. Bright black / white (8, 15) keep
/// the port's grays: upstream's bg3 #475258 is 1.55:1 on the bg (zsh-autosuggestions
/// draw in color 8) and its bright white would sit below normal white #f2efdf.
pub fn everforest_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("everforest_dark"),
        display_name: Cow::Borrowed("Everforest Dark"),
        bg: [45, 53, 59, 255],   // #2d353b
        fg: [211, 198, 170],     // #d3c6aa
        cursor: [230, 152, 117], // #e69875
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [122, 132, 120], // 0  black      #7a8478
            [230, 126, 128], // 1  red        #e67e80
            [167, 192, 128], // 2  green      #a7c080
            [219, 188, 127], // 3  yellow     #dbbc7f
            [127, 187, 179], // 4  blue       #7fbbb3
            [214, 153, 182], // 5  magenta    #d699b6
            [131, 192, 146], // 6  cyan       #83c092
            [242, 239, 223], // 7  white      #f2efdf
            [166, 176, 160], // 8  br black   #a6b0a0
            [230, 126, 128], // 9  br red     #e67e80
            [167, 192, 128], // 10 br green   #a7c080
            [219, 188, 127], // 11 br yellow  #dbbc7f
            [127, 187, 179], // 12 br blue    #7fbbb3
            [214, 153, 182], // 13 br magenta #d699b6
            [131, 192, 146], // 14 br cyan    #83c092
            [255, 251, 239], // 15 br white   #fffbef
        ],
    }
}

/// Rose Pine — rosepinetheme.com (exact hex).
pub fn rose_pine() -> Theme {
    Theme {
        name: Cow::Borrowed("rose_pine"),
        display_name: Cow::Borrowed("Rose Pine"),
        bg: [25, 23, 36, 255],   // #191724
        fg: [224, 222, 244],     // #e0def4
        cursor: [224, 222, 244], // #e0def4
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [38, 35, 58], // 0  black      #26233a
            [235, 111, 146], // 1  red        #eb6f92
            [49, 116, 143], // 2  green      #31748f
            [246, 193, 119], // 3  yellow     #f6c177
            [156, 207, 216], // 4  blue       #9ccfd8
            [196, 167, 231], // 5  magenta    #c4a7e7
            [235, 188, 186], // 6  cyan       #ebbcba
            [224, 222, 244], // 7  white      #e0def4
            [110, 106, 134], // 8  br black   #6e6a86
            [235, 111, 146], // 9  br red     #eb6f92
            [49, 116, 143], // 10 br green   #31748f
            [246, 193, 119], // 11 br yellow  #f6c177
            [156, 207, 216], // 12 br blue    #9ccfd8
            [196, 167, 231], // 13 br magenta #c4a7e7
            [235, 188, 186], // 14 br cyan    #ebbcba
            [224, 222, 244], // 15 br white   #e0def4
        ],
    }
}

/// Kanagawa — rebelot/kanagawa.nvim (exact hex).
pub fn kanagawa() -> Theme {
    Theme {
        name: Cow::Borrowed("kanagawa"),
        display_name: Cow::Borrowed("Kanagawa"),
        bg: [31, 31, 40, 255],   // #1f1f28
        fg: [220, 215, 186],     // #dcd7ba
        cursor: [220, 215, 186], // #dcd7ba
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [22, 22, 29], // 0  black      #16161d
            [195, 64, 67], // 1  red        #c34043
            [118, 148, 106], // 2  green      #76946a
            [192, 163, 110], // 3  yellow     #c0a36e
            [126, 156, 216], // 4  blue       #7e9cd8
            [149, 127, 184], // 5  magenta    #957fb8
            [106, 149, 137], // 6  cyan       #6a9589
            [200, 192, 147], // 7  white      #c8c093
            [114, 113, 105], // 8  br black   #727169
            [232, 36, 36], // 9  br red     #e82424
            [152, 187, 108], // 10 br green   #98bb6c
            [230, 195, 132], // 11 br yellow  #e6c384
            [127, 180, 202], // 12 br blue    #7fb4ca
            [147, 138, 169], // 13 br magenta #938aa9
            [122, 168, 159], // 14 br cyan    #7aa89f
            [220, 215, 186], // 15 br white   #dcd7ba
        ],
    }
}

/// Material — Material Dark (exact hex).
pub fn material_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("material_dark"),
        display_name: Cow::Borrowed("Material"),
        bg: [35, 35, 34, 255],   // #232322
        fg: [229, 229, 229],     // #e5e5e5
        cursor: [22, 175, 202], // #16afca
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [33, 33, 33], // 0  black      #212121
            [183, 20, 31], // 1  red        #b7141f
            [69, 123, 36], // 2  green      #457b24
            [246, 152, 30], // 3  yellow     #f6981e
            [19, 78, 178], // 4  blue       #134eb2
            [112, 26, 162], // 5  magenta    #701aa2
            [14, 113, 124], // 6  cyan       #0e717c
            [239, 239, 239], // 7  white      #efefef
            [79, 79, 79], // 8  br black   #4f4f4f
            [232, 59, 63], // 9  br red     #e83b3f
            [122, 186, 58], // 10 br green   #7aba3a
            [255, 234, 46], // 11 br yellow  #ffea2e
            [84, 164, 243], // 12 br blue    #54a4f3
            [170, 77, 188], // 13 br magenta #aa4dbc
            [38, 187, 209], // 14 br cyan    #26bbd1
            [217, 217, 217], // 15 br white   #d9d9d9
        ],
    }
}

/// Ayu Dark — ayu-theme/ayu (exact hex).
pub fn ayu_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("ayu_dark"),
        display_name: Cow::Borrowed("Ayu Dark"),
        bg: [11, 14, 20, 255],   // #0b0e14
        fg: [191, 189, 182],     // #bfbdb6
        cursor: [230, 180, 80], // #e6b450
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [17, 21, 28], // 0  black      #11151c
            [234, 108, 115], // 1  red        #ea6c73
            [127, 217, 98], // 2  green      #7fd962
            [249, 175, 79], // 3  yellow     #f9af4f
            [83, 189, 250], // 4  blue       #53bdfa
            [205, 161, 250], // 5  magenta    #cda1fa
            [144, 225, 198], // 6  cyan       #90e1c6
            [199, 199, 199], // 7  white      #c7c7c7
            [104, 104, 104], // 8  br black   #686868
            [240, 113, 120], // 9  br red     #f07178
            [170, 217, 76], // 10 br green   #aad94c
            [255, 180, 84], // 11 br yellow  #ffb454
            [89, 194, 255], // 12 br blue    #59c2ff
            [210, 166, 255], // 13 br magenta #d2a6ff
            [149, 230, 203], // 14 br cyan    #95e6cb
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Ayu Mirage — ayu-theme/ayu (exact hex).
pub fn ayu_mirage() -> Theme {
    Theme {
        name: Cow::Borrowed("ayu_mirage"),
        display_name: Cow::Borrowed("Ayu Mirage"),
        bg: [31, 36, 48, 255],   // #1f2430
        fg: [204, 202, 194],     // #cccac2
        cursor: [255, 204, 102], // #ffcc66
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [23, 27, 36], // 0  black      #171b24
            [237, 130, 116], // 1  red        #ed8274
            [135, 217, 108], // 2  green      #87d96c
            [250, 204, 110], // 3  yellow     #facc6e
            [109, 203, 250], // 4  blue       #6dcbfa
            [218, 186, 250], // 5  magenta    #dabafa
            [144, 225, 198], // 6  cyan       #90e1c6
            [199, 199, 199], // 7  white      #c7c7c7
            [104, 104, 104], // 8  br black   #686868
            [242, 135, 121], // 9  br red     #f28779
            [213, 255, 128], // 10 br green   #d5ff80
            [255, 209, 115], // 11 br yellow  #ffd173
            [115, 208, 255], // 12 br blue    #73d0ff
            [223, 191, 255], // 13 br magenta #dfbfff
            [149, 230, 203], // 14 br cyan    #95e6cb
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Tomorrow Night — chriskempson/tomorrow (exact hex).
pub fn tomorrow_night() -> Theme {
    Theme {
        name: Cow::Borrowed("tomorrow_night"),
        display_name: Cow::Borrowed("Tomorrow Night"),
        bg: [29, 31, 33, 255],   // #1d1f21
        fg: [197, 200, 198],     // #c5c8c6
        cursor: [197, 200, 198], // #c5c8c6
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [0, 0, 0], // 0  black      #000000
            [204, 102, 102], // 1  red        #cc6666
            [181, 189, 104], // 2  green      #b5bd68
            [240, 198, 116], // 3  yellow     #f0c674
            [129, 162, 190], // 4  blue       #81a2be
            [178, 148, 187], // 5  magenta    #b294bb
            [138, 190, 183], // 6  cyan       #8abeb7
            [255, 255, 255], // 7  white      #ffffff
            [76, 76, 76], // 8  br black   #4c4c4c
            [204, 102, 102], // 9  br red     #cc6666
            [181, 189, 104], // 10 br green   #b5bd68
            [240, 198, 116], // 11 br yellow  #f0c674
            [129, 162, 190], // 12 br blue    #81a2be
            [178, 148, 187], // 13 br magenta #b294bb
            [138, 190, 183], // 14 br cyan    #8abeb7
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Oceanic Next — voronianski/oceanic-next (exact hex).
pub fn oceanic_next() -> Theme {
    Theme {
        name: Cow::Borrowed("oceanic_next"),
        display_name: Cow::Borrowed("Oceanic Next"),
        bg: [22, 44, 53, 255],   // #162c35
        fg: [192, 197, 206],     // #c0c5ce
        cursor: [192, 197, 206], // #c0c5ce
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [22, 44, 53], // 0  black      #162c35
            [236, 95, 103], // 1  red        #ec5f67
            [153, 199, 148], // 2  green      #99c794
            [250, 200, 99], // 3  yellow     #fac863
            [102, 153, 204], // 4  blue       #6699cc
            [197, 148, 197], // 5  magenta    #c594c5
            [95, 179, 179], // 6  cyan       #5fb3b3
            [255, 255, 255], // 7  white      #ffffff
            [101, 115, 126], // 8  br black   #65737e
            [236, 95, 103], // 9  br red     #ec5f67
            [153, 199, 148], // 10 br green   #99c794
            [250, 200, 99], // 11 br yellow  #fac863
            [102, 153, 204], // 12 br blue    #6699cc
            [197, 148, 197], // 13 br magenta #c594c5
            [95, 179, 179], // 14 br cyan    #5fb3b3
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// GitHub Dark — primer/github-vscode-theme (exact hex).
pub fn github_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("github_dark"),
        display_name: Cow::Borrowed("GitHub Dark"),
        bg: [13, 17, 23, 255],   // #0d1117
        fg: [201, 209, 217],     // #c9d1d9
        cursor: [88, 166, 255], // #58a6ff
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [72, 79, 88], // 0  black      #484f58
            [255, 123, 114], // 1  red        #ff7b72
            [63, 185, 80], // 2  green      #3fb950
            [210, 153, 34], // 3  yellow     #d29922
            [88, 166, 255], // 4  blue       #58a6ff
            [188, 140, 255], // 5  magenta    #bc8cff
            [57, 197, 207], // 6  cyan       #39c5cf
            [177, 186, 196], // 7  white      #b1bac4
            [110, 118, 129], // 8  br black   #6e7681
            [255, 161, 152], // 9  br red     #ffa198
            [86, 211, 100], // 10 br green   #56d364
            [227, 179, 65], // 11 br yellow  #e3b341
            [121, 192, 255], // 12 br blue    #79c0ff
            [210, 168, 255], // 13 br magenta #d2a8ff
            [86, 212, 221], // 14 br cyan    #56d4dd
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Palenight — material palenight (exact hex).
pub fn palenight() -> Theme {
    Theme {
        name: Cow::Borrowed("palenight"),
        display_name: Cow::Borrowed("Palenight"),
        bg: [41, 45, 62, 255],   // #292d3e
        fg: [191, 199, 213],     // #bfc7d5
        cursor: [126, 87, 194], // #7e57c2
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [103, 110, 149], // 0  black      #676e95
            [255, 85, 114], // 1  red        #ff5572
            [169, 199, 125], // 2  green      #a9c77d
            [255, 203, 107], // 3  yellow     #ffcb6b
            [130, 170, 255], // 4  blue       #82aaff
            [199, 146, 234], // 5  magenta    #c792ea
            [137, 221, 255], // 6  cyan       #89ddff
            [255, 255, 255], // 7  white      #ffffff
            [103, 110, 149], // 8  br black   #676e95
            [255, 85, 114], // 9  br red     #ff5572
            [195, 232, 141], // 10 br green   #c3e88d
            [255, 203, 107], // 11 br yellow  #ffcb6b
            [130, 170, 255], // 12 br blue    #82aaff
            [199, 146, 234], // 13 br magenta #c792ea
            [137, 221, 255], // 14 br cyan    #89ddff
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Catppuccin Macchiato — catppuccin (exact hex).
pub fn catppuccin_macchiato() -> Theme {
    Theme {
        name: Cow::Borrowed("catppuccin_macchiato"),
        display_name: Cow::Borrowed("Catppuccin Macchiato"),
        bg: [36, 39, 58, 255],   // #24273a
        fg: [202, 211, 245],     // #cad3f5
        cursor: [244, 219, 214], // #f4dbd6
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [73, 77, 100], // 0  black      #494d64
            [237, 135, 150], // 1  red        #ed8796
            [166, 218, 149], // 2  green      #a6da95
            [238, 212, 159], // 3  yellow     #eed49f
            [138, 173, 244], // 4  blue       #8aadf4
            [245, 189, 230], // 5  magenta    #f5bde6
            [139, 213, 202], // 6  cyan       #8bd5ca
            [184, 192, 224], // 7  white      #b8c0e0
            [91, 96, 120], // 8  br black   #5b6078
            [237, 135, 150], // 9  br red     #ed8796
            [166, 218, 149], // 10 br green   #a6da95
            [238, 212, 159], // 11 br yellow  #eed49f
            [138, 173, 244], // 12 br blue    #8aadf4
            [245, 189, 230], // 13 br magenta #f5bde6
            [139, 213, 202], // 14 br cyan    #8bd5ca
            [165, 173, 203], // 15 br white   #a5adcb
        ],
    }
}

/// Catppuccin Latte — https://github.com/catppuccin/kitty/blob/main/themes/latte.conf (MIT, exact hex).
/// Upstream cursor_text (= bg) is only 2.34:1 on the cursor, so it is left unset and JeTTY's
/// rule draws the glyph in fg (3.02:1).
pub fn catppuccin_latte() -> Theme {
    Theme {
        name: Cow::Borrowed("catppuccin_latte"),
        display_name: Cow::Borrowed("Catppuccin Latte"),
        bg: [239, 241, 245, 255], // #eff1f5
        fg: [76, 79, 105], // #4c4f69
        cursor: [220, 138, 120], // #dc8a78
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [92, 95, 119], // 0  black      #5c5f77
            [210, 15, 57], // 1  red        #d20f39
            [64, 160, 43], // 2  green      #40a02b
            [223, 142, 29], // 3  yellow     #df8e1d
            [30, 102, 245], // 4  blue       #1e66f5
            [234, 118, 203], // 5  magenta    #ea76cb
            [23, 146, 153], // 6  cyan       #179299
            [172, 176, 190], // 7  white      #acb0be
            [108, 111, 133], // 8  br black   #6c6f85
            [210, 15, 57], // 9  br red     #d20f39
            [64, 160, 43], // 10 br green   #40a02b
            [223, 142, 29], // 11 br yellow  #df8e1d
            [30, 102, 245], // 12 br blue    #1e66f5
            [234, 118, 203], // 13 br magenta #ea76cb
            [23, 146, 153], // 14 br cyan    #179299
            [188, 192, 204], // 15 br white   #bcc0cc
        ],
    }
}

/// Tokyo Night Storm — https://github.com/folke/tokyonight.nvim/blob/main/extras/kitty/tokyonight_storm.conf (Apache-2.0, exact hex).
/// Current upstream extras: the brights differ from the normals.
pub fn tokyo_night_storm() -> Theme {
    Theme {
        name: Cow::Borrowed("tokyo_night_storm"),
        display_name: Cow::Borrowed("Tokyo Night Storm"),
        bg: [36, 40, 59, 255], // #24283b
        fg: [192, 202, 245], // #c0caf5
        cursor: [192, 202, 245], // #c0caf5
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [29, 32, 47], // 0  black      #1d202f
            [247, 118, 142], // 1  red        #f7768e
            [158, 206, 106], // 2  green      #9ece6a
            [224, 175, 104], // 3  yellow     #e0af68
            [122, 162, 247], // 4  blue       #7aa2f7
            [187, 154, 247], // 5  magenta    #bb9af7
            [125, 207, 255], // 6  cyan       #7dcfff
            [169, 177, 214], // 7  white      #a9b1d6
            [65, 72, 104], // 8  br black   #414868
            [255, 137, 157], // 9  br red     #ff899d
            [159, 224, 68], // 10 br green   #9fe044
            [250, 186, 74], // 11 br yellow  #faba4a
            [141, 176, 255], // 12 br blue    #8db0ff
            [199, 169, 255], // 13 br magenta #c7a9ff
            [164, 218, 255], // 14 br cyan    #a4daff
            [192, 202, 245], // 15 br white   #c0caf5
        ],
    }
}

/// Rose Pine Dawn — https://github.com/rose-pine/kitty/blob/main/dist/rose-pine-dawn.conf (MIT, exact hex).
/// Cursor = fg, like the shipped rose_pine (upstream #cecacd is 1.48:1 on the bg).
pub fn rose_pine_dawn() -> Theme {
    Theme {
        name: Cow::Borrowed("rose_pine_dawn"),
        display_name: Cow::Borrowed("Rose Pine Dawn"),
        bg: [250, 244, 237, 255], // #faf4ed
        fg: [87, 82, 121], // #575279
        cursor: [87, 82, 121], // #575279
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [242, 233, 225], // 0  black      #f2e9e1
            [180, 99, 122], // 1  red        #b4637a
            [40, 105, 131], // 2  green      #286983
            [234, 157, 52], // 3  yellow     #ea9d34
            [86, 148, 159], // 4  blue       #56949f
            [144, 122, 169], // 5  magenta    #907aa9
            [215, 130, 126], // 6  cyan       #d7827e
            [87, 82, 121], // 7  white      #575279
            [152, 147, 165], // 8  br black   #9893a5
            [180, 99, 122], // 9  br red     #b4637a
            [40, 105, 131], // 10 br green   #286983
            [234, 157, 52], // 11 br yellow  #ea9d34
            [86, 148, 159], // 12 br blue    #56949f
            [144, 122, 169], // 13 br magenta #907aa9
            [215, 130, 126], // 14 br cyan    #d7827e
            [87, 82, 121], // 15 br white   #575279
        ],
    }
}

/// Gruvbox Light — https://github.com/morhetz/gruvbox-contrib/blob/master/termite/gruvbox-light (MIT/X11 (per morhetz/gruvbox README), exact hex).
/// Cursor = fg1 (the st port's defaultcs = 15).
pub fn gruvbox_light() -> Theme {
    Theme {
        name: Cow::Borrowed("gruvbox_light"),
        display_name: Cow::Borrowed("Gruvbox Light"),
        bg: [251, 241, 199, 255], // #fbf1c7
        fg: [60, 56, 54], // #3c3836
        cursor: [60, 56, 54], // #3c3836
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [251, 241, 199], // 0  black      #fbf1c7
            [204, 36, 29], // 1  red        #cc241d
            [152, 151, 26], // 2  green      #98971a
            [215, 153, 33], // 3  yellow     #d79921
            [69, 133, 136], // 4  blue       #458588
            [177, 98, 134], // 5  magenta    #b16286
            [104, 157, 106], // 6  cyan       #689d6a
            [124, 111, 100], // 7  white      #7c6f64
            [146, 131, 116], // 8  br black   #928374
            [157, 0, 6], // 9  br red     #9d0006
            [121, 116, 14], // 10 br green   #79740e
            [181, 118, 20], // 11 br yellow  #b57614
            [7, 102, 120], // 12 br blue    #076678
            [143, 63, 113], // 13 br magenta #8f3f71
            [66, 123, 88], // 14 br cyan    #427b58
            [60, 56, 54], // 15 br white   #3c3836
        ],
    }
}

/// Synthwave '84 — https://github.com/robb0wen/synthwave-vscode/blob/master/themes/synthwave-color-theme.json (MIT, exact hex).
/// Upstream defines 12 hues; black / white / bright black / bright white are VS Code's dark
/// defaults. Its glyph-under-cursor #ffffff (1.4:1) is left unset.
pub fn synthwave_84() -> Theme {
    Theme {
        name: Cow::Borrowed("synthwave_84"),
        display_name: Cow::Borrowed("Synthwave '84"),
        bg: [38, 35, 53, 255], // #262335
        fg: [255, 255, 255], // #ffffff
        cursor: [3, 237, 249], // #03edf9
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [0, 0, 0], // 0  black      #000000
            [254, 68, 80], // 1  red        #fe4450
            [114, 241, 184], // 2  green      #72f1b8
            [243, 231, 15], // 3  yellow     #f3e70f
            [3, 237, 249], // 4  blue       #03edf9
            [255, 126, 219], // 5  magenta    #ff7edb
            [3, 237, 249], // 6  cyan       #03edf9
            [229, 229, 229], // 7  white      #e5e5e5
            [102, 102, 102], // 8  br black   #666666
            [254, 68, 80], // 9  br red     #fe4450
            [114, 241, 184], // 10 br green   #72f1b8
            [254, 222, 93], // 11 br yellow  #fede5d
            [3, 237, 249], // 12 br blue    #03edf9
            [255, 126, 219], // 13 br magenta #ff7edb
            [3, 237, 249], // 14 br cyan    #03edf9
            [229, 229, 229], // 15 br white   #e5e5e5
        ],
    }
}

/// Phosphor Green — https://github.com/livlign/posh-palette/blob/main/schemes/green-phosphor.json (MIT, exact hex).
/// Upstream 'Green Phosphor CRT'. Monochrome: ANSI red is green (#00aa00), so error / bell /
/// failed-command accents are green too.
pub fn phosphor_green() -> Theme {
    Theme {
        name: Cow::Borrowed("phosphor_green"),
        display_name: Cow::Borrowed("Phosphor Green"),
        bg: [11, 15, 11, 255], // #0b0f0b
        fg: [51, 255, 51], // #33ff33
        cursor: [51, 255, 51], // #33ff33
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [0, 34, 0], // 0  black      #002200
            [0, 170, 0], // 1  red        #00aa00
            [51, 255, 51], // 2  green      #33ff33
            [102, 255, 102], // 3  yellow     #66ff66
            [0, 204, 68], // 4  blue       #00cc44
            [0, 255, 136], // 5  magenta    #00ff88
            [102, 255, 170], // 6  cyan       #66ffaa
            [182, 255, 182], // 7  white      #b6ffb6
            [10, 90, 10], // 8  br black   #0a5a0a
            [25, 204, 25], // 9  br red     #19cc19
            [102, 255, 102], // 10 br green   #66ff66
            [153, 255, 153], // 11 br yellow  #99ff99
            [51, 255, 119], // 12 br blue    #33ff77
            [102, 255, 170], // 13 br magenta #66ffaa
            [153, 255, 204], // 14 br cyan    #99ffcc
            [230, 255, 230], // 15 br white   #e6ffe6
        ],
    }
}

/// Phosphor Amber — https://github.com/livlign/posh-palette/blob/main/schemes/amber-crt.json (MIT, exact hex).
/// Upstream 'Amber CRT Retro'. Monochrome amber: ANSI red is orange (#ff6a00).
pub fn phosphor_amber() -> Theme {
    Theme {
        name: Cow::Borrowed("phosphor_amber"),
        display_name: Cow::Borrowed("Phosphor Amber"),
        bg: [26, 18, 0, 255], // #1a1200
        fg: [255, 176, 0], // #ffb000
        cursor: [255, 176, 0], // #ffb000
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [42, 30, 0], // 0  black      #2a1e00
            [255, 106, 0], // 1  red        #ff6a00
            [255, 176, 0], // 2  green      #ffb000
            [255, 199, 66], // 3  yellow     #ffc742
            [255, 140, 0], // 4  blue       #ff8c00
            [255, 160, 51], // 5  magenta    #ffa033
            [255, 210, 127], // 6  cyan       #ffd27f
            [255, 217, 160], // 7  white      #ffd9a0
            [106, 78, 0], // 8  br black   #6a4e00
            [255, 138, 30], // 9  br red     #ff8a1e
            [255, 199, 66], // 10 br green   #ffc742
            [255, 224, 138], // 11 br yellow  #ffe08a
            [255, 171, 61], // 12 br blue    #ffab3d
            [255, 194, 102], // 13 br magenta #ffc266
            [255, 227, 176], // 14 br cyan    #ffe3b0
            [255, 240, 214], // 15 br white   #fff0d6
        ],
    }
}

/// Kanagawa Dragon — https://github.com/rebelot/kanagawa.nvim/blob/master/extras/ghostty/kanagawa-dragon (MIT, exact hex).
/// Same 16 colors in the upstream alacritty / kitty / ghostty extras.
pub fn kanagawa_dragon() -> Theme {
    Theme {
        name: Cow::Borrowed("kanagawa_dragon"),
        display_name: Cow::Borrowed("Kanagawa Dragon"),
        bg: [24, 22, 22, 255], // #181616
        fg: [197, 201, 197], // #c5c9c5
        cursor: [200, 192, 147], // #c8c093
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [13, 12, 12], // 0  black      #0d0c0c
            [196, 116, 110], // 1  red        #c4746e
            [138, 154, 123], // 2  green      #8a9a7b
            [196, 178, 138], // 3  yellow     #c4b28a
            [139, 164, 176], // 4  blue       #8ba4b0
            [162, 146, 163], // 5  magenta    #a292a3
            [142, 164, 162], // 6  cyan       #8ea4a2
            [200, 192, 147], // 7  white      #c8c093
            [166, 166, 156], // 8  br black   #a6a69c
            [228, 104, 118], // 9  br red     #e46876
            [135, 169, 135], // 10 br green   #87a987
            [230, 195, 132], // 11 br yellow  #e6c384
            [127, 180, 202], // 12 br blue    #7fb4ca
            [147, 138, 169], // 13 br magenta #938aa9
            [122, 168, 159], // 14 br cyan    #7aa89f
            [197, 201, 197], // 15 br white   #c5c9c5
        ],
    }
}

/// GitHub Light — https://github.com/primer/github-vscode-theme/blob/main/src/theme.js (MIT, exact hex).
/// Same source as github_dark (github-vscode-theme 6.3.5 -> @primer/primitives 7.10.0). ANSI
/// yellow is dark brown #4d2d00 by design.
pub fn github_light() -> Theme {
    Theme {
        name: Cow::Borrowed("github_light"),
        display_name: Cow::Borrowed("GitHub Light"),
        bg: [255, 255, 255, 255], // #ffffff
        fg: [36, 41, 47], // #24292f
        cursor: [9, 105, 218], // #0969da
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [36, 41, 47], // 0  black      #24292f
            [207, 34, 46], // 1  red        #cf222e
            [17, 99, 41], // 2  green      #116329
            [77, 45, 0], // 3  yellow     #4d2d00
            [9, 105, 218], // 4  blue       #0969da
            [130, 80, 223], // 5  magenta    #8250df
            [27, 124, 131], // 6  cyan       #1b7c83
            [110, 119, 129], // 7  white      #6e7781
            [87, 96, 106], // 8  br black   #57606a
            [164, 14, 38], // 9  br red     #a40e26
            [26, 127, 55], // 10 br green   #1a7f37
            [99, 60, 1], // 11 br yellow  #633c01
            [33, 139, 255], // 12 br blue    #218bff
            [164, 117, 249], // 13 br magenta #a475f9
            [49, 146, 170], // 14 br cyan    #3192aa
            [140, 149, 159], // 15 br white   #8c959f
        ],
    }
}

/// Night Owl — https://github.com/sdras/night-owl-vscode-theme/blob/main/themes/Night%20Owl-color-theme.json (MIT, exact hex).
/// fg = editor.foreground (upstream sets no terminal.foreground); cursor =
/// editorCursor.foreground.
pub fn night_owl() -> Theme {
    Theme {
        name: Cow::Borrowed("night_owl"),
        display_name: Cow::Borrowed("Night Owl"),
        bg: [1, 22, 39, 255], // #011627
        fg: [214, 222, 235], // #d6deeb
        cursor: [128, 164, 194], // #80a4c2
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [1, 22, 39], // 0  black      #011627
            [239, 83, 80], // 1  red        #ef5350
            [34, 218, 110], // 2  green      #22da6e
            [197, 228, 120], // 3  yellow     #c5e478
            [130, 170, 255], // 4  blue       #82aaff
            [199, 146, 234], // 5  magenta    #c792ea
            [33, 199, 168], // 6  cyan       #21c7a8
            [255, 255, 255], // 7  white      #ffffff
            [87, 86, 86], // 8  br black   #575656
            [239, 83, 80], // 9  br red     #ef5350
            [34, 218, 110], // 10 br green   #22da6e
            [255, 235, 149], // 11 br yellow  #ffeb95
            [130, 170, 255], // 12 br blue    #82aaff
            [199, 146, 234], // 13 br magenta #c792ea
            [127, 219, 202], // 14 br cyan    #7fdbca
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Carbonfox — https://github.com/EdenEast/nightfox.nvim/blob/main/extra/carbonfox/kitty.conf (MIT, exact hex).
/// ANSI yellow is teal (#08bdba) and cyan is azure (#33b1ff) by design (IBM Carbon has no
/// yellow).
pub fn carbonfox() -> Theme {
    Theme {
        name: Cow::Borrowed("carbonfox"),
        display_name: Cow::Borrowed("Carbonfox"),
        bg: [22, 22, 22, 255], // #161616
        fg: [242, 244, 248], // #f2f4f8
        cursor: [242, 244, 248], // #f2f4f8
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [40, 40, 40], // 0  black      #282828
            [238, 83, 150], // 1  red        #ee5396
            [37, 190, 106], // 2  green      #25be6a
            [8, 189, 186], // 3  yellow     #08bdba
            [120, 169, 255], // 4  blue       #78a9ff
            [190, 149, 255], // 5  magenta    #be95ff
            [51, 177, 255], // 6  cyan       #33b1ff
            [223, 223, 224], // 7  white      #dfdfe0
            [72, 72, 72], // 8  br black   #484848
            [241, 109, 166], // 9  br red     #f16da6
            [70, 200, 128], // 10 br green   #46c880
            [45, 199, 196], // 11 br yellow  #2dc7c4
            [140, 182, 255], // 12 br blue    #8cb6ff
            [200, 165, 255], // 13 br magenta #c8a5ff
            [82, 189, 255], // 14 br cyan    #52bdff
            [228, 228, 229], // 15 br white   #e4e4e5
        ],
    }
}

/// Catppuccin Frappe — https://github.com/catppuccin/kitty/blob/main/themes/frappe.conf (MIT, exact hex).
/// Same white / bright-white order as catppuccin_mocha / _macchiato.
pub fn catppuccin_frappe() -> Theme {
    Theme {
        name: Cow::Borrowed("catppuccin_frappe"),
        display_name: Cow::Borrowed("Catppuccin Frappe"),
        bg: [48, 52, 70, 255], // #303446
        fg: [198, 208, 245], // #c6d0f5
        cursor: [242, 213, 207], // #f2d5cf
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [81, 87, 109], // 0  black      #51576d
            [231, 130, 132], // 1  red        #e78284
            [166, 209, 137], // 2  green      #a6d189
            [229, 200, 144], // 3  yellow     #e5c890
            [140, 170, 238], // 4  blue       #8caaee
            [244, 184, 228], // 5  magenta    #f4b8e4
            [129, 200, 190], // 6  cyan       #81c8be
            [181, 191, 226], // 7  white      #b5bfe2
            [98, 104, 128], // 8  br black   #626880
            [231, 130, 132], // 9  br red     #e78284
            [166, 209, 137], // 10 br green   #a6d189
            [229, 200, 144], // 11 br yellow  #e5c890
            [140, 170, 238], // 12 br blue    #8caaee
            [244, 184, 228], // 13 br magenta #f4b8e4
            [129, 200, 190], // 14 br cyan    #81c8be
            [165, 173, 206], // 15 br white   #a5adce
        ],
    }
}

/// Rose Pine Moon — https://github.com/rose-pine/kitty/blob/main/dist/rose-pine-moon.conf (MIT, exact hex).
/// Cursor = fg, like the shipped rose_pine (upstream #56526e is 2.11:1 on the bg).
pub fn rose_pine_moon() -> Theme {
    Theme {
        name: Cow::Borrowed("rose_pine_moon"),
        display_name: Cow::Borrowed("Rose Pine Moon"),
        bg: [35, 33, 54, 255], // #232136
        fg: [224, 222, 244], // #e0def4
        cursor: [224, 222, 244], // #e0def4
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [57, 53, 82], // 0  black      #393552
            [235, 111, 146], // 1  red        #eb6f92
            [62, 143, 176], // 2  green      #3e8fb0
            [246, 193, 119], // 3  yellow     #f6c177
            [156, 207, 216], // 4  blue       #9ccfd8
            [196, 167, 231], // 5  magenta    #c4a7e7
            [234, 154, 151], // 6  cyan       #ea9a97
            [224, 222, 244], // 7  white      #e0def4
            [110, 106, 134], // 8  br black   #6e6a86
            [235, 111, 146], // 9  br red     #eb6f92
            [62, 143, 176], // 10 br green   #3e8fb0
            [246, 193, 119], // 11 br yellow  #f6c177
            [156, 207, 216], // 12 br blue    #9ccfd8
            [196, 167, 231], // 13 br magenta #c4a7e7
            [234, 154, 151], // 14 br cyan    #ea9a97
            [224, 222, 244], // 15 br white   #e0def4
        ],
    }
}

/// Alucard (Dracula Light) — https://draculatheme.com/spec (MIT, exact hex).
/// Dracula's official light variant (spec 'Alucard Classic'): ANSI black == bg, ANSI white ==
/// fg. Cursor = fg like dracula.
pub fn alucard() -> Theme {
    Theme {
        name: Cow::Borrowed("alucard"),
        display_name: Cow::Borrowed("Alucard (Dracula Light)"),
        bg: [255, 251, 235, 255], // #fffbeb
        fg: [31, 31, 31], // #1f1f1f
        cursor: [31, 31, 31], // #1f1f1f
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [255, 251, 235], // 0  black      #fffbeb
            [203, 58, 42], // 1  red        #cb3a2a
            [20, 113, 10], // 2  green      #14710a
            [132, 110, 21], // 3  yellow     #846e15
            [100, 74, 201], // 4  blue       #644ac9
            [163, 20, 77], // 5  magenta    #a3144d
            [3, 106, 150], // 6  cyan       #036a96
            [31, 31, 31], // 7  white      #1f1f1f
            [108, 102, 75], // 8  br black   #6c664b
            [215, 76, 61], // 9  br red     #d74c3d
            [25, 141, 12], // 10 br green   #198d0c
            [158, 132, 26], // 11 br yellow  #9e841a
            [120, 98, 208], // 12 br blue    #7862d0
            [191, 24, 90], // 13 br magenta #bf185a
            [4, 127, 180], // 14 br cyan    #047fb4
            [44, 43, 49], // 15 br white   #2c2b31
        ],
    }
}

/// Flexoki Light — https://github.com/kepano/flexoki/blob/main/kitty/flexoki_light.conf (MIT, exact hex).
/// The author-signed kitty port (the repo's alacritty files disagree).
pub fn flexoki_light() -> Theme {
    Theme {
        name: Cow::Borrowed("flexoki_light"),
        display_name: Cow::Borrowed("Flexoki Light"),
        bg: [255, 252, 240, 255], // #fffcf0
        fg: [16, 15, 15], // #100f0f
        cursor: [16, 15, 15], // #100f0f
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [16, 15, 15], // 0  black      #100f0f
            [209, 77, 65], // 1  red        #d14d41
            [135, 154, 57], // 2  green      #879a39
            [208, 162, 21], // 3  yellow     #d0a215
            [67, 133, 190], // 4  blue       #4385be
            [206, 93, 151], // 5  magenta    #ce5d97
            [58, 169, 159], // 6  cyan       #3aa99f
            [255, 252, 240], // 7  white      #fffcf0
            [111, 110, 105], // 8  br black   #6f6e69
            [175, 48, 41], // 9  br red     #af3029
            [102, 128, 11], // 10 br green   #66800b
            [173, 131, 1], // 11 br yellow  #ad8301
            [32, 94, 166], // 12 br blue    #205ea6
            [160, 47, 111], // 13 br magenta #a02f6f
            [36, 131, 123], // 14 br cyan    #24837b
            [242, 240, 229], // 15 br white   #f2f0e5
        ],
    }
}

/// Everforest Light — https://github.com/sainnhe/everforest/blob/master/colors/everforest.vim (MIT, exact hex).
/// everforest.vim light / medium: black = fg, white = bg3, brights == normals.
pub fn everforest_light() -> Theme {
    Theme {
        name: Cow::Borrowed("everforest_light"),
        display_name: Cow::Borrowed("Everforest Light"),
        bg: [253, 246, 227, 255], // #fdf6e3
        fg: [92, 106, 114], // #5c6a72
        cursor: [92, 106, 114], // #5c6a72
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [92, 106, 114], // 0  black      #5c6a72
            [248, 85, 82], // 1  red        #f85552
            [141, 161, 1], // 2  green      #8da101
            [223, 160, 0], // 3  yellow     #dfa000
            [58, 148, 197], // 4  blue       #3a94c5
            [223, 105, 186], // 5  magenta    #df69ba
            [53, 167, 124], // 6  cyan       #35a77c
            [230, 226, 204], // 7  white      #e6e2cc
            [92, 106, 114], // 8  br black   #5c6a72
            [248, 85, 82], // 9  br red     #f85552
            [141, 161, 1], // 10 br green   #8da101
            [223, 160, 0], // 11 br yellow  #dfa000
            [58, 148, 197], // 12 br blue    #3a94c5
            [223, 105, 186], // 13 br magenta #df69ba
            [53, 167, 124], // 14 br cyan    #35a77c
            [230, 226, 204], // 15 br white   #e6e2cc
        ],
    }
}

/// Poimandres — https://github.com/drcmda/poimandres-theme/blob/main/themes/poimandres-color-theme.json (MIT, exact hex).
/// All 16 slots + fg defined upstream.
pub fn poimandres() -> Theme {
    Theme {
        name: Cow::Borrowed("poimandres"),
        display_name: Cow::Borrowed("Poimandres"),
        bg: [27, 30, 40, 255], // #1b1e28
        fg: [166, 172, 205], // #a6accd
        cursor: [166, 172, 205], // #a6accd
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [27, 30, 40], // 0  black      #1b1e28
            [208, 103, 157], // 1  red        #d0679d
            [93, 228, 199], // 2  green      #5de4c7
            [255, 250, 194], // 3  yellow     #fffac2
            [137, 221, 255], // 4  blue       #89ddff
            [240, 135, 189], // 5  magenta    #f087bd
            [137, 221, 255], // 6  cyan       #89ddff
            [255, 255, 255], // 7  white      #ffffff
            [166, 172, 205], // 8  br black   #a6accd
            [208, 103, 157], // 9  br red     #d0679d
            [93, 228, 199], // 10 br green   #5de4c7
            [255, 250, 194], // 11 br yellow  #fffac2
            [173, 215, 255], // 12 br blue    #add7ff
            [240, 135, 189], // 13 br magenta #f087bd
            [173, 215, 255], // 14 br cyan    #add7ff
            [255, 255, 255], // 15 br white   #ffffff
        ],
    }
}

/// Melange Dark — https://github.com/savq/melange-nvim/blob/master/term/ghostty/melange_dark (MIT, exact hex).
/// Ghostty port (the kitty port is identical except cursor = none, i.e. reverse video).
pub fn melange_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("melange_dark"),
        display_name: Cow::Borrowed("Melange Dark"),
        bg: [41, 37, 34, 255], // #292522
        fg: [236, 225, 215], // #ece1d7
        cursor: [236, 225, 215], // #ece1d7
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [52, 48, 44], // 0  black      #34302c
            [189, 129, 131], // 1  red        #bd8183
            [120, 153, 122], // 2  green      #78997a
            [228, 155, 93], // 3  yellow     #e49b5d
            [127, 145, 178], // 4  blue       #7f91b2
            [179, 128, 176], // 5  magenta    #b380b0
            [123, 150, 149], // 6  cyan       #7b9695
            [193, 167, 142], // 7  white      #c1a78e
            [134, 116, 98], // 8  br black   #867462
            [212, 119, 102], // 9  br red     #d47766
            [133, 182, 149], // 10 br green   #85b695
            [235, 192, 109], // 11 br yellow  #ebc06d
            [163, 169, 206], // 12 br blue    #a3a9ce
            [207, 155, 194], // 13 br magenta #cf9bc2
            [137, 179, 182], // 14 br cyan    #89b3b6
            [236, 225, 215], // 15 br white   #ece1d7
        ],
    }
}

/// Tokyo Night Moon — https://github.com/folke/tokyonight.nvim/blob/main/extras/kitty/tokyonight_moon.conf (Apache-2.0, exact hex).
pub fn tokyo_night_moon() -> Theme {
    Theme {
        name: Cow::Borrowed("tokyo_night_moon"),
        display_name: Cow::Borrowed("Tokyo Night Moon"),
        bg: [34, 36, 54, 255], // #222436
        fg: [200, 211, 245], // #c8d3f5
        cursor: [200, 211, 245], // #c8d3f5
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [27, 29, 43], // 0  black      #1b1d2b
            [255, 117, 127], // 1  red        #ff757f
            [195, 232, 141], // 2  green      #c3e88d
            [255, 199, 119], // 3  yellow     #ffc777
            [130, 170, 255], // 4  blue       #82aaff
            [192, 153, 255], // 5  magenta    #c099ff
            [134, 225, 252], // 6  cyan       #86e1fc
            [130, 139, 184], // 7  white      #828bb8
            [68, 74, 115], // 8  br black   #444a73
            [255, 141, 148], // 9  br red     #ff8d94
            [199, 251, 109], // 10 br green   #c7fb6d
            [255, 216, 171], // 11 br yellow  #ffd8ab
            [154, 184, 255], // 12 br blue    #9ab8ff
            [202, 171, 255], // 13 br magenta #caabff
            [178, 235, 255], // 14 br cyan    #b2ebff
            [200, 211, 245], // 15 br white   #c8d3f5
        ],
    }
}

/// Tokyo Night Day — https://github.com/folke/tokyonight.nvim/blob/main/extras/kitty/tokyonight_day.conf (Apache-2.0, exact hex).
pub fn tokyo_night_day() -> Theme {
    Theme {
        name: Cow::Borrowed("tokyo_night_day"),
        display_name: Cow::Borrowed("Tokyo Night Day"),
        bg: [225, 226, 231, 255], // #e1e2e7
        fg: [55, 96, 191], // #3760bf
        cursor: [55, 96, 191], // #3760bf
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [180, 181, 185], // 0  black      #b4b5b9
            [245, 42, 101], // 1  red        #f52a65
            [88, 117, 57], // 2  green      #587539
            [140, 108, 62], // 3  yellow     #8c6c3e
            [46, 125, 233], // 4  blue       #2e7de9
            [152, 84, 241], // 5  magenta    #9854f1
            [0, 113, 151], // 6  cyan       #007197
            [97, 114, 176], // 7  white      #6172b0
            [161, 166, 197], // 8  br black   #a1a6c5
            [255, 71, 116], // 9  br red     #ff4774
            [92, 133, 36], // 10 br green   #5c8524
            [162, 118, 41], // 11 br yellow  #a27629
            [53, 138, 255], // 12 br blue    #358aff
            [164, 99, 255], // 13 br magenta #a463ff
            [0, 126, 168], // 14 br cyan    #007ea8
            [55, 96, 191], // 15 br white   #3760bf
        ],
    }
}

/// Kanagawa Lotus — https://github.com/rebelot/kanagawa.nvim/blob/master/extras/ghostty/kanagawa-lotus (MIT, exact hex).
pub fn kanagawa_lotus() -> Theme {
    Theme {
        name: Cow::Borrowed("kanagawa_lotus"),
        display_name: Cow::Borrowed("Kanagawa Lotus"),
        bg: [242, 236, 188, 255], // #f2ecbc
        fg: [84, 84, 100], // #545464
        cursor: [67, 67, 108], // #43436c
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [31, 31, 40], // 0  black      #1f1f28
            [200, 64, 83], // 1  red        #c84053
            [111, 137, 78], // 2  green      #6f894e
            [119, 113, 63], // 3  yellow     #77713f
            [77, 105, 155], // 4  blue       #4d699b
            [179, 91, 121], // 5  magenta    #b35b79
            [89, 123, 117], // 6  cyan       #597b75
            [84, 84, 100], // 7  white      #545464
            [138, 137, 128], // 8  br black   #8a8980
            [215, 71, 75], // 9  br red     #d7474b
            [110, 145, 95], // 10 br green   #6e915f
            [131, 111, 74], // 11 br yellow  #836f4a
            [102, 147, 191], // 12 br blue    #6693bf
            [98, 76, 131], // 13 br magenta #624c83
            [94, 133, 122], // 14 br cyan    #5e857a
            [67, 67, 108], // 15 br white   #43436c
        ],
    }
}

/// Iceberg — https://github.com/cocopon/iceberg.vim/blob/master/colors/iceberg.vim (MIT, exact hex).
pub fn iceberg() -> Theme {
    Theme {
        name: Cow::Borrowed("iceberg"),
        display_name: Cow::Borrowed("Iceberg"),
        bg: [22, 24, 33, 255], // #161821
        fg: [198, 200, 209], // #c6c8d1
        cursor: [198, 200, 209], // #c6c8d1
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [30, 33, 50], // 0  black      #1e2132
            [226, 120, 120], // 1  red        #e27878
            [180, 190, 130], // 2  green      #b4be82
            [226, 164, 120], // 3  yellow     #e2a478
            [132, 160, 198], // 4  blue       #84a0c6
            [160, 147, 199], // 5  magenta    #a093c7
            [137, 184, 194], // 6  cyan       #89b8c2
            [198, 200, 209], // 7  white      #c6c8d1
            [107, 112, 137], // 8  br black   #6b7089
            [233, 137, 137], // 9  br red     #e98989
            [192, 202, 142], // 10 br green   #c0ca8e
            [233, 177, 137], // 11 br yellow  #e9b189
            [145, 172, 209], // 12 br blue    #91acd1
            [173, 160, 211], // 13 br magenta #ada0d3
            [149, 196, 206], // 14 br cyan    #95c4ce
            [210, 212, 222], // 15 br white   #d2d4de
        ],
    }
}

/// Flexoki Dark — https://github.com/kepano/flexoki/blob/main/kitty/flexoki_dark.conf (MIT, exact hex).
pub fn flexoki_dark() -> Theme {
    Theme {
        name: Cow::Borrowed("flexoki_dark"),
        display_name: Cow::Borrowed("Flexoki Dark"),
        bg: [16, 15, 15, 255], // #100f0f
        fg: [206, 205, 195], // #cecdc3
        cursor: [206, 205, 195], // #cecdc3
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [16, 15, 15], // 0  black      #100f0f
            [175, 48, 41], // 1  red        #af3029
            [102, 128, 11], // 2  green      #66800b
            [173, 131, 1], // 3  yellow     #ad8301
            [32, 94, 166], // 4  blue       #205ea6
            [160, 47, 111], // 5  magenta    #a02f6f
            [36, 131, 123], // 6  cyan       #24837b
            [135, 133, 128], // 7  white      #878580
            [111, 110, 105], // 8  br black   #6f6e69
            [209, 77, 65], // 9  br red     #d14d41
            [135, 154, 57], // 10 br green   #879a39
            [208, 162, 21], // 11 br yellow  #d0a215
            [67, 133, 190], // 12 br blue    #4385be
            [206, 93, 151], // 13 br magenta #ce5d97
            [58, 169, 159], // 14 br cyan    #3aa99f
            [206, 205, 195], // 15 br white   #cecdc3
        ],
    }
}

/// Dayfox — https://github.com/EdenEast/nightfox.nvim/blob/main/extra/dayfox/kitty.conf (MIT, exact hex).
pub fn dayfox() -> Theme {
    Theme {
        name: Cow::Borrowed("dayfox"),
        display_name: Cow::Borrowed("Dayfox"),
        bg: [246, 242, 238, 255], // #f6f2ee
        fg: [61, 43, 90], // #3d2b5a
        cursor: [61, 43, 90], // #3d2b5a
        cursor_text: None,
        selection_fg: None,
        accent: None,
        selection_bg: None,
        palette: [
            [53, 44, 36], // 0  black      #352c24
            [165, 34, 47], // 1  red        #a5222f
            [57, 104, 71], // 2  green      #396847
            [172, 84, 2], // 3  yellow     #ac5402
            [40, 72, 169], // 4  blue       #2848a9
            [110, 51, 206], // 5  magenta    #6e33ce
            [40, 121, 128], // 6  cyan       #287980
            [242, 233, 225], // 7  white      #f2e9e1
            [83, 76, 69], // 8  br black   #534c45
            [179, 67, 78], // 9  br red     #b3434e
            [87, 127, 99], // 10 br green   #577f63
            [184, 110, 40], // 11 br yellow  #b86e28
            [72, 99, 182], // 12 br blue    #4863b6
            [132, 82, 213], // 13 br magenta #8452d5
            [72, 141, 147], // 14 br cyan    #488d93
            [244, 236, 230], // 15 br white   #f4ece6
        ],
    }
}
