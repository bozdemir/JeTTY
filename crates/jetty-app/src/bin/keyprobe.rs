// Diagnostic: a winit-only window (NO GPU) that logs what winit reports for
// each key / IME event AND what JeTTY would send for it — the same
// `input::decide_key_event` path, with the default keymap. IME is allowed as in
// JeTTY, so dead keys and Compose behave exactly as they do there. Needs no
// presentation, so it runs on any X server (Xvfb / Xephyr) or Wayland.
//
// Run:  DISPLAY=:99 cargo run -p jetty-app --bin keyprobe
//       KEYPROBE_KITTY=31 … also prints the kitty keyboard protocol encoding
//       for those flags (1 disambiguate, 2 event types, 4 alternate keys,
//       8 all keys, 16 associated text); KEYPROBE_DECCKM=1 encodes for an app
//       in application cursor mode.
// Inject keys with `scripts/nested-live.sh x key …` on a nested display.
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{Ime, Modifiers, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

use jetty_app::input::{self, KeyAction, KeyEventKind, KeyInput, KeyMods, KeyModes, KeyOptions, OptionAsAlt};
use jetty_app::keymap::KeyMap;

struct Probe {
    window: Option<Arc<Window>>,
    mods: Modifiers,
    keymap: KeyMap,
    kitty: u8,
    decckm: bool,
}

/// Bytes as a readable escaped string (`\e[1;5A`, `\x03`, `ş`).
fn shown(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\x1b' => out.push_str("\\e"),
                c if c.is_control() => out.push_str(&format!("\\x{:02x}", u32::from(c))),
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn decision(a: &KeyAction) -> String {
    match a {
        KeyAction::Send(bytes) => format!("sends \"{}\"", shown(bytes)),
        KeyAction::None => "sends nothing".to_string(),
        other => format!("runs {other:?}"),
    }
}

/// A press that winit says typed printable text, while JeTTY sends something
/// else — how a dead key + Space typing " " instead of "'" showed. Ctrl, Alt
/// and Super chords send other bytes by design and are not flagged.
fn mismatch(ev: &KeyInput<'_>, a: &KeyAction) -> Option<String> {
    let m = ev.mods;
    let text = ev.text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control))?;
    if ev.kind == KeyEventKind::Release || m.ctrl || m.alt || m.super_ {
        return None;
    }
    match a {
        KeyAction::Send(bytes) if bytes == text.as_bytes() => None,
        other => Some(format!("⚠ winit typed {text:?}, but jetty {}", decision(other))),
    }
}

/// The session the probe runs in: winit's backend and the variables that
/// decide dead keys and input methods (`XMODIFIERS` = an XIM such as ibus).
fn session(el: &ActiveEventLoop) -> String {
    #[cfg(all(unix, not(target_os = "macos")))]
    let backend = {
        use winit::platform::wayland::ActiveEventLoopExtWayland;
        if el.is_wayland() { "wayland" } else { "x11" }
    };
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let backend = {
        let _ = el;
        std::env::consts::OS
    };
    let var = |k: &str| format!("{k}={}", std::env::var(k).unwrap_or_else(|_| "-".to_string()));
    let vars = ["XDG_SESSION_TYPE", "WAYLAND_DISPLAY", "DISPLAY", "XMODIFIERS"].map(var).join(" ");
    format!("winit backend {backend}; {vars}")
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            let w = el
                .create_window(Window::default_attributes().with_title("JeTTY keyprobe"))
                .unwrap();
            // As JeTTY: IME on, so Compose / dead keys / input methods route
            // through it (their result arrives as an `Ime::Commit`).
            w.set_ime_allowed(true);
            self.window = Some(Arc::new(w));
            eprintln!("PROBE: {}", session(el));
            eprintln!("PROBE: window created, ready for input (kitty flags {}, DECCKM {})", self.kitty, self.decckm);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, e: WindowEvent) {
        match e {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => {
                let k = KeyMods::from_winit(&m);
                eprintln!(
                    "MODS  ctrl={} shift={} alt={} super={} (lalt={} ralt={})",
                    k.ctrl, k.shift, k.alt, k.super_, k.lalt, k.ralt
                );
                self.mods = m;
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                let kind = KeyEventKind::from_winit(event.state, event.repeat);
                let base = event.key_without_modifiers();
                let ev = KeyInput {
                    physical: event.physical_key,
                    logical: &event.logical_key,
                    key_without_modifiers: &base,
                    text: event.text.as_deref(),
                    location: event.location,
                    kind,
                    mods: KeyMods::from_winit(&self.mods),
                };
                // X11 synthesizes presses for keys held at focus gain; JeTTY
                // ignores those, so the probe labels them.
                let tag = if is_synthetic { " (synthetic)" } else { "" };
                eprintln!(
                    "KEY {kind:?}{tag} physical={:?} logical={:?} key_no_mods={:?} text={:?} \
                     text_all_mods={:?} location={:?}",
                    event.physical_key,
                    event.logical_key,
                    base,
                    event.text,
                    event.text_with_all_modifiers(),
                    event.location,
                );
                let opts = KeyOptions::native(OptionAsAlt::None);
                let legacy = KeyModes { app_cursor: self.decckm, ..KeyModes::default() };
                let a = input::decide_key_event(&self.keymap, &ev, &legacy, &opts, false);
                eprintln!("    → jetty {}", decision(&a));
                if let Some(warning) = mismatch(&ev, &a) {
                    eprintln!("    {warning}");
                }
                if self.kitty != 0 {
                    let modes = KeyModes { kitty_flags: self.kitty, ..legacy };
                    let a = input::decide_key_event(&self.keymap, &ev, &modes, &opts, false);
                    eprintln!("    → jetty, kitty flags {}: {}", self.kitty, decision(&a));
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                eprintln!("IME commit {text:?} → jetty sends \"{}\"", shown(text.as_bytes()));
            }
            WindowEvent::Ime(ime) => eprintln!("IME {ime:?}"),
            _ => {}
        }
    }
}

fn main() {
    let env_u8 = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(0);
    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Wait);
    let mut p = Probe {
        window: None,
        mods: Modifiers::default(),
        keymap: KeyMap::defaults(),
        kitty: env_u8("KEYPROBE_KITTY"),
        decckm: env_u8("KEYPROBE_DECCKM") != 0,
    };
    el.run_app(&mut p).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

    #[test]
    fn printable_text_sent_otherwise_is_flagged() {
        // A dead key + Space on Wayland: winit typed "'", JeTTY sent " ".
        let space = Key::Named(NamedKey::Space);
        let at = PhysicalKey::Code(KeyCode::Space);
        let ev = KeyInput { text: Some("'"), ..KeyInput::press(at, &space, KeyMods::default()) };
        let flagged = mismatch(&ev, &KeyAction::Send(b" ".to_vec())).expect("flagged");
        assert!(flagged.contains("\"'\"") && flagged.contains("sends \" \""), "{flagged}");
        assert_eq!(mismatch(&ev, &KeyAction::Send(b"'".to_vec())), None);
        // Chords send other bytes by design; control text is no typed text.
        let ctrl = KeyInput { mods: KeyMods { ctrl: true, ..KeyMods::default() }, ..ev };
        assert_eq!(mismatch(&ctrl, &KeyAction::Send(vec![0])), None);
        let enter = KeyInput { text: Some("\r"), ..ev };
        assert_eq!(mismatch(&enter, &KeyAction::Send(b"\r".to_vec())), None);
        let release = KeyInput { kind: KeyEventKind::Release, ..ev };
        assert_eq!(mismatch(&release, &KeyAction::None), None);
    }
}
