//! App-side backdrop state (visuals v2, slice E): the `[backdrop]` mirror and
//! its parsed settings, the image decode worker's bookkeeping plus the ONE
//! texture every window on the main device shares, and the opt-in animation's
//! clock (its frames are paced by `effects::anim_step`, like every effect
//! animation). The GPU layer is `jetty_render::Backdrop`: one per window,
//! `None` while the mode is "none" (nothing built), created lazily on the first
//! frame that needs it ([`prepare`]).
//!
//! The image is decoded on a worker thread (`jetty_render::backdrop_image`),
//! which wakes the event loop with `AppEvent::BackdropImage`; the result is
//! uploaded once to the main device and the CPU pixels are dropped (idle RSS).
//! A broken or missing file leaves the base gradient on screen plus a notice.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use jetty_render::backdrop_image::DecodedImage;
use jetty_render::{Backdrop, BackdropFrame, BackdropMode, BackdropSettings, GpuImage};

use crate::config::BackdropConfig;

/// Fallback monitor size for the decode downscale when no monitor is known.
pub(crate) const FALLBACK_MONITOR: (u32, u32) = (3840, 2160);

/// What a decode was asked for — any change re-decodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageKey {
    pub path: PathBuf,
    /// `blur` in thousandths (the decode bakes the blur in).
    pub blur_q: u16,
    /// The monitor size the image is downscaled to cover.
    pub max: (u32, u32),
}

impl ImageKey {
    pub fn blur(&self) -> f32 {
        self.blur_q as f32 / 1000.0
    }
}

/// The decode pipeline of the backdrop image.
#[derive(Debug)]
pub(crate) enum ImageSlot {
    /// Nothing in flight.
    None,
    /// Decoding on the worker thread.
    Loading(ImageKey),
    /// Decoded; waiting for a GPU to upload to (startup race / device rebuild).
    Decoded(ImageKey, Arc<DecodedImage>),
    /// Could not be loaded (a notice was shown); not retried until it changes.
    Failed(ImageKey),
}

impl ImageSlot {
    fn key(&self) -> Option<&ImageKey> {
        match self {
            ImageSlot::None => None,
            ImageSlot::Loading(k) | ImageSlot::Decoded(k, _) | ImageSlot::Failed(k) => Some(k),
        }
    }
}

/// The app's backdrop state (see the module doc).
pub(crate) struct BackdropState {
    /// `[backdrop]` exactly as configured (what `persist` writes back).
    pub cfg: BackdropConfig,
    /// The parsed settings the renderer reads.
    pub settings: BackdropSettings,
    /// The decode pipeline (what is loading, decoded but not uploaded, failed).
    pub image: ImageSlot,
    /// The image on the GPU now, shared by every window on its device. It
    /// stays up while a newer request decodes (no flash of the gradient while
    /// a slider is dragged); a failed request or leaving image mode drops it.
    shown: Option<(ImageKey, Arc<GpuImage>)>,
    /// Generation of the latest decode request: a stale result is dropped.
    gen: u64,
    /// The animation clock.
    clock: Instant,
    /// Motion is reduced (`reduce_motion`): an animated look holds still — see
    /// [`Self::set_calm`].
    calm: bool,
}

impl BackdropState {
    pub fn new(cfg: BackdropConfig) -> Self {
        BackdropState {
            settings: cfg.settings(),
            cfg,
            image: ImageSlot::None,
            shown: None,
            gen: 0,
            clock: Instant::now(),
            calm: false,
        }
    }

    /// Adopt a new `[backdrop]` table. Returns whether anything changed.
    pub fn set_config(&mut self, cfg: BackdropConfig) -> bool {
        if cfg == self.cfg {
            return false;
        }
        self.settings = Self::effective(&cfg, self.calm);
        self.cfg = cfg;
        true
    }

    /// Hold an animated look still while motion is reduced (`reduce_motion`):
    /// it draws the static frame `animate = false` draws, and asks for no
    /// paced wakes. Returns whether the look changed (a repaint is owed).
    pub fn set_calm(&mut self, calm: bool) -> bool {
        if calm == self.calm {
            return false;
        }
        self.calm = calm;
        self.settings = Self::effective(&self.cfg, calm);
        self.cfg.settings().animates()
    }

    /// The settings the renderer reads: `cfg`'s, with the animation off when
    /// `calm`.
    fn effective(cfg: &BackdropConfig, calm: bool) -> BackdropSettings {
        let mut s = cfg.settings();
        if calm {
            s.animate = false;
        }
        s
    }

