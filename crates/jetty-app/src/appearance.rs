//! System appearance: the desktop's light/dark preference, its reduced-motion
//! wish and its accent color — what `follow_system_theme` (and
//! `reduce_motion = "system"`) follow.
//!
//! * **Linux / BSD**: the freedesktop settings portal
//!   (`org.freedesktop.portal.Settings`, namespace `org.freedesktop.appearance`)
//!   — a cross-desktop standard every portal backend serves (KDE, GNOME, wlroots,
//!   …); NO desktop-specific API. One thread reads the keys once (`ReadOne`,
//!   falling back to the deprecated `Read` on portals older than version 2),
//!   then blocks on the session bus for `SettingChanged` signals — filtered by
//!   the bus to this namespace, so the thread wakes only for a real change: zero
//!   idle CPU, no polling. No portal (a bare window manager) → nothing is ever
//!   reported and JeTTY keeps `theme`.
//! * **macOS** (and any platform where winit reports it): the system theme
//!   (`ActiveEventLoop::system_theme` at startup, `WindowEvent::ThemeChanged`
//!   after) — winit never reports either on X11/Wayland.
//!
//! The D-Bus side is behind the small [`portal::Settings`] trait, so everything
//! that decides something (value parsing, the `ReadOne` → `Read` fallback, no
//! portal, missing keys) is tested without a bus.

/// The desktop's color-scheme preference (`org.freedesktop.appearance
/// color-scheme`: 0 no preference, 1 prefer dark, 2 prefer light).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    NoPreference,
    Dark,
    Light,
}

impl ColorScheme {
    /// The portal's number for it; unknown values (a future spec) → `None`.
    pub fn from_portal(v: u32) -> Option<Self> {
        match v {
            0 => Some(ColorScheme::NoPreference),
            1 => Some(ColorScheme::Dark),
            2 => Some(ColorScheme::Light),
            _ => None,
        }
    }

    /// Whether the light theme applies. "No preference" counts as light: it is
    /// what desktops report for their default (light) look.
    pub fn wants_light(self) -> bool {
        self != ColorScheme::Dark
    }

    /// From a winit window/system theme (macOS, Windows).
    pub fn from_winit(t: winit::window::Theme) -> Self {
        match t {
            winit::window::Theme::Dark => ColorScheme::Dark,
            winit::window::Theme::Light => ColorScheme::Light,
        }
    }
}

/// A report of system appearance settings; each field is `Some` only when it
/// was read or changed (a signal carries one key, the first read all of them).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Appearance {
    pub color_scheme: Option<ColorScheme>,
    /// `reduced-motion`: the user asked desktops to minimize animation.
    pub reduced_motion: Option<bool>,
    /// `accent-color` as sRGB bytes; `Some(None)` = reported but unset.
    pub accent: Option<Option<[u8; 3]>>,
}

impl Appearance {
    /// Nothing reported.
    pub fn is_empty(&self) -> bool {
        *self == Appearance::default()
    }

    /// Fold `other` (newer) into `self`: its reported fields win.
    pub fn merge(&mut self, other: Appearance) {
        if other.color_scheme.is_some() {
            self.color_scheme = other.color_scheme;
        }
        if other.reduced_motion.is_some() {
            self.reduced_motion = other.reduced_motion;
        }
        if other.accent.is_some() {
            self.accent = other.accent;
        }
    }
}

/// Whether the light slot decides the theme — `light_theme` is shown instead of
/// `theme`: following a system that prefers light (or states no preference),
/// with a `light_theme` set. An unknown scheme (no portal, not read yet) keeps
/// the dark/default slot.
pub fn light_slot(follow: bool, scheme: Option<ColorScheme>, light_theme: &str) -> bool {
    follow && scheme.is_some_and(ColorScheme::wants_light) && !light_theme.is_empty()
}

/// The first reading of the portal, handed to whoever must not wait for the
/// event loop (the first tab's `COLORFGBG` is decided on a worker thread before
/// the loop delivers [`crate::app::AppEvent::Appearance`]). Filled once.
#[derive(Clone, Default)]
pub struct FirstReading(std::sync::Arc<(std::sync::Mutex<Option<Appearance>>, std::sync::Condvar)>);

