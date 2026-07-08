/// Ordered list of built-in theme preset names. Indices are stable so they
/// can be used as `theme_idx` in the app state (Ctrl+Shift+T cycles through).
///
/// These are widely-loved community terminal palettes used verbatim (exact
/// hex), not hand-tuned approximations.
pub const PRESETS: [&str; 22] = [
    "catppuccin_mocha", "tokyo_night", "gruvbox_dark", "dracula", "onyx", "nord",
    "solarized_dark", "solarized_light", "one_dark", "monokai", "monokai_pro",
    "everforest_dark", "rose_pine", "kanagawa", "material_dark", "ayu_dark", "ayu_mirage",
    "tomorrow_night", "oceanic_next", "github_dark", "palenight", "catppuccin_macchiato",
];

/// Terminal color theme: background, foreground, cursor, and the 16-color ANSI palette.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,         // stable id used in config + PRESETS (snake_case)
    pub display_name: &'static str, // human-facing label shown in the Settings theme list
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
            _ => catppuccin_mocha(),
        }
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
        name: "catppuccin_mocha",
        display_name: "Catppuccin Mocha",
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
        display_name: "Tokyo Night",
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
        display_name: "Gruvbox Dark",
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
        display_name: "Dracula",
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
        display_name: "Onyx",
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


/// Nord — nordtheme.com (exact hex).
pub fn nord() -> Theme {
    Theme {
        name: "nord",
        display_name: "Nord",
        bg: [46, 52, 64, 255],   // #2e3440
        fg: [216, 222, 233],     // #d8dee9
        cursor: [236, 239, 244], // #eceff4
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
        name: "solarized_dark",
        display_name: "Solarized Dark",
        bg: [0, 43, 54, 255],   // #002b36
        fg: [131, 148, 150],     // #839496
        cursor: [131, 148, 150], // #839496
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
        name: "solarized_light",
        display_name: "Solarized Light",
        bg: [253, 246, 227, 255],   // #fdf6e3
        fg: [101, 123, 131],     // #657b83
        cursor: [101, 123, 131], // #657b83
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
        name: "one_dark",
        display_name: "One Dark",
        bg: [33, 37, 43, 255],   // #21252b
        fg: [171, 178, 191],     // #abb2bf
        cursor: [171, 178, 191], // #abb2bf
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
        name: "monokai",
        display_name: "Monokai",
        bg: [39, 40, 34, 255],   // #272822
        fg: [253, 255, 241],     // #fdfff1
        cursor: [192, 193, 181], // #c0c1b5
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
        name: "monokai_pro",
        display_name: "Monokai Pro",
        bg: [45, 42, 46, 255],   // #2d2a2e
        fg: [252, 252, 250],     // #fcfcfa
        cursor: [193, 192, 192], // #c1c0c0
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

/// Everforest Dark — sainnhe/everforest (exact hex).
pub fn everforest_dark() -> Theme {
    Theme {
        name: "everforest_dark",
        display_name: "Everforest Dark",
        bg: [45, 53, 59, 255],   // #2d353b
        fg: [211, 198, 170],     // #d3c6aa
        cursor: [230, 152, 117], // #e69875
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
            [248, 85, 82], // 9  br red     #f85552
            [141, 161, 1], // 10 br green   #8da101
            [223, 160, 0], // 11 br yellow  #dfa000
            [58, 148, 197], // 12 br blue    #3a94c5
            [223, 105, 186], // 13 br magenta #df69ba
            [53, 167, 124], // 14 br cyan    #35a77c
            [255, 251, 239], // 15 br white   #fffbef
        ],
    }
}

/// Rose Pine — rosepinetheme.com (exact hex).
pub fn rose_pine() -> Theme {
    Theme {
        name: "rose_pine",
        display_name: "Rose Pine",
        bg: [25, 23, 36, 255],   // #191724
        fg: [224, 222, 244],     // #e0def4
        cursor: [224, 222, 244], // #e0def4
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
        name: "kanagawa",
        display_name: "Kanagawa",
        bg: [31, 31, 40, 255],   // #1f1f28
        fg: [220, 215, 186],     // #dcd7ba
        cursor: [220, 215, 186], // #dcd7ba
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
        name: "material_dark",
        display_name: "Material",
        bg: [35, 35, 34, 255],   // #232322
        fg: [229, 229, 229],     // #e5e5e5
        cursor: [22, 175, 202], // #16afca
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
        name: "ayu_dark",
        display_name: "Ayu Dark",
        bg: [11, 14, 20, 255],   // #0b0e14
        fg: [191, 189, 182],     // #bfbdb6
        cursor: [230, 180, 80], // #e6b450
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
        name: "ayu_mirage",
        display_name: "Ayu Mirage",
        bg: [31, 36, 48, 255],   // #1f2430
        fg: [204, 202, 194],     // #cccac2
        cursor: [255, 204, 102], // #ffcc66
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
        name: "tomorrow_night",
        display_name: "Tomorrow Night",
        bg: [29, 31, 33, 255],   // #1d1f21
        fg: [197, 200, 198],     // #c5c8c6
        cursor: [197, 200, 198], // #c5c8c6
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
        name: "oceanic_next",
        display_name: "Oceanic Next",
        bg: [22, 44, 53, 255],   // #162c35
        fg: [192, 197, 206],     // #c0c5ce
        cursor: [192, 197, 206], // #c0c5ce
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
        name: "github_dark",
        display_name: "GitHub Dark",
        bg: [13, 17, 23, 255],   // #0d1117
        fg: [201, 209, 217],     // #c9d1d9
        cursor: [88, 166, 255], // #58a6ff
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
        name: "palenight",
        display_name: "Palenight",
        bg: [41, 45, 62, 255],   // #292d3e
        fg: [191, 199, 213],     // #bfc7d5
        cursor: [126, 87, 194], // #7e57c2
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
        name: "catppuccin_macchiato",
        display_name: "Catppuccin Macchiato",
        bg: [36, 39, 58, 255],   // #24273a
        fg: [202, 211, 245],     // #cad3f5
        cursor: [244, 219, 214], // #f4dbd6
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
