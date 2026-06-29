# Global F9 Hotkey (Yakuake-style summon)

Jetty supports a global F9 hotkey to show/hide the window from anywhere on
the desktop — no need to click the taskbar or alt-tab.

## X11

On X11, Jetty automatically registers a system-wide F9 key grab at startup
using the `global-hotkey` crate. No configuration is needed.

F9 is a toggle: press it to hide the window, press it again to summon it. On
summon the window is re-centred on the current monitor (or re-docked to the top
in Dropdown mode), takes keyboard focus, and replays the reveal effect.
(Jetty launches visible, so the first F9 press after startup hides it.)

## Wayland

Global key grabs are not available to regular apps on Wayland (by design). Bind
**`jetty --toggle`** to a key in your compositor: the first press launches Jetty,
and each press after toggles the running instance over a Unix socket
(`$XDG_RUNTIME_DIR/jetty.sock`, falling back to `/tmp/jetty.sock`), so it shows or
hides instantly. Use `jetty --show` / `jetty --hide` instead for a dedicated
summon / dismiss key. The control invocation forwards the command and exits
immediately — no window, no GUI work.

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

The global hotkey is plain **F9** (no `fn` modifier is added by Jetty;
registration is `HotKey::new(None, Code::F9)`). On a Mac keyboard where the
function-row keys default to media actions, press `fn`+`F9` so the OS delivers
F9, or enable "Use F1, F2, etc. keys as standard function keys" in
System Settings → Keyboard.

macOS requires Jetty to be granted Accessibility (and on some versions Input
Monitoring) permission before a system-wide key tap is delivered: System
Settings → Privacy & Security → Accessibility → enable Jetty. Without this the
F9 grab is silently inactive; the IPC toggle still works as a fallback
(bind `jetty --toggle` to a shortcut via a launcher).

Known limitation: macOS global-hotkey support is best-effort (the manager is
registered off the main thread, which upstream documents as fragile on macOS).
If F9 does not toggle, bind `jetty --toggle` to a shortcut via a launcher,
as on Wayland.

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
