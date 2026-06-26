/// Ordered list of built-in theme preset names. Indices are stable so they
/// can be used as `theme_idx` in the app state (Ctrl+Shift+T cycles through).
///
/// These are widely-loved community terminal palettes used verbatim (exact
/// hex), not hand-tuned approximations.
pub const PRESETS: [&str; 5] = ["catppuccin_mocha", "tokyo_night", "gruvbox_dark", "dracula", "onyx"];

/// Terminal color theme: background, foreground, cursor, and the 16-color ANSI palette.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub bg: [u8; 4],           // background RGBA (alpha < 255 => transparent)
    pub fg: [u8; 3],           // default foreground
    pub cursor: [u8; 3],       // cursor block color
    pub palette: [[u8; 3]; 16], // standard ANSI 0..=15
}

impl Theme {
    /// Resolve a theme by name string. Unknown names fall back to catppuccin_mocha.
    pub fn by_name(name: &str) -> Theme {
        match name {
            "catppuccin_mocha" => catppuccin_mocha(),
            "tokyo_night" => tokyo_night(),
            "gruvbox_dark" => gruvbox_dark(),
            "dracula" => dracula(),
            "onyx" => onyx(),
            _ => catppuccin_mocha(),
        }
    }
}

/// Catppuccin Mocha — the soothing pastel dark theme (catppuccin.com). Default.
pub fn catppuccin_mocha() -> Theme {
    Theme {
        name: "catppuccin_mocha",
        bg: [30, 30, 46, 255],   // base   #1e1e2e
        fg: [205, 214, 244],     // text   #cdd6f4
        cursor: [245, 224, 220], // rosewater #f5e0dc
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

/// Tokyo Night — the popular dark blue scheme (enkia/tokyonight).
pub fn tokyo_night() -> Theme {
    Theme {
        name: "tokyo_night",
        bg: [26, 27, 38, 255],   // #1a1b26
        fg: [192, 202, 245],     // #c0caf5
        cursor: [192, 202, 245], // #c0caf5
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
            [247, 118, 142], // 9  #f7768e
            [158, 206, 106], // 10 #9ece6a
            [224, 175, 104], // 11 #e0af68
            [122, 162, 247], // 12 #7aa2f7
            [187, 154, 247], // 13 #bb9af7
            [125, 207, 255], // 14 #7dcfff
            [192, 202, 245], // 15 #c0caf5
        ],
    }
}

/// Gruvbox Dark — Pavel Pertsev's retro-groove color scheme.
pub fn gruvbox_dark() -> Theme {
    Theme {
        name: "gruvbox_dark",
        bg: [40, 40, 40, 255],
        fg: [235, 219, 178],
        cursor: [251, 241, 199],
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
        name: "dracula",
        bg: [40, 42, 54, 255],   // #282a36
        fg: [248, 248, 242],     // #f8f8f2
        cursor: [248, 248, 242], // #f8f8f2
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
        name: "onyx",
        bg: [22, 22, 26, 255],   // #16161a
        fg: [200, 200, 205],     // #c8c8cd
        cursor: [97, 175, 239],  // #61afef
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

