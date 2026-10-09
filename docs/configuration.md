# Configuration reference

Every setting JeTTY reads lives in one TOML file. The Settings window (`Ctrl+,`)
writes it for you; this page lists every key it can hold, its default, and what
it does.

| OS | Config file | User themes |
|---|---|---|
| Linux | `~/.config/jetty/config.toml` (`$XDG_CONFIG_HOME/jetty/…`) | `~/.config/jetty/themes/*.toml` |
| macOS | `~/Library/Application Support/jetty/config.toml` | `~/Library/Application Support/jetty/themes/*.toml` |

`JETTY_CONFIG_DIR=/some/dir` uses `/some/dir/config.toml` and `/some/dir/themes/`
instead; `jetty --help` prints the path in use. Every key is optional: a key you
leave out has its default, so a config file only needs the lines you want to
change.

## How the file is read

- **Live.** Saving the file — or a theme file — applies it within a moment, no
  restart. The exceptions are marked below: `summon_hotkey` (the global hotkey is
  registered once; a reload says so), turning `hot_reload` back on, and
  `show_welcome` (it is about the next launch). `shell` applies to tabs opened
  after the change.
- **Forgiving.** A problem never resets your other settings. A value of the wrong
  type (`opacity = "0.9"`), a word a key does not know (`window_mode = "dropdwn"`)
  or an unknown key (`fontsize`) is reported — with the closest valid spelling,
  "did you mean `font_size`?" — and only that key falls back: to its default at
  startup, to the value in use on a reload. Words are read in any letter case
  (`"Dropdown"`, `"Bottom"`). A number outside its range is clamped and reported
  (`font_size = 100` is out of range (6–48) — using 48). A file that is not valid
  TOML at all is reported with its line and column: on startup JeTTY runs on
  defaults and keeps a copy as `config.toml.bad-<time>`; on a reload it keeps
  the settings in use. Either way it never saves over a broken file.
- **Where problems show.** At startup in the first tab (a desktop launch has no
  terminal for them), on a reload in a notice at the bottom of the window, and
  always on stderr. `jetty --check-config` prints every problem of the config
  file and the theme files — the notice has room for one — and exits with 1 if
  there are any.
- **Your formatting stays.** Settings changes rewrite only the keys that changed:
  comments, order and keys JeTTY does not know survive. A symlinked file
  (dotfiles) is written through the link; a read-only one is never replaced.

Names of fonts and themes are matched in any letter case, and a theme by its id
(`solarized_light`) or the name Settings shows (`"Solarized Light"`).

<!-- config-keys:start — every key and its default; checked against the code by crates/jetty-app/src/config/check.rs -->

## Theme and colors