    /// Whether the backdrop wants paced animation frames on an adapter: the
    /// look moves (`animate` on a gradient / pattern), and never on a CPU
    /// adapter (lavapipe) — there every frame costs real CPU.
    pub fn animates_on(&self, cpu_adapter: bool) -> bool {
        self.settings.animates() && !cpu_adapter
    }

    /// The animation phase (seconds). Wrapped daily so the f32 never loses the
    /// precision a smooth 30 fps drift needs on a long-running session.
    pub fn time(&self) -> f32 {
        (self.clock.elapsed().as_secs_f64() % 86_400.0) as f32
    }

    /// The image the settings want, if any (the decode request key).
    pub fn wanted_image(&self, config_dir: &Path, max: (u32, u32)) -> Option<ImageKey> {
        if self.settings.mode != BackdropMode::Image {
            return None;
        }
        let path = self.cfg.image_path(config_dir)?;
        Some(ImageKey { path, blur_q: (self.settings.blur.clamp(0.0, 1.0) * 1000.0).round() as u16, max })
    }

    /// Bring the image in line with the settings: drop everything when no image
    /// is wanted, else start a decode (via `spawn(generation, key)`) unless the
    /// same image is already shown / loading / decoded / known broken. One
    /// decode at a time: a request made while one is in flight (a slider being
    /// dragged) waits — the app re-syncs when the decode lands, so only the
    /// latest request is decoded next. `max` (the monitor size, an X11
    /// round-trip) is only asked for when an image is wanted.
    pub fn sync_image(
        &mut self,
        config_dir: &Path,
        max: impl FnOnce() -> (u32, u32),
        spawn: impl FnOnce(u64, ImageKey),
    ) {
        let wanted = if self.settings.mode == BackdropMode::Image && !self.cfg.image.trim().is_empty() {
            self.wanted_image(config_dir, max())
        } else {
            None
        };
        let Some(key) = wanted else {
            self.image = ImageSlot::None;
            self.shown = None;
            return;
        };
        if self.image.key() == Some(&key) || self.shown.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        if matches!(self.image, ImageSlot::Loading(_)) {
            return;
        }
        self.gen = self.gen.wrapping_add(1);
        self.image = ImageSlot::Loading(key.clone());
        spawn(self.gen, key);
    }

    /// A decode finished. A stale generation, or one whose request was dropped
    /// meanwhile (image mode left), is ignored. Returns the error to show the
    /// user when this (current) decode failed — the gradient then shows. The
    /// caller re-syncs afterwards (a request may have waited behind this one).
    pub fn on_decoded(&mut self, gen: u64, result: Result<Arc<DecodedImage>, String>) -> Option<String> {
        if gen != self.gen {
            return None;
        }
        let key = match &self.image {
            ImageSlot::Loading(k) => k.clone(),
            _ => return None,
        };
        match result {
            Ok(img) => {
                self.image = ImageSlot::Decoded(key, img);
                None
            }
            Err(e) => {
                self.image = ImageSlot::Failed(key);
                self.shown = None;
                Some(e)
            }
        }
    }

    /// Upload a decoded image to `device` (its CPU pixels are dropped then) and
    /// show it. Returns an error to show when the device cannot hold it.
    pub fn upload_pending(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<String> {
        if !matches!(self.image, ImageSlot::Decoded(..)) {
            return None;
        }
        let ImageSlot::Decoded(key, img) = std::mem::replace(&mut self.image, ImageSlot::None) else {
            return None;
        };
        match GpuImage::upload(device, queue, &img) {
            Some(gpu) => {
                self.shown = Some((key, Arc::new(gpu)));
                None
            }
            None => {
                self.image = ImageSlot::Failed(key);
                self.shown = None;
                Some("too large for this GPU".to_string())
            }
        }
    }

    /// The image on the GPU, when there is one (windows on another device skip it).
    pub fn gpu_image(&self) -> Option<&Arc<GpuImage>> {
        self.shown.as_ref().map(|(_, img)| img)
    }

    /// The main device was rebuilt (GPU loss): a texture on the old device is
    /// useless — forget it so the next `sync_image` decodes again.
    pub fn on_device_rebuilt(&mut self) {
        self.shown = None;
    }
}

/// The image files in `<config_dir>/backgrounds/` (PNG / JPEG by extension,
/// sorted, at most 64) — what the palette offers (and Settings will list).
/// Read only when the palette opens; a missing folder is an empty list.
pub(crate) fn background_images(config_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(config_dir.join("backgrounds")) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            [".png", ".jpg", ".jpeg"].iter().any(|ext| lower.ends_with(ext))
        })
        .collect();
    names.sort();
    names.truncate(64);
    names
}

