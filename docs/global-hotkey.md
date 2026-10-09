# Global summon hotkey (Yakuake-style)

Jetty supports a global hotkey — **F9** by default — to show/hide the window from
anywhere on the desktop, no need to click the taskbar or alt-tab. Pick another key
with the `summon_hotkey` config key (`"F12"`, `"Ctrl+Shift+F12"`, …; read at
startup). An invalid value falls back to F9, and a key that can't be grabbed is
reported in the window — not only on stderr.

## X11

On X11, Jetty automatically registers a system-wide grab of the summon key at
startup using the `global-hotkey` crate. No configuration is needed.

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

On summon the window is placed according to `window_mode` — re-centred on the
current monitor (Center), re-docked to the top strip (Dropdown), or expanded to
cover the whole monitor (Fullscreen) — then takes keyboard focus and replays the
reveal effect. (Jetty launches visible — unless started with `--background` —
so the first F9 press after startup hides it.)

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
desktop-environment-specific code, works on every compositor.

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

macOS requires Jetty to be granted Accessibility (and on some versions Input
Monitoring) permission before a system-wide key tap is delivered: System
Settings → Privacy & Security → Accessibility → enable Jetty. Without this the
F9 grab is silently inactive; the IPC toggle still works as a fallback
(bind `jetty --toggle` to a shortcut via a launcher).

The hotkey manager is created and kept on the main thread, as the
`global-hotkey` crate requires on macOS (earlier versions registered it on a
background thread, where it could silently never fire). If the grab fails, Jetty
says so in the window; binding `jetty --toggle` to a shortcut via a launcher works
as on Wayland.

Hiding the terminal (F9, `jetty --hide`) while no other JeTTY window is open
hides the whole application, as Cmd+H does, so the keyboard goes back to the app
you came from instead of staying with a JeTTY that has no window. A hidden start
(`jetty --background`, the login item) does not take the keyboard either.

## Notes

- The PTY (shell) keeps running while the window is hidden — nothing is killed.
- On X11, both mechanisms are active: the built-in grab AND the IPC socket.
  Either works; the hotkey grab is faster (no process fork).
- The socket is cleaned up on normal exit; stale sockets from crashes are
  automatically removed at next startup.
- The built-in global grab uses the `global-hotkey` crate, which supports a
  system-wide grab on X11, macOS, and Windows. On Wayland the crate cannot
  register a grab, which is why the compositor-binding + IPC fallback is required
  there. (Jetty targets Linux and macOS; Windows is untested.)