| Key | Default | What it does |
|---|---|---|
| `theme` | `"catppuccin_mocha"` | The color theme: one of the 46 built-ins or a [user theme](#user-themes). Settings and the command palette show them all. |
| `follow_system_theme` | `false` | Follow the desktop's light/dark preference (the freedesktop settings portal on Linux and BSD, the system appearance on macOS): `light_theme` while it prefers light or states no preference, `theme` while it prefers dark. Without a portal (a bare window manager) `theme` stays. |
| `light_theme` | `"catppuccin_latte"` | The theme shown while `follow_system_theme` is on and the system is light. While it is on screen, a theme picked in Settings or the palette is saved here. `""` = no light variant. |
| `minimum_contrast` | `1.0` | Minimum contrast ratio between text and its background, `1`–`21`. `1` is off; `4.5` is WCAG AA, `3` large text. Text below it is pushed toward white or black, keeping its hue; powerline, block and sextant glyphs never change. |
| `opacity` | `1.0` | Background opacity, `0.1`–`1` (needs a compositor). `Ctrl+Alt+=` / `Ctrl+Alt+-` step it. |

## Fonts and text

| Key | Default | What it does |
|---|---|---|
| `font_family` | `"MesloLGS NF"` | The terminal font: any installed family (Settings lists the monospace ones). One that is not installed shows "MesloLGS NF" — or the first monospace font, when that is missing too — and says so; the name you chose is kept for when it is installed. The default itself falls back without a notice. Bold and italic always come from the family itself (a variable font's own bold weight; regular weight in a family with no bold), so they never shift a column. |
| `font_size` | `16.0` | The terminal font size in points, `6`–`48`. `Ctrl+=` / `Ctrl+-` / `Ctrl+0` change it. |
| `ui_font_family` | `""` | The font of the window chrome — tab titles, status bar, menus, Settings, dialogs. `""` is the system's sans-serif. |
| `ui_font_size` | `16.0` | The chrome font size in points, `10`–`28`. |
| `line_height` | `1.3` | Line height as a multiple of the font size, `1.0`–`2.0`. Glyphs are centered in the taller row; backgrounds, selection and the cursor fill it. |
| `builtin_glyphs` | `true` | Draw box drawing, block elements, Powerline separators, braille and sextants as cell-exact built-in glyphs, so borders and prompts join without seams at any size. `false` takes them from the font. |
| `color_emoji` | `true` | Draw emoji in color from the installed emoji font, two cells wide. Text-style symbols (✔ ❤) stay text. |
| `bold_is_bright` | `false` | Bold text in one of the 8 normal ANSI colors uses its bright twin (the classic xterm look). |

## Window

| Key | Default | What it does |
|---|---|---|
| `window_mode` | `"center"` | How the summon hotkey shows the window: `"center"` (centered, or where you left it), `"dropdown"` (a full-width strip that slides down from the top) or `"fullscreen"` (the whole monitor). `F11` toggles fullscreen for the moment without changing this. |
| `dropdown_height_pct` | `0.5` | The dropdown's height as a fraction of the monitor, `0.25`–`1`. |
| `dropdown_width_pct` | `1.0` | The dropdown's width as a fraction of the monitor, `0.2`–`1` (no Settings control). |
| `corner_radius` | `10.0` | Window corner radius in logical pixels, `0`–`24`. |
| `padding_x` | `8.0` | Space between the window's left and right edges and the text, in logical pixels, `0`–`64`. The scrollbar lives in the right padding. |
| `padding_y` | `4.0` | Space above and below the text, in logical pixels, `0`–`64`. |
| `scrollbar` | `"always"` | When the scrollbar shows: `"always"`, `"auto"` (while scrolled back, dragged or hovered) or `"never"` (no scrollbar and no gutter — the text gets the full width). |
| `window_border` | `"none"` | A thin ring around the window: `"none"`, `"focus"` (while it has keyboard focus) or `"always"` (muted while unfocused), in the accent or the active tab's color. |
| `summon_effect` | `"phosphor"` | How the window appears when summoned: `"none"`, `"bayer"`, `"phosphor"`, `"liquid"`, `"focus"`, `"pop"`, `"glide"` or `"fade"`. |
| `summon_hotkey` | `"F9"` | The global hotkey that shows and hides JeTTY, e.g. `"F12"` or `"Ctrl+Shift+F12"`. Applies after a restart. On Wayland, bind `jetty --toggle` in your compositor instead. |
| `focus_autohide` | `true` | Hide the window when it loses focus (drop-down terminal style). On X11 another program's keyboard grab — a held global shortcut, a window manager's move or resize, Alt+Tab while you choose — is not a focus loss: the window stays unless the focus ends up elsewhere. |
| `launch_at_login` | `false` | Start JeTTY hidden at login (an XDG autostart entry on Linux, a LaunchAgent on macOS); press the summon hotkey and it is there. Starting JeTTY never removes the entry: set `false` (or use the Settings toggle) while JeTTY runs. Ignored with `JETTY_CONFIG_DIR`. |

## Tabs and chrome

| Key | Default | What it does |
|---|---|---|
| `tab_bar_position` | `"top"` | `"top"` or `"bottom"`. |
| `tab_style` | `"pill"` | The tab look: `"pill"`, `"underline"`, `"slant"`, `"powerline"` or `"compact"`. |
| `tab_close_button` | `"always"` | Which tabs show their ×: `"always"`, `"hover"` (the tab under the pointer) or `"active"` (the active tab and the hovered one). |
| `tab_bar_opacity` | `false` | `true` lets the tab bar follow `opacity` like the terminal area. |
| `tab_title` | `"osc"` | Tab titles: `"osc"` (the program's title, else "Tab N") or `"auto"` (the program's title, else the running command or the shell's directory — needs shell integration). A manual rename always wins. |
| `progress_bar` | `true` | Show the progress programs report (OSC 9;4: cargo, winget, Claude Code …) in the tab and along the bar's edge. |
| `show_perf_hud` | `true` | The live performance readout in the status bar (frame time, fps, CPU, VT throughput). It never causes a redraw of its own. |
| `show_welcome` | `true` | The welcome splash in the first tab at launch (gone at the first key). Applies at the next launch. |

## Motion and alerts

| Key | Default | What it does |
|---|---|---|
| `reduce_motion` | `"off"` | Calm the motion down: `"off"`, `"on"` or `"system"` (follow the desktop's reduced-motion setting). While active the summon reveal is a short fade, the dropdown does not slide, CRT roll, flicker and jitter stop, the cursor trail is off and the visual bell is the rim. |
| `visual_bell` | `"off"` | The bell (BEL in the active tab) as a picture: `"off"`, `"flash"` (a 150 ms flash of the window) or `"rim"` (a glow along its edge). At most three a second; an unfocused window asks for attention instead. |
| `command_pulse` | `"off"` | A pulse along the window edge when a command finishes (needs shell integration): `"off"`, `"failures"` (red, on a failed command) or `"all"` (also the accent when a long command succeeds). |

## Shell and terminal

| Key | Default | What it does |
|---|---|---|
| `shell` | `""` | The shell to run. `""` = `$SHELL`, then your login shell, then `/bin/bash`; or an absolute path such as `"/usr/bin/fish"`. Applies to new tabs. |
| `scrollback_lines` | `10000` | History kept per tab, in lines, `100`–`100000`. |
| `kitty_keyboard` | `true` | Offer the kitty keyboard protocol to programs that ask for it (unambiguous keys: Ctrl+I ≠ Tab, key releases). `false` turns it off in every tab. |
| `osc52_allow_paste` | `false` | Let programs — a remote host over SSH too — READ your clipboard with OSC 52. Copying through OSC 52 always works; reading is off because it can leak whatever is on the clipboard. |
| `copy_on_select` | `"primary"` | Where a finished mouse selection is copied: `"primary"` (the X11/Wayland selection a middle click pastes), `"clipboard"`, `"both"` or `"off"`. Without a primary selection (macOS) `"primary"` means the clipboard. |
| `run_selection` | `true` | "Run the selection in a new tab" (menu, `Ctrl+Shift+Enter`, palette, copy mode). `false` turns every way of doing it off. |
| `macos_option_as_alt` | `"none"` | macOS: which Option key acts as Alt/Meta instead of typing characters — `"none"`, `"left"`, `"right"` or `"both"`. Ignored elsewhere. |

## Notifications

Run & Notify needs shell integration (`jetty --help` prints the line for your
shell's rc file); without it nothing is ever notified.

| Key | Default | What it does |
|---|---|---|
| `notify_on_command_finish` | `true` | A desktop notification (and taskbar urgency) when a command finishes while JeTTY is hidden or unfocused. |
| `notify_min_seconds` | `10` | Only commands that ran at least this long notify on success, `1`–`86400` seconds (a failure may notify sooner). |
| `notify_only_on_failure` | `false` | Only notify about failed commands. |
| `auto_summon_on_finish` | `false` | Bring JeTTY back (with the tab that finished) when a command finishes — only while it is hidden, never mid-typing. Follows `notify_only_on_failure`. |

## The config file itself

| Key | Default | What it does |
|---|---|---|
| `hot_reload` | `true` | Watch the config folder and apply changes live. `false` stops watching; turning it back on needs a restart. |

## `[cursor]` — the cursor

| Key | Default | What it does |
|---|---|---|
| `shape` | `"block"` | `"block"`, `"beam"`, `"underline"`, `"double_underline"` or `"thick_underline"`. The shape a fresh screen shows and programs reset to; a program's own choice still wins until it resets. |
| `thickness` | `0.12` | Beam and underline thickness as a fraction of the cell, `0.04`–`0.5`. |
| `unfocused` | `"hollow"` | The cursor of an unfocused window: `"hollow"` (an outline), `"unchanged"` or `"none"`. |
| `color` | `"theme"` | `"theme"` (the theme's cursor color), `"cell"` (the cell under it, reversed) or `"auto"` (the theme color, reversed where it would be hard to see). |
| `guide` | `"off"` | A faint band across the cursor's row: `"off"`, `"shell"` (not in full-screen programs) or `"always"`. |
| `trail` | `false` | When the cursor jumps, it leaves a short smear that catches up with it. |
| `trail_ms` | `200` | How long the trail takes to catch up, `60`–`1000` ms. |
| `trail_threshold` | `2` | A jump must cover more than this many cells (rows + columns) to leave a trail, `1`–`40` — typing never does. |

## `[backdrop]` — the background

Everything here is off until `mode` says otherwise; `"none"` builds and draws nothing.

| Key | Default | What it does |
|---|---|---|
| `mode` | `"none"` | `"none"`, `"theme"` (a look made for the current theme), `"gradient"`, `"image"` or `"pattern"`. |
| `colors` | `[]` | Gradient colors as `"#rrggbb"` or `"#rgb"`, up to 4. Empty = colors from the theme. |
| `angle` | `135.0` | The gradient's direction in degrees, as in CSS: `0` toward the top, `90` toward the right. |
| `shape` | `"linear"` | `"linear"` or `"radial"`. |
| `strength` | `0.5` | How strongly the backdrop shows over the theme background, `0`–`1`. |
| `vignette` | `0.0` | Darkening toward the corners, `0`–`1`. |
| `grain` | `0.0` | Film grain, `0`–`1`. |
| `image` | `""` | The picture for `mode = "image"`: an absolute path, `~/…`, or a name in `<config dir>/backgrounds/`. PNG or JPEG, up to 8192×8192. |
| `fit` | `"cover"` | `"cover"`, `"contain"`, `"stretch"`, `"center"` or `"tile"`. |
| `dim` | `0.7` | How far the image is blended toward the theme background, `0`–`1` — raised automatically when the text would read below 4.5:1. |
| `blur` | `0.0` | Frosted-glass blur of the image, `0`–`1`. |
| `pattern` | `"stars"` | For `mode = "pattern"`: `"stars"`, `"aurora"`, `"grid"` or `"synthwave"`. |
| `animate` | `false` | Slow motion for gradients and patterns (at most 30 fps, paused while hidden, never on a software renderer). |
| `parallax` | `false` | Shift the backdrop with the scrollback position. |

## `[effects]` — post-processing

All of it is off by default except the caret flash, so the default look and the
idle CPU are untouched. Colors are `[red, green, blue]` from `0` to `1`.

| Key | Default | What it does |
|---|---|---|
| `crt_enabled` | `false` | The CRT pass; the `crt_*` keys below tune it. |
| `crt_curvature` | `0.0` | Screen curvature, `0`–`1`. |
| `crt_scanline` | `0.5` | Scanline strength, `0`–`1`. |
| `crt_mask` | `0.3` | Shadow-mask strength, `0`–`1`. |
| `crt_bloom` | `0.4` | Glow around bright text, `0`–`1`. |
| `crt_bloom_radius` | `0.0` | How far the glow spreads, `0`–`1`. |
| `crt_chromatic` | `0.2` | Color fringing, `0`–`1`. |
| `crt_vignette` | `0.4` | Darkened corners, `0`–`1`. |
| `crt_scanline_tint` | `[1.0, 1.0, 1.0]` | The scanlines' color. |
| `crt_phosphor` | `"off"` | A monochrome display: `"off"` (full color), `"amber"`, `"green"`, `"white"`, `"blue"`, `"paper"` (dark ink on light paper) or `"custom"` (`crt_phosphor_color` on black). |
| `crt_phosphor_color` | `[1.0, 0.69, 0.0]` | The `"custom"` phosphor color. |
| `crt_phosphor_hue` | `0.0` | How much of the text's own color the phosphor keeps, `0` (pure mono) – `1`. |
| `crt_grain` | `0.0` | Film grain, `0`–`1`. |
| `crt_grain_animate` | `false` | Re-roll the grain (an animation: it repaints continuously, at most 30 fps). |
| `crt_dither` | `false` | 1-bit ordered dither — the e-ink look. |
| `crt_animate_roll` | `false` | A slowly rolling scan band (an animation). |
| `crt_flicker` | `false` | Brightness flicker (an animation). |
| `crt_jitter` | `false` | Horizontal jitter (an animation). |
| `animate_unfocused` | `false` | Keep the CRT animations running while the window is unfocused (otherwise it stands still and costs nothing). |
| `caret_flash_enabled` | `true` | Typing flashes the cursor. |
| `caret_flash_ms` | `130.0` | How long the flash lasts, `60`–`400` ms. |
| `caret_flash_color` | `[1.0, 1.0, 1.0]` | The flash's color. |
| `caret_glow_enabled` | `false` | A soft glow and ripple around the cursor while typing. |
| `glitch_on_error` | `false` | A short color-split glitch when a command fails (shell integration). |
| `glitch_on_bell` | `false` | The same on the terminal bell (at most one a second). |

<!-- config-keys:end -->

## `[keys]` — keybindings

Every shortcut is a default you can remap here, by the names in the
[keybindings table](../README.md#️-keybindings):

```toml
[keys]
new_tab = "Ctrl+Shift+N"
paste = ["Ctrl+Shift+V", "Shift+Insert"]   # several chords for one action
toggle_fullscreen = ""                       # "" unbinds: F11 goes to the program
context_menu = ["Menu", "Shift+F10"]       # Shift+F10 too, for keyboards without a Menu key
```

A chord is modifiers and a key joined by `+` (`Ctrl`, `Shift`, `Alt`, `Super`/`Cmd`).
Only F-keys, `PageUp` / `PageDown` and `Menu` (the context-menu key) may be bound
without a modifier.
A chord you bind is taken from the action that had it by default (the help
overlay shows the result). An unknown action name or a chord that cannot be
parsed is reported and ignored; the rest still apply. Terminal control bytes —
`Ctrl` with a letter, `Space`, `[`, `\`, `]` or `/` (`Ctrl+C`, `Ctrl+T` …) —
cannot be taken over: add `Shift` or `Alt`. An action none of whose chords can
be used keeps its default.
The command palette's **Reset keybindings** clears the table (after saving a
`config.toml.bak-<time>` copy).

## User themes

A theme is a TOML file in the `themes/` folder next to `config.toml`; it appears
in Settings and the command palette, and `theme = "<name>"` picks it. A theme
whose `name` matches a built-in (`dracula`) replaces that built-in. Editing a
theme that is on screen applies at once; a save that does not load (a typo
mid-edit) keeps the last version that did, and says where the problem is.

```toml
name         = "my_theme"   # optional: the file name without .toml
display_name = "My Theme"   # optional: from the name
background   = "#1e1e2e"    # required (or `bg`)
foreground   = "#cdd6f4"    # required (or `fg`)
cursor       = "#f5e0dc"    # required
cursor_text  = "#1e1e2e"    # optional: the glyph under a block cursor
selection_background = "#45475a"  # optional (or `selection_bg`)
selection_foreground = "#cdd6f4"  # optional (or `selection_fg`)
accent       = "#89b4fa"    # optional: menus, focus marks, the welcome logo
# The 16 ANSI colors: a list of exactly 16 …
palette = ["#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
           "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8"]
# … or two tables, [normal] and [bright], each with black, red, green, yellow,
# blue, magenta, cyan and white.
```

Colors are `#rrggbb` or `#rgb`. `opacity` is a global setting, so an `opacity`
key in a theme file is ignored.