/// Decode `key` on a worker thread and wake the event loop with the result.
pub(crate) fn spawn_decode(proxy: winit::event_loop::EventLoopProxy<crate::app::AppEvent>, gen: u64, key: ImageKey) {
    let spawned = std::thread::Builder::new().name("jetty-backdrop".into()).spawn(move || {
        let result = jetty_render::backdrop_image::load(&key.path, key.max.0, key.max.1, key.blur()).map(Arc::new);
        let _ = proxy.send_event(crate::app::AppEvent::BackdropImage(gen, result));
    });
    if let Err(e) = spawned {
        eprintln!("jetty: could not start the backdrop image decoder: {e}");
    }
}

/// Prepare one window's backdrop for this frame: build the layer on first use,
/// drop it when the mode is "none" (no GPU objects remain). `Some` = draw it in
/// the grid pass.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare<'a>(
    slot: &'a mut Option<Backdrop>,
    state: &BackdropState,
    gpu: &jetty_render::GpuContext,
    theme: &jetty_core::Theme,
    slide_y: f32,
    scroll_px: f32,
    dpi: f32,
) -> Option<&'a Backdrop> {
    if state.settings.is_off() {
        *slot = None;
        return None;
    }
    let bd = slot.get_or_insert_with(|| Backdrop::new(&gpu.device, gpu.format));
    let frame = BackdropFrame {
        width: gpu.config.width,
        height: gpu.config.height,
        slide_y,
        scroll_px,
        dpi,
        premultiply: gpu.premultiply_clear,
        time: state.time(),
        image: state.gpu_image(),
    };
    if bd.prepare(&gpu.device, &gpu.queue, &state.settings, theme, &frame) {
        Some(&*bd)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_cfg(path: &str, blur: f32) -> BackdropConfig {
        BackdropConfig { mode: "image".into(), image: path.into(), blur, ..BackdropConfig::default() }
    }

    fn tiny_image() -> Arc<DecodedImage> {
        Arc::new(jetty_render::backdrop_image::prepare(
            jetty_render::backdrop_image::RawImage { w: 2, h: 2, rgba: vec![255; 16] },
            2,
            2,
            0.0,
        ))
    }

    #[test]
    fn off_by_default_and_no_image_wanted() {
        let s = BackdropState::new(BackdropConfig::default());
        assert!(s.settings.is_off());
        assert!(!s.animates_on(false));
        assert_eq!(s.wanted_image(Path::new("/c"), (100, 100)), None);
    }

    #[test]
    fn image_decode_life_cycle() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("wall.png", 0.25));
        let mut spawned = Vec::new();
        s.sync_image(dir, || (1920, 1200), |g, k| spawned.push((g, k)));
        assert_eq!(spawned.len(), 1);
        let (gen, key) = spawned[0].clone();
        assert_eq!(key.path, PathBuf::from("/cfg/backgrounds/wall.png"));
        assert_eq!(key.blur_q, 250);
        assert_eq!(key.max, (1920, 1200));
        assert!(matches!(s.image, ImageSlot::Loading(_)));
        // The same request again does not re-spawn.
        s.sync_image(dir, || (1920, 1200), |_, _| panic!("no second decode"));
        // A stale generation is ignored.
        assert_eq!(s.on_decoded(gen + 7, Err("old".into())), None);
        assert!(matches!(s.image, ImageSlot::Loading(_)));
        // The current one lands.
        assert_eq!(s.on_decoded(gen, Ok(tiny_image())), None);
        assert!(matches!(s.image, ImageSlot::Decoded(..)));
        assert!(s.gpu_image().is_none(), "not on a GPU yet");
    }

    #[test]
    fn a_broken_image_is_reported_once_and_not_retried() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("/x/broken.jpg", 0.0));
        let mut gen = 0;
        s.sync_image(dir, || (800, 600), |g, _| gen = g);
        assert_eq!(s.on_decoded(gen, Err("bad JPEG".into())), Some("bad JPEG".into()));
        assert!(matches!(s.image, ImageSlot::Failed(_)));
        // Same settings: no new decode (the notice is not repeated forever).
        s.sync_image(dir, || (800, 600), |_, _| panic!("a known-broken image is not re-decoded"));
        // A different file is tried.
        s.set_config(image_cfg("/x/other.jpg", 0.0));
        let mut again = false;
        s.sync_image(dir, || (800, 600), |_, _| again = true);
        assert!(again);
    }

    #[test]
    fn leaving_image_mode_drops_the_image() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("a.png", 0.0));
        let mut gen = 0;
        s.sync_image(dir, || (800, 600), |g, _| gen = g);
        s.on_decoded(gen, Ok(tiny_image()));
        assert!(s.set_config(BackdropConfig { mode: "theme".into(), ..BackdropConfig::default() }));
        s.sync_image(dir, || (800, 600), |_, _| panic!("no decode for theme mode"));
        assert!(matches!(s.image, ImageSlot::None));
        // An unchanged config reports no change.
        let same = s.cfg.clone();
        assert!(!s.set_config(same));
    }

    #[test]
    fn requests_coalesce_behind_an_in_flight_decode() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("a.png", 0.0));
        let mut spawned = Vec::new();
        s.sync_image(dir, || (800, 600), |g, k| spawned.push((g, k)));
        // A drag: three more blur values while the first decode runs.
        for blur in [0.2, 0.4, 0.6] {
            s.set_config(image_cfg("a.png", blur));
            s.sync_image(dir, || (800, 600), |g, k| spawned.push((g, k)));
        }
        assert_eq!(spawned.len(), 1, "one decode in flight at a time");
        // The first lands; the re-sync decodes only the LATEST request.
        let (gen, _) = spawned[0].clone();
        s.on_decoded(gen, Ok(tiny_image()));
        s.sync_image(dir, || (800, 600), |g, k| spawned.push((g, k)));
        assert_eq!(spawned.len(), 2);
        assert_eq!(spawned[1].1.blur_q, 600);
        // Leaving image mode while a decode runs drops its result.
        s.set_config(BackdropConfig::default());
        s.sync_image(dir, || (800, 600), |_, _| panic!("no decode when off"));
        assert_eq!(s.on_decoded(spawned[1].0, Ok(tiny_image())), None);
        assert!(matches!(s.image, ImageSlot::None));
    }

    #[test]
    fn blur_change_re_decodes() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("a.png", 0.0));
        let mut gens = Vec::new();
        s.sync_image(dir, || (800, 600), |g, _| gens.push(g));
        s.on_decoded(gens[0], Ok(tiny_image()));
        // The blur is baked into the decode: a new value decodes again.
        s.set_config(image_cfg("a.png", 0.5));
        s.sync_image(dir, || (800, 600), |g, _| gens.push(g));
        assert_eq!(gens.len(), 2);
        // The same value again does not.
        s.sync_image(dir, || (800, 600), |_, _| panic!("already loading this one"));
    }

    #[test]
    fn animation_needs_a_real_gpu_and_a_moving_look() {
        let s = BackdropState::new(BackdropConfig {
            mode: "pattern".into(),
            animate: true,
            ..BackdropConfig::default()
        });
        assert!(s.animates_on(false));
        assert!(!s.animates_on(true), "never on a CPU adapter");
        let img = BackdropState::new(BackdropConfig { animate: true, ..image_cfg("a.png", 0.0) });
        assert!(!img.animates_on(false), "images do not animate");
        let still = BackdropState::new(BackdropConfig { mode: "theme".into(), ..BackdropConfig::default() });
        assert!(!still.animates_on(false), "animate is opt-in");
    }

    #[test]
    fn background_folder_listing() {
        let dir = std::env::temp_dir().join(format!("jetty-bd-test-{}", std::process::id()));
        let bg = dir.join("backgrounds");
        std::fs::create_dir_all(&bg).unwrap();
        for f in ["b.JPG", "a.png", "notes.txt", "c.jpeg"] {
            std::fs::write(bg.join(f), b"x").unwrap();
        }
        std::fs::create_dir_all(bg.join("sub.png")).unwrap(); // a folder, not an image
        assert_eq!(background_images(&dir), vec!["a.png", "b.JPG", "c.jpeg"]);
        assert!(background_images(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reduced_motion_holds_an_animated_backdrop_still() {
        let moving = BackdropConfig { mode: "pattern".into(), pattern: "aurora".into(), animate: true, ..BackdropConfig::default() };
        let mut s = BackdropState::new(moving.clone());
        assert!(s.animates_on(false));
        assert!(s.set_calm(true), "the look changes: a repaint is owed");
        assert!(!s.animates_on(false) && !s.settings.animate, "the static frame, no paced wakes");
        assert_eq!(s.cfg, moving, "the config (what Settings shows and saves) is untouched");
        // A new table while calm stays calm; leaving reduced motion moves again.
        assert!(s.set_config(BackdropConfig { pattern: "stars".into(), ..moving.clone() }));
        assert!(!s.animates_on(false));
        assert!(!s.set_calm(true), "no change");
        assert!(s.set_calm(false));
        assert!(s.animates_on(false));
        // A still look has nothing to calm: no repaint.
        let mut still = BackdropState::new(BackdropConfig { animate: false, ..moving });
        assert!(!still.set_calm(true));
    }

    #[test]
    fn time_is_small_and_advances() {
        let s = BackdropState::new(BackdropConfig::default());
        let t = s.time();
        assert!((0.0..86_400.0).contains(&t));
    }
}