impl FirstReading {
    /// Record the first reading (an empty one = no portal) and wake waiters.
    pub fn set(&self, a: Appearance) {
        let (slot, cv) = &*self.0;
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(a);
            cv.notify_all();
        }
    }

    /// The first reading if it is in, waiting at most `timeout` for it.
    pub fn wait(&self, timeout: std::time::Duration) -> Option<Appearance> {
        let (slot, cv) = &*self.0;
        let g = slot.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = cv.wait_timeout_while(g, timeout, |r| r.is_none()).unwrap_or_else(|e| e.into_inner());
        *g
    }
}

/// Start watching the system appearance (Linux / BSD: the settings portal).
/// `emit` receives the first reading (when there is anything to report) and
/// every change; `first` gets the first reading either way. Returns `false`
/// where there is nothing to watch (macOS — winit reports it there).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn spawn_watcher(first: FirstReading, emit: impl Fn(Appearance) + Send + 'static) -> bool {
    std::thread::Builder::new()
        .name("jetty-appearance".into())
        .spawn(move || {
            if let Err(e) = portal::watch(&first, &emit) {
                // No session bus / no portal: keep `theme`. Not an error the
                // user must act on — a bare window manager has no portal.
                first.set(Appearance::default());
                if std::env::var_os("JETTY_DEBUG").is_some() {
                    eprintln!("jetty: system appearance unavailable ({e})");
                }
            }
        })
        .is_ok()
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn spawn_watcher(first: FirstReading, _emit: impl Fn(Appearance) + Send + 'static) -> bool {
    first.set(Appearance::default());
    false
}

