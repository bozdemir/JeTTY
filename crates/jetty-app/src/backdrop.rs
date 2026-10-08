//! App-side backdrop state (visuals v2, slice E): the `[backdrop]` mirror and
//! its parsed settings, the image decode worker's bookkeeping plus the ONE
//! texture every window on the main device shares, and the opt-in animation's
//! 30 fps pacing. The GPU layer is `jetty_render::Backdrop`: one per window,
//! `None` while the mode is "none" (nothing built), created lazily on the first
//! frame that needs it ([`prepare`]).
//!
//! The image is decoded on a worker thread (`jetty_render::backdrop_image`),
//! which wakes the event loop with `AppEvent::BackdropImage`; the result is
//! uploaded once to the main device and the CPU pixels are dropped (idle RSS).
//! A broken or missing file leaves the base gradient on screen plus a notice.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jetty_render::backdrop_image::DecodedImage;
use jetty_render::{Backdrop, BackdropFrame, BackdropMode, BackdropSettings, GpuImage};

use crate::config::BackdropConfig;

/// Frame interval of an animated backdrop: ≤ 30 fps, as timed wakes (never Poll).
pub(crate) const ANIM_INTERVAL: Duration = Duration::from_millis(33);

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

/// The backdrop image's life cycle.
#[derive(Debug)]
pub(crate) enum ImageSlot {
    /// No image wanted (mode is not "image", or no file named).
    None,
    /// Decoding on the worker thread.
    Loading(ImageKey),
    /// Decoded; waiting for the main GPU (startup race / device rebuild).
    Decoded(ImageKey, Arc<DecodedImage>),
    /// On the GPU, shared by every window on that device.
    Ready(ImageKey, Arc<GpuImage>),
    /// Could not be loaded (a notice was shown): the base gradient shows.
    Failed(ImageKey),
}

impl ImageSlot {
    fn key(&self) -> Option<&ImageKey> {
        match self {
            ImageSlot::None => None,
            ImageSlot::Loading(k) | ImageSlot::Decoded(k, _) | ImageSlot::Ready(k, _) | ImageSlot::Failed(k) => Some(k),
        }
    }
}

/// The app's backdrop state (see the module doc).
pub(crate) struct BackdropState {
    /// `[backdrop]` exactly as configured (what `persist` writes back).
    pub cfg: BackdropConfig,
    /// The parsed settings the renderer reads.
    pub settings: BackdropSettings,
    pub image: ImageSlot,
    /// Generation of the latest decode request: a stale result is dropped.
    gen: u64,
    /// The animation clock.
    clock: Instant,
    /// The next animation wake while the backdrop animates; `None` otherwise.
    pub tick_at: Option<Instant>,
    /// The main adapter is a CPU rasterizer (lavapipe): never animate there.
    pub cpu_adapter: bool,
}

impl BackdropState {
    pub fn new(cfg: BackdropConfig) -> Self {
        BackdropState {
            settings: cfg.settings(),
            cfg,
            image: ImageSlot::None,
            gen: 0,
            clock: Instant::now(),
            tick_at: None,
            cpu_adapter: false,
        }
    }

    /// Adopt a new `[backdrop]` table. Returns whether anything changed.
    pub fn set_config(&mut self, cfg: BackdropConfig) -> bool {
        if cfg == self.cfg {
            return false;
        }
        self.settings = cfg.settings();
        self.cfg = cfg;
        true
    }

    /// Whether frames must be paced for the animation right now (the look
    /// moves, and the adapter is a real GPU).
    pub fn animates(&self) -> bool {
        self.settings.animates() && !self.cpu_adapter
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

    /// Bring the image slot in line with the settings: drop it when no image is
    /// wanted, else start a decode (via `spawn(generation, key)`) unless the
    /// same image is already loading / loaded / known broken. `max` (the monitor
    /// size, an X11 round-trip) is only asked for when an image is wanted.
    pub fn sync_image(
        &mut self,
        config_dir: &Path,
        max: impl FnOnce() -> (u32, u32),
        spawn: impl FnOnce(u64, ImageKey),
    ) {
        if self.settings.mode != BackdropMode::Image || self.cfg.image.trim().is_empty() {
            self.image = ImageSlot::None;
            return;
        }
        match self.wanted_image(config_dir, max()) {
            None => {
                self.image = ImageSlot::None;
            }
            Some(key) => {
                if self.image.key() == Some(&key) {
                    return;
                }
                self.gen = self.gen.wrapping_add(1);
                self.image = ImageSlot::Loading(key.clone());
                spawn(self.gen, key);
            }
        }
    }

    /// A decode finished. A stale generation is ignored. Returns the error to
    /// show the user when this (current) decode failed.
    pub fn on_decoded(&mut self, gen: u64, result: Result<Arc<DecodedImage>, String>) -> Option<String> {
        if gen != self.gen {
            return None;
        }
        let ImageSlot::Loading(key) = std::mem::replace(&mut self.image, ImageSlot::None) else {
            return None;
        };
        match result {
            Ok(img) => {
                self.image = ImageSlot::Decoded(key, img);
                None
            }
            Err(e) => {
                self.image = ImageSlot::Failed(key);
                Some(e)
            }
        }
    }

    /// Upload a decoded image to `device` (then its CPU pixels are dropped).
    /// Returns an error to show when the device cannot hold it.
    pub fn upload_pending(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<String> {
        if !matches!(self.image, ImageSlot::Decoded(..)) {
            return None;
        }
        let ImageSlot::Decoded(key, img) = std::mem::replace(&mut self.image, ImageSlot::None) else {
            return None;
        };
        match GpuImage::upload(device, queue, &img) {
            Some(gpu) => {
                self.image = ImageSlot::Ready(key, Arc::new(gpu));
                None
            }
            None => {
                self.image = ImageSlot::Failed(key);
                Some("too large for this GPU".to_string())
            }
        }
    }

    /// The uploaded image, when ready (windows on another device skip it).
    pub fn gpu_image(&self) -> Option<&Arc<GpuImage>> {
        match &self.image {
            ImageSlot::Ready(_, img) => Some(img),
            _ => None,
        }
    }

    /// The main device was rebuilt (GPU loss): a texture on the old device is
    /// useless — forget it so the next `sync_image` decodes again.
    pub fn on_device_rebuilt(&mut self) {
        if matches!(self.image, ImageSlot::Ready(..)) {
            self.image = ImageSlot::None;
        }
    }
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
        assert!(!s.animates());
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
    fn blur_change_re_decodes() {
        let dir = Path::new("/cfg");
        let mut s = BackdropState::new(image_cfg("a.png", 0.0));
        let mut n = 0;
        s.sync_image(dir, || (800, 600), |_, _| n += 1);
        s.set_config(image_cfg("a.png", 0.5));
        s.sync_image(dir, || (800, 600), |_, _| n += 1);
        assert_eq!(n, 2);
    }

    #[test]
    fn animation_needs_a_real_gpu_and_a_moving_look() {
        let mut s = BackdropState::new(BackdropConfig {
            mode: "pattern".into(),
            animate: true,
            ..BackdropConfig::default()
        });
        assert!(s.animates());
        s.cpu_adapter = true;
        assert!(!s.animates(), "never on a CPU adapter");
        let img = BackdropState::new(BackdropConfig { animate: true, ..image_cfg("a.png", 0.0) });
        assert!(!img.animates(), "images do not animate");
    }

    #[test]
    fn time_is_small_and_advances() {
        let s = BackdropState::new(BackdropConfig::default());
        let t = s.time();
        assert!((0.0..86_400.0).contains(&t));
    }
}
