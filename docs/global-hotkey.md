# Global summon hotkey (Yakuake-style)

Jetty supports a global hotkey — **F9** by default — to show/hide the window from
anywhere on the desktop, no need to click the taskbar or alt-tab. Pick another key
with the `summon_hotkey` config key (`"F12"`, `"Ctrl+Shift+F12"`, …; read at
startup). An invalid value falls back to F9, and a key that can't be grabbed is
reported in the window — not only on stderr.

## X11

On X11, Jetty grabs the summon key on the root window at startup — its own
passive key grab (`jetty_platform::hotkey`, over x11rb) on a thread that sleeps
in the kernel until the key is pressed: no polling, no idle wakeups. Every
NumLock / CapsLock combination is grabbed too, a held key fires once, and a key
another program already grabs is reported in the window. No configuration is
needed.

Key names are `global-hotkey`'s (`F12`, `KeyT`, `Digit1`, `Backquote`, `Space`
…). A letter is the key your layout labels with it; the digit row and the
symbol keys are positions — `Ctrl+Backquote` is the key below Esc whatever it
types, as on macOS.

F9 does what you'd expect from the window's state:

- **hidden** → summons it;
- **shown and in front** (any JeTTY window — the terminal, Settings or a
  detached tab — has focus) → hides it;
- **shown but behind other windows** (e.g. with `focus_autohide = false`, after
  you clicked elsewhere) → brings it to the front instead of hiding, so one
  press brings it back; a minimized window is restored. On X11 JeTTY asks the
  window manager the way a taskbar click does (the standard EWMH activation
  request with the "user action" source), so focus-stealing prevention — KWin's,
  for one — lets it through. If the window manager still refuses (Wayland
  without an activation token), the next press within 1.5 s hides it.

On summon the window is placed according to `window_mode` — centred the first
time, then back where you left it as long as that spot is on a connected monitor
(Center), re-docked to the top strip (Dropdown), or expanded to cover the whole
monitor (Fullscreen) — then takes keyboard focus and replays the reveal effect.
(Jetty launches visible — unless started with `--background` — so the first F9
press after startup hides it.)

In Fullscreen mode the OS fullscreen state is dropped on every hide and
re-applied on every summon: it is never held while the window is hidden. That is
what keeps the summon reliable (a fullscreen request that matches the state the
window already claims is silently dropped by the X11 backend) and what stops a
hidden window from keeping the desktop's panels out of the way.

## Wayland

Global key grabs are not available to regular apps on Wayland (by design). Bind
**`jetty --toggle`** to a key in your compositor: the first press launches Jetty,
and each press after toggles the running instance over a Unix socket
(`$XDG_RUNTIME_DIR/jetty.sock`; without `XDG_RUNTIME_DIR` — always on macOS — a
private 0700 `jetty/` directory in your cache dir: `~/.cache/jetty/jetty.sock`,
`~/Library/Caches/jetty/jetty.sock` on macOS. Never a world-writable `/tmp` path),
so it shows or hides instantly. Use `jetty --show` / `jetty --hide` instead for a
dedicated summon / dismiss key. The control invocation forwards the command and
exits immediately — no window, no GUI work. (`jetty --background`, used by
"Launch at login", starts Jetty hidden and does nothing if it already runs.)

This is a generic, compositor-independent path — no portal, no
desktop-environment-specific code, works on every compositor. Once hidden, the
window comes back only through that binding (or `jetty --show`, or launching
JeTTY again from the app menu, which forwards a toggle): a Wayland app cannot
listen for a key it doesn't have focus for.

JeTTY's X11 grab still reaches XWayland in a Wayland session, so `summon_hotkey`
fires only while an X11 (XWayland) window has focus — which looks like a flaky
hotkey. Bind `jetty --toggle` to the same key in the compositor instead; JeTTY
doesn't report the grab as a problem there.

### KDE Plasma (Wayland)

System Settings → Shortcuts → Custom Shortcuts → New → Global Shortcut →
Command: `jetty --toggle`, Trigger: F9

### GNOME (Wayland)

Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts →
Add shortcut, Command: `jetty --toggle`, Shortcut: F9

### Sway / i3 (Wayland/X11)

```
bindsym F9 exec jetty --toggle
```

### Hyprland

```
bind = , F9, exec, jetty --toggle
```

## macOS

The default global hotkey is plain **F9** (no `fn` modifier is added by Jetty).
On a Mac keyboard where the function-row keys default to media actions, press
`fn`+`F9` so the OS delivers F9, or enable "Use F1, F2, etc. keys as standard
function keys" in System Settings → Keyboard — or set `summon_hotkey` to a chord
that needs neither (e.g. `"Ctrl+Shift+Space"`).

The hotkey is a system-wide hotkey registration (Carbon's `RegisterEventHotKey`,
through the `global-hotkey` crate), which needs **no** Accessibility or Input
Monitoring permission — granting one changes nothing. macOS refuses a chord
another app has registered, and macOS 15 one whose only modifier is Option (or
Option+Shift); Jetty then says so in the window. Choose a chord with Ctrl or Cmd
(`"Ctrl+Shift+Space"`, `"Cmd+F9"`), or bind `jetty --toggle` to a shortcut via a
launcher, which works as on Wayland. Only a media key (play/pause, volume) is
watched through an event tap, and that one needs Jetty allowed in System
Settings → Privacy & Security → Accessibility.

The hotkey manager is created and kept on the main thread, as the
`global-hotkey` crate requires on macOS (earlier versions registered it on a
background thread, where it could silently never fire).

Hiding the terminal (F9, `jetty --hide`) while no other JeTTY window is open
hides the whole application, as Cmd+H does, so the keyboard goes back to the app
you came from instead of staying with a JeTTY that has no window. A hidden start
(`jetty --background`, the login item) does not take the keyboard either. A click
on JeTTY's Dock icon, `open -a JeTTY` or a launch from Spotlight or Finder while
it runs brings the terminal back, like `jetty --show`.

## Notes

- The PTY (shell) keeps running while the window is hidden — nothing is killed.
- On X11, both mechanisms are active: the built-in grab AND the IPC socket.
  Either works; the hotkey grab is faster (no process fork).
- The socket is cleaned up on normal exit; stale sockets from crashes are
  automatically removed at next startup.
- One JeTTY per display: `jetty` run on another display than the running
  instance's (an `ssh -X` session, a second X session) starts or summons that
  display's own JeTTY instead of toggling this one. A launch with no display at
  all (a console) still controls the running one.
- A newer JeTTY launched while an older one runs (an updated AppImage next to
  the old file, an upgraded package) toggles the running one, which then says
  how to switch; an AppImage also moves Launch at login to itself.
- The built-in grab is JeTTY's own on X11 (`jetty_platform::hotkey`) and the
  `global-hotkey` crate's Carbon hotkey on macOS; `summon_hotkey` uses that
  crate's syntax on both, with the modifiers `Ctrl`, `Shift`, `Alt` / `Option`
  and `Super` / `Cmd` (unlike `[keys]`, no `Opt`, `Win` or `Meta`). Wayland has
  no global grab for apps, which is why the compositor binding + IPC path is
  required there. (Jetty runs on Linux and macOS; it does not build on Windows.)