/// The freedesktop settings portal (Linux / BSD).
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod portal {
    use super::{Appearance, ColorScheme, FirstReading};
    use zbus::zvariant::{OwnedValue, Value};

    pub(crate) const DEST: &str = "org.freedesktop.portal.Desktop";
    pub(crate) const PATH: &str = "/org/freedesktop/portal/desktop";
    pub(crate) const IFACE: &str = "org.freedesktop.portal.Settings";
    pub(crate) const NS: &str = "org.freedesktop.appearance";
    pub(crate) const COLOR_SCHEME: &str = "color-scheme";
    pub(crate) const REDUCED_MOTION: &str = "reduced-motion";
    pub(crate) const ACCENT: &str = "accent-color";
    const KEYS: [&str; 3] = [COLOR_SCHEME, REDUCED_MOTION, ACCENT];

    /// Why one setting could not be read.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum ReadError {
        /// The method doesn't exist (`ReadOne` on a portal older than v2).
        UnknownMethod,
        /// The portal has no such setting (a desktop that doesn't set it).
        NotFound,
        /// No portal (or no Settings interface) on this bus.
        NoPortal,
        /// Anything else (a broken reply, the bus went away).
        Other(String),
    }

    /// A D-Bus error name → why the read failed.
    pub(crate) fn classify(error_name: &str) -> ReadError {
        match error_name {
            "org.freedesktop.DBus.Error.UnknownMethod" => ReadError::UnknownMethod,
            "org.freedesktop.portal.Error.NotFound" => ReadError::NotFound,
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.DBus.Error.UnknownInterface" => ReadError::NoPortal,
            other => ReadError::Other(other.to_string()),
        }
    }

    /// The portal's Settings calls — a trait so the logic is tested without a bus.
    pub(crate) trait Settings {
        /// `ReadOne(namespace, key) → v` (portal version 2+).
        fn read_one(&self, key: &str) -> Result<OwnedValue, ReadError>;
        /// The deprecated `Read(namespace, key) → v` whose value is wrapped in
        /// one more variant.
        fn read(&self, key: &str) -> Result<OwnedValue, ReadError>;
    }

    /// Strip the variant layers around a value (`Read` answers `v` holding `v`).
    fn inner<'a>(mut v: &'a Value<'a>) -> &'a Value<'a> {
        while let Value::Value(b) = v {
            v = b;
        }
        v
    }

    fn as_u32(v: &Value<'_>) -> Option<u32> {
        match inner(v) {
            Value::U32(n) => Some(*n),
            Value::I32(n) => u32::try_from(*n).ok(),
            Value::U8(n) => Some(u32::from(*n)),
            Value::U16(n) => Some(u32::from(*n)),
            Value::U64(n) => u32::try_from(*n).ok(),
            Value::I64(n) => u32::try_from(*n).ok(),
            Value::Bool(b) => Some(u32::from(*b)),
            _ => None,
        }
    }

    /// `color-scheme`: 0 / 1 / 2.
    pub(crate) fn parse_color_scheme(v: &Value<'_>) -> Option<ColorScheme> {
        as_u32(v).and_then(ColorScheme::from_portal)
    }

    /// `reduced-motion`: 0 no preference, 1 reduce.
    pub(crate) fn parse_reduced_motion(v: &Value<'_>) -> Option<bool> {
        match as_u32(v)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// `accent-color`: `(ddd)` sRGB in 0..=1; a channel outside that range
    /// means "no accent set" (the spec's way to say unset).
    pub(crate) fn parse_accent(v: &Value<'_>) -> Option<Option<[u8; 3]>> {
        let Value::Structure(s) = inner(v) else { return None };
        let f = s.fields();
        if f.len() != 3 {
            return None;
        }
        let mut rgb = [0u8; 3];
        for (out, field) in rgb.iter_mut().zip(f) {
            let Value::F64(x) = inner(field) else { return None };
            if !(0.0..=1.0).contains(x) {
                return Some(None);
            }
            *out = (x * 255.0).round() as u8;
        }
        Some(Some(rgb))
    }

    /// One setting's value applied to `a`; whether it was understood.
    pub(crate) fn apply(a: &mut Appearance, key: &str, v: &Value<'_>) -> bool {
        match key {
            COLOR_SCHEME => {
                a.color_scheme = parse_color_scheme(v);
                a.color_scheme.is_some()
            }
            REDUCED_MOTION => {
                a.reduced_motion = parse_reduced_motion(v);
                a.reduced_motion.is_some()
            }
            ACCENT => {
                a.accent = parse_accent(v);
                a.accent.is_some()
            }
            _ => false,
        }
    }

    /// A `SettingChanged(namespace, key, value)` signal → the change, when it is
    /// one of ours and its value is understood.
    pub(crate) fn parse_signal(namespace: &str, key: &str, v: &Value<'_>) -> Option<Appearance> {
        if namespace != NS {
            return None;
        }
        let mut a = Appearance::default();
        apply(&mut a, key, v).then_some(a)
    }

    /// Read one key: `ReadOne`, or `Read` once `ReadOne` turned out not to exist
    /// (`legacy` remembers that for the remaining keys).
    pub(crate) fn read_key(s: &impl Settings, key: &str, legacy: &mut bool) -> Result<OwnedValue, ReadError> {
        if !*legacy {
            match s.read_one(key) {
                Err(ReadError::UnknownMethod) => *legacy = true,
                other => return other,
            }
        }
        s.read(key)
    }

    /// The first reading: every appearance key the portal has. `Err` when there
    /// is no portal at all (the caller then keeps `theme`); keys a desktop
    /// doesn't set are simply absent.
    pub(crate) fn read_initial(s: &impl Settings) -> Result<Appearance, ReadError> {
        let mut a = Appearance::default();
        let mut legacy = false;
        for key in KEYS {
            match read_key(s, key, &mut legacy) {
                Ok(v) => {
                    apply(&mut a, key, &v);
                }
                Err(ReadError::NoPortal) => return Err(ReadError::NoPortal),
                // A missing key, or one this portal can't answer: skip it.
                Err(_) => {}
            }
        }
        Ok(a)
    }

    /// The real portal on the session bus.
    struct Bus<'c>(&'c zbus::blocking::Connection);

    impl Bus<'_> {
        fn call(&self, method: &str, key: &str) -> Result<OwnedValue, ReadError> {
            let reply = self.0.call_method(Some(DEST), PATH, Some(IFACE), method, &(NS, key)).map_err(|e| match e {
                zbus::Error::MethodError(name, _, _) => classify(name.as_str()),
                other => ReadError::Other(other.to_string()),
            })?;
            reply.body().deserialize::<OwnedValue>().map_err(|e| ReadError::Other(e.to_string()))
        }
    }

    impl Settings for Bus<'_> {
        fn read_one(&self, key: &str) -> Result<OwnedValue, ReadError> {
            self.call("ReadOne", key)
        }
        fn read(&self, key: &str) -> Result<OwnedValue, ReadError> {
            self.call("Read", key)
        }
    }

    /// Subscribe, read once, then forward changes forever (this thread blocks
    /// on the bus between them). Returns only on an error.
    pub(crate) fn watch(first: &FirstReading, emit: &dyn Fn(Appearance)) -> Result<(), String> {
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        // Subscribe BEFORE the first read so no change can slip in between. The
        // bus matches the well-known sender and the namespace (arg0) itself:
        // the other settings a desktop changes never wake this thread.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(DEST)
            .and_then(|b| b.path(PATH))
            .and_then(|b| b.interface(IFACE))
            .and_then(|b| b.member("SettingChanged"))
            .and_then(|b| b.arg(0, NS))
            .map_err(|e| e.to_string())?
            .build();
        let changes = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64))
            .map_err(|e| e.to_string())?;
        let initial = read_initial(&Bus(&conn)).map_err(|e| format!("{e:?}"))?;
        first.set(initial);
        if !initial.is_empty() {
            emit(initial);
        }
        for msg in changes {
            let Ok(msg) = msg else { continue };
            let body = msg.body();
            let Ok((namespace, key, value)) = body.deserialize::<(String, String, OwnedValue)>() else {
                continue;
            };
            if let Some(change) = parse_signal(&namespace, &key, &value) {
                emit(change);
            }
        }
        Err("the session bus closed".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::RefCell;
        use std::collections::HashMap;
        use zbus::zvariant::Structure;

        fn owned(v: Value<'static>) -> OwnedValue {
            OwnedValue::try_from(v).unwrap()
        }

        /// Values as `Read` returns them: wrapped in one more variant.
        fn legacy_wrap(v: Value<'static>) -> OwnedValue {
            owned(Value::Value(Box::new(v)))
        }

        /// A scripted portal: `read_one` and `read` answers per key, plus a log.
        #[derive(Default)]
        struct Fake {
            read_one: HashMap<&'static str, Result<OwnedValue, ReadError>>,
            read: HashMap<&'static str, Result<OwnedValue, ReadError>>,
            /// Answer for a key with no scripted answer.
            missing: Option<ReadError>,
            calls: RefCell<Vec<String>>,
        }

        impl Fake {
            fn answer(&self, map: &HashMap<&'static str, Result<OwnedValue, ReadError>>, key: &str) -> Result<OwnedValue, ReadError> {
                match map.get(key) {
                    Some(Ok(v)) => Ok(v.try_clone().unwrap()),
                    Some(Err(e)) => Err(e.clone()),
                    None => Err(self.missing.clone().unwrap_or(ReadError::NotFound)),
                }
            }
        }

        impl Settings for Fake {
            fn read_one(&self, key: &str) -> Result<OwnedValue, ReadError> {
                self.calls.borrow_mut().push(format!("ReadOne {key}"));
                self.answer(&self.read_one, key)
            }
            fn read(&self, key: &str) -> Result<OwnedValue, ReadError> {
                self.calls.borrow_mut().push(format!("Read {key}"));
                self.answer(&self.read, key)
            }
        }

        fn accent(r: f64, g: f64, b: f64) -> Value<'static> {
            Value::Structure(Structure::from((r, g, b)))
        }

        #[test]
        fn color_scheme_values_parse_bare_and_wrapped() {
            for (n, want) in [(0, ColorScheme::NoPreference), (1, ColorScheme::Dark), (2, ColorScheme::Light)] {
                assert_eq!(parse_color_scheme(&Value::U32(n)), Some(want), "ReadOne shape");
                let wrapped = Value::Value(Box::new(Value::U32(n)));
                assert_eq!(parse_color_scheme(&wrapped), Some(want), "Read shape (v in v)");
            }
            assert_eq!(parse_color_scheme(&Value::U32(7)), None, "a future value is ignored");
            assert_eq!(parse_color_scheme(&Value::from("dark")), None);
        }

        #[test]
        fn reduced_motion_and_accent_parse() {
            assert_eq!(parse_reduced_motion(&Value::U32(0)), Some(false));
            assert_eq!(parse_reduced_motion(&Value::Value(Box::new(Value::U32(1)))), Some(true));
            assert_eq!(parse_reduced_motion(&Value::U32(5)), None);
            // The value this KDE Plasma 6 machine reports.
            assert_eq!(parse_accent(&accent(0.168627, 0.552941, 0.909804)), Some(Some([43, 141, 232])));
            assert_eq!(parse_accent(&Value::Value(Box::new(accent(1.0, 0.0, 0.0)))), Some(Some([255, 0, 0])));
            // Out of range = "no accent set".
            assert_eq!(parse_accent(&accent(-1.0, -1.0, -1.0)), Some(None));
            assert_eq!(parse_accent(&Value::U32(1)), None);
        }

        #[test]
        fn initial_read_uses_read_one_on_a_modern_portal() {
            let mut fake = Fake::default();
            fake.read_one.insert(COLOR_SCHEME, Ok(owned(Value::U32(1))));
            fake.read_one.insert(REDUCED_MOTION, Ok(owned(Value::U32(0))));
            fake.read_one.insert(ACCENT, Ok(owned(accent(0.5, 0.5, 0.5))));
            let a = read_initial(&fake).unwrap();
            assert_eq!(a.color_scheme, Some(ColorScheme::Dark));
            assert_eq!(a.reduced_motion, Some(false));
            assert_eq!(a.accent, Some(Some([128, 128, 128])));
            assert!(fake.calls.borrow().iter().all(|c| c.starts_with("ReadOne")), "{:?}", fake.calls.borrow());
        }

        #[test]
        fn an_old_portal_falls_back_to_read_once() {
            let mut fake = Fake::default();
            for k in KEYS {
                fake.read_one.insert(k, Err(ReadError::UnknownMethod));
            }
            fake.read.insert(COLOR_SCHEME, Ok(legacy_wrap(Value::U32(2))));
            fake.read.insert(REDUCED_MOTION, Ok(legacy_wrap(Value::U32(1))));
            let a = read_initial(&fake).unwrap();
            assert_eq!(a.color_scheme, Some(ColorScheme::Light));
            assert_eq!(a.reduced_motion, Some(true));
            assert_eq!(a.accent, None, "the old portal has no accent-color");
            // ReadOne is tried once; every key after that goes straight to Read.
            assert_eq!(
                *fake.calls.borrow(),
                ["ReadOne color-scheme", "Read color-scheme", "Read reduced-motion", "Read accent-color"]
            );
        }

        #[test]
        fn missing_keys_are_absent_not_fatal() {
            let mut fake = Fake::default();
            fake.read_one.insert(COLOR_SCHEME, Ok(owned(Value::U32(0))));
            // reduced-motion / accent-color: NotFound (the `missing` default).
            let a = read_initial(&fake).unwrap();
            assert_eq!(a.color_scheme, Some(ColorScheme::NoPreference));
            assert_eq!((a.reduced_motion, a.accent), (None, None));
            // A desktop that sets none of them: an empty, valid reading.
            let empty = read_initial(&Fake::default()).unwrap();
            assert!(empty.is_empty());
        }

        #[test]
        fn no_portal_is_reported_as_such() {
            let fake = Fake { missing: Some(ReadError::NoPortal), ..Default::default() };
            assert_eq!(read_initial(&fake), Err(ReadError::NoPortal));
            assert_eq!(fake.calls.borrow().len(), 1, "gives up at the first call");
        }

        #[test]
        fn dbus_error_names_are_classified() {
            assert_eq!(classify("org.freedesktop.DBus.Error.UnknownMethod"), ReadError::UnknownMethod);
            assert_eq!(classify("org.freedesktop.portal.Error.NotFound"), ReadError::NotFound);
            assert_eq!(classify("org.freedesktop.DBus.Error.ServiceUnknown"), ReadError::NoPortal);
            assert_eq!(classify("org.freedesktop.DBus.Error.UnknownInterface"), ReadError::NoPortal);
            assert!(matches!(classify("org.example.Weird"), ReadError::Other(_)));
        }

        #[test]
        fn signals_for_other_namespaces_or_keys_are_ignored() {
            let a = parse_signal(NS, COLOR_SCHEME, &Value::U32(2)).unwrap();
            assert_eq!(a, Appearance { color_scheme: Some(ColorScheme::Light), ..Default::default() });
            let a = parse_signal(NS, REDUCED_MOTION, &Value::U32(1)).unwrap();
            assert_eq!(a.reduced_motion, Some(true));
            assert!(parse_signal("org.kde.kdeglobals.General", COLOR_SCHEME, &Value::U32(1)).is_none());
            assert!(parse_signal(NS, "contrast", &Value::U32(1)).is_none());
            assert!(parse_signal(NS, COLOR_SCHEME, &Value::from("nope")).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_decision_follows_only_when_asked() {
        use ColorScheme::*;
        let light = "catppuccin_latte";
        // Off: always the chosen (dark/default) theme.
        for s in [None, Some(Dark), Some(Light), Some(NoPreference)] {
            assert!(!light_slot(false, s, light), "{s:?}");
        }
        // On: light and no-preference → light_theme; dark / unknown → theme.
        assert!(light_slot(true, Some(Light), light));
        assert!(light_slot(true, Some(NoPreference), light), "no preference = light");
        assert!(!light_slot(true, Some(Dark), light));
        assert!(!light_slot(true, None, light), "no portal keeps `theme`");
        // An empty light_theme means "no light variant".
        assert!(!light_slot(true, Some(Light), ""));
    }

    #[test]
    fn appearance_merge_keeps_unreported_fields() {
        let mut a = Appearance {
            color_scheme: Some(ColorScheme::Dark),
            reduced_motion: Some(true),
            accent: Some(Some([1, 2, 3])),
        };
        a.merge(Appearance { color_scheme: Some(ColorScheme::Light), ..Default::default() });
        assert_eq!(a.color_scheme, Some(ColorScheme::Light));
        assert_eq!(a.reduced_motion, Some(true));
        assert_eq!(a.accent, Some(Some([1, 2, 3])));
        a.merge(Appearance { accent: Some(None), ..Default::default() });
        assert_eq!(a.accent, Some(None), "an accent reported unset clears it");
    }

    #[test]
    fn winit_themes_map() {
        assert_eq!(ColorScheme::from_winit(winit::window::Theme::Dark), ColorScheme::Dark);
        assert_eq!(ColorScheme::from_winit(winit::window::Theme::Light), ColorScheme::Light);
        assert!(ColorScheme::Light.wants_light() && ColorScheme::NoPreference.wants_light());
        assert!(!ColorScheme::Dark.wants_light());
    }

    #[test]
    fn first_reading_hands_over_once_and_times_out_empty() {
        let f = FirstReading::default();
        assert_eq!(f.wait(std::time::Duration::from_millis(1)), None, "not in yet");
        let g = f.clone();
        let t = std::thread::spawn(move || {
            g.set(Appearance { color_scheme: Some(ColorScheme::Light), ..Default::default() });
        });
        let got = f.wait(std::time::Duration::from_secs(5));
        t.join().unwrap();
        assert_eq!(got.and_then(|a| a.color_scheme), Some(ColorScheme::Light));
        f.set(Appearance { color_scheme: Some(ColorScheme::Dark), ..Default::default() });
        assert_eq!(
            f.wait(std::time::Duration::ZERO).and_then(|a| a.color_scheme),
            Some(ColorScheme::Light),
            "only the FIRST reading is kept"
        );
    }
}
