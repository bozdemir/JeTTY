//! Startup Vulkan driver pre-selection (Linux): keep the Khronos loader from
//! initializing drivers the first GPU instance cannot or will not use.
//!
//! Creating a Vulkan instance makes the loader `dlopen` and initialize EVERY
//! installed driver (ICD) to ask for its extensions and devices — wanted or
//! not. On a hybrid Intel + NVIDIA laptop with Mesa installed that is the
//! NVIDIA driver (its device enumeration alone is ~50 ms), lavapipe and RADV
//! (each pulls in libLLVM, 138 MB on disk), nouveau, asahi, virtio, … — about
//! 55 ms of JeTTY's ~75 ms GPU block and ~75 MB of mapped libraries, plus open
//! NVIDIA device handles for the life of the process, all to pick the
//! integrated GPU anyway.
//!
//! The filter is the loader's own, standard `VK_LOADER_DRIVERS_DISABLE`
//! (loader ≥ 1.3.234; an older loader ignores it), decided from the kernel's
//! view of the GPUs (`/sys/class/drm`) — no desktop or compositor specifics:
//! * a Mesa / NVIDIA driver whose kernel driver is absent can have no device:
//!   skipped;
//! * lavapipe (software) only ever wins when no hardware adapter can present:
//!   skipped while a real GPU's kernel driver is present;
//! * the NVIDIA driver, under the default low-power preference while an
//!   integrated-capable GPU is present (wgpu then picks the integrated one).
//!
//! It is only a FIRST attempt: when the adapter it yields is not provably the one
//! the unfiltered loader would pick ([`Plan::accepts`]: an integrated GPU when
//! NVIDIA was skipped, any hardware adapter when lavapipe was) — or none was
//! found, e.g. an integrated GPU that cannot present — the filter is dropped and
//! the instance created again exactly as before. Never applied when the user
//! steers driver or GPU selection (`VK_DRIVER_FILES`, `DRI_PRIME`, …), and
//! released right after the first instance: the shells never see it (the app
//! hides it from them) and later instances (GPU-loss recovery) are unfiltered.

use std::sync::Mutex;

/// The loader variable carrying the filter: comma-separated manifest file-name
/// globs (`*` at either end) of drivers to skip.
pub const FILTER_VAR: &str = "VK_LOADER_DRIVERS_DISABLE";

/// The user picks the drivers: never filter on top.
#[cfg(any(test, target_os = "linux"))]
const USER_DRIVER_VARS: &[&str] = &[
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "VK_ADD_DRIVER_FILES",
    "VK_LOADER_DRIVERS_SELECT",
    "VK_LOADER_DRIVERS_DISABLE",
];

/// The user steers which GPU (or backend, wgpu's `WGPU_BACKEND`) is used: leave
/// every driver to the loader.
#[cfg(any(test, target_os = "linux"))]
const GPU_STEERING_VARS: &[&str] =
    &["DRI_PRIME", "MESA_VK_DEVICE_SELECT", "__NV_PRIME_RENDER_OFFLOAD", "WGPU_BACKEND"];

/// Drivers that can only have a device on top of these kernel drivers:
/// `(manifest name stem, kernel drivers)`. Anything not listed (AMDVLK, ARM
/// drivers, SwiftShader, …) is never filtered.
const KERNEL_BACKED: &[(&str, &[&str])] = &[
    ("intel_icd", &["i915", "xe"]),
    ("intel_hasvk_icd", &["i915"]),
    ("radeon_icd", &["amdgpu"]),
    ("nouveau_icd", &["nouveau"]),
    ("asahi_icd", &["asahi"]),
    ("virtio_icd", &["virtio_gpu"]),
    ("gfxstream_vk_icd", &["virtio_gpu"]),
    ("nvidia_icd", &["nvidia"]),
];

/// Kernel drivers of real (non-virtual) GPUs: with one present a hardware
/// adapter outranks lavapipe.
const REAL_GPU: &[&str] = &["i915", "xe", "amdgpu", "nouveau", "nvidia", "asahi"];

/// Kernel drivers that can back an INTEGRATED GPU (wgpu's low-power pick).
const INTEGRATED_CAPABLE: &[&str] = &["i915", "xe", "amdgpu", "asahi"];

/// The first loader that honors `VK_LOADER_DRIVERS_DISABLE` (1.3.234).
const FIRST_FILTERING_LOADER: u32 = (1 << 22) | (3 << 12) | 234;

/// One startup filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The [`FILTER_VAR`] value.
    pub disable: String,
    /// NVIDIA was skipped for the low-power pick: only an integrated adapter is
    /// certainly what the unfiltered loader would have chosen too.
    pub integrated_only: bool,
    /// lavapipe was skipped: a CPU adapter is not the unfiltered choice.
    pub lavapipe_skipped: bool,
}

/// What a filtered attempt found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub device_type: wgpu::DeviceType,
    /// The Vulkan loader's version (`vkEnumerateInstanceVersion`), if known.
    pub loader_version: Option<u32>,
}

impl Probe {
    /// The probe of an adapter from `instance`.
    pub fn of(instance: &wgpu::Instance, adapter: &wgpu::Adapter) -> Probe {
        Probe { device_type: adapter.get_info().device_type, loader_version: loader_version(instance) }
    }
}

/// The loader version the Vulkan instance was created with.
#[cfg(target_os = "linux")]
fn loader_version(instance: &wgpu::Instance) -> Option<u32> {
    // SAFETY: reads the version number the HAL stored when it created the
    // instance; nothing is mutated and the reference does not outlive `instance`.
    unsafe { instance.as_hal::<wgpu::hal::api::Vulkan>() }.map(|i| i.shared_instance().instance_api_version())
}

#[cfg(not(target_os = "linux"))]
fn loader_version(_instance: &wgpu::Instance) -> Option<u32> {
    None
}

impl Plan {
    /// Whether the filtered attempt's adapter is the one the unfiltered loader
    /// would also have picked (see the module docs).
    pub fn accepts(&self, probe: &Probe) -> bool {
        // A loader too old to filter created an unfiltered instance anyway.
        if probe.loader_version.is_some_and(|v| v < FIRST_FILTERING_LOADER) {
            return true;
        }
        if self.integrated_only {
            return probe.device_type == wgpu::DeviceType::IntegratedGpu;
        }
        !(self.lavapipe_skipped && probe.device_type == wgpu::DeviceType::Cpu)
    }
}

/// The filter for a machine whose GPUs use the kernel `drivers`, with the
/// discrete GPU preferred (`high_performance`, `JETTY_GPU=high`) or not. `None`
/// when nothing would be skipped — or no GPU is visible at all (a VM without
/// one, a sandbox hiding `/sys`), where lavapipe may be the only adapter.
pub fn plan(drivers: &[&str], high_performance: bool) -> Option<Plan> {
    if drivers.is_empty() {
        return None;
    }
    let has = |names: &[&str]| names.iter().any(|n| drivers.contains(n));
    let mut skip: Vec<&str> = KERNEL_BACKED.iter().filter(|(_, needs)| !has(needs)).map(|(icd, _)| *icd).collect();
    let lavapipe_skipped = has(REAL_GPU);
    if lavapipe_skipped {
        skip.push("lvp_icd");
    }
    let integrated_only = !high_performance && has(&["nvidia"]) && has(INTEGRATED_CAPABLE);
    if integrated_only {
        skip.push("nvidia_icd");
    }
    if skip.is_empty() {
        return None;
    }
    let disable = skip.iter().map(|s| format!("*{s}*")).collect::<Vec<_>>().join(",");
    Some(Plan { disable, integrated_only, lavapipe_skipped })
}

/// Whether the user set any of `vars` (non-empty), via `get`.
#[cfg(any(test, target_os = "linux"))]
fn any_set(vars: &[&str], get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    vars.iter().any(|v| get(v).is_some_and(|x| !x.is_empty()))
}

/// The kernel drivers behind the DRM render nodes (+ `nvidia` when its module is
/// loaded: the NVIDIA driver also works without a DRM node). Sorted, deduped.
#[cfg(target_os = "linux")]
fn drm_drivers() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/drm") {
        for e in dir.flatten() {
            if !e.file_name().to_string_lossy().starts_with("renderD") {
                continue;
            }
            if let Ok(link) = std::fs::read_link(e.path().join("device/driver")) {
                if let Some(name) = link.file_name().and_then(|n| n.to_str()) {
                    out.push(name.to_string());
                }
            }
        }
    }
    if std::path::Path::new("/sys/module/nvidia").exists() {
        out.push("nvidia".to_string());
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The filter installed for this process's first instance (`None` once released).
static ACTIVE: Mutex<Option<Plan>> = Mutex::new(None);

/// `JETTY_GPU=high` (aliases `discrete`, `dgpu`): the discrete GPU is wanted.
/// The ONE parse of that variable — `GpuContext::new` and the filter must agree.
pub fn wants_high_performance() -> bool {
    matches!(std::env::var("JETTY_GPU").as_deref(), Ok("high") | Ok("discrete") | Ok("dgpu"))
}

/// Install the startup filter for `high_performance`, when this machine has one
/// (see [`plan`]) and the user steers neither drivers nor GPU. Sets an
/// environment variable, so call it ONCE, early, while the process has a single
/// thread. Returns the plan installed (hide [`FILTER_VAR`] from child shells
/// then: the first one is spawned while the filter is still in place).
#[cfg(target_os = "linux")]
pub fn install(high_performance: bool) -> Option<Plan> {
    let env = |v: &str| std::env::var_os(v);
    if any_set(USER_DRIVER_VARS, &env) || any_set(GPU_STEERING_VARS, &env) {
        return None;
    }
    let drivers = drm_drivers();
    let names: Vec<&str> = drivers.iter().map(String::as_str).collect();
    let plan = plan(&names, high_performance)?;
    std::env::set_var(FILTER_VAR, &plan.disable);
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(plan.clone());
    Some(plan)
}

#[cfg(not(target_os = "linux"))]
pub fn install(_high_performance: bool) -> Option<Plan> {
    None
}

/// Drop the startup filter: later instances see every driver. Overwrites the
/// variable with an empty value (the loader's "no filter") rather than removing
/// it — replacing a value is a single pointer store other threads can't trip on,
/// removing one shifts the whole environment under them. Idempotent.
pub fn release() {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if active.take().is_some() {
        std::env::set_var(FILTER_VAR, "");
    }
}

/// Run `attempt` (create a Vulkan instance and pick an adapter) under the startup
/// filter: kept when `probe` shows the unfiltered loader would have picked the
/// same adapter, else the filter is dropped and `attempt` runs again unfiltered —
/// exactly the pre-filter behavior. Without an installed filter this is just
/// `attempt()`. The filter is always released afterwards.
pub fn with_prefilter<T, E>(attempt: impl FnMut() -> Result<T, E>, probe: impl Fn(&T) -> Probe) -> Result<T, E> {
    let plan = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    retry_unfiltered(plan.as_ref(), attempt, probe, release)
}

/// [`with_prefilter`]'s logic over an explicit plan and release hook (testable).
fn retry_unfiltered<T, E>(
    plan: Option<&Plan>,
    mut attempt: impl FnMut() -> Result<T, E>,
    probe: impl Fn(&T) -> Probe,
    release: impl FnOnce(),
) -> Result<T, E> {
    let first = attempt();
    let Some(plan) = plan else { return first };
    let keep = first.as_ref().is_ok_and(|t| plan.accepts(&probe(t)));
    release();
    if keep {
        return first;
    }
    // The filtered instance (and its surface) go before the unfiltered retry.
    drop(first);
    attempt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, VirtualGpu};

    const NEW_LOADER: Option<u32> = Some((1 << 22) | (4 << 12) | 341);
    const OLD_LOADER: Option<u32> = Some((1 << 22) | (3 << 12) | 204);

    fn probe(device_type: wgpu::DeviceType) -> Probe {
        Probe { device_type, loader_version: NEW_LOADER }
    }

    fn skipped(p: &Plan) -> Vec<&str> {
        p.disable.split(',').map(|g| g.trim_matches('*')).collect()
    }

    #[test]
    fn a_hybrid_intel_nvidia_laptop_keeps_only_the_intel_drivers() {
        let p = plan(&["i915", "nvidia"], false).expect("a plan");
        let s = skipped(&p);
        for icd in ["nvidia_icd", "lvp_icd", "radeon_icd", "nouveau_icd", "asahi_icd", "virtio_icd", "gfxstream_vk_icd"] {
            assert!(s.contains(&icd), "{icd} not skipped: {s:?}");
        }
        assert!(!s.contains(&"intel_icd") && !s.contains(&"intel_hasvk_icd"), "{s:?}");
        assert!(p.integrated_only && p.lavapipe_skipped);
        // Each glob names one manifest family: `*intel_icd*` must not also hit
        // `intel_hasvk_icd.json`, nor `*radeon_icd*` AMDVLK's `amd_icd64.json`.
        assert!(!"intel_hasvk_icd.json".contains("intel_icd"));
        assert!(!"amd_icd64.json".contains("radeon_icd"));
    }

    #[test]
    fn the_discrete_preference_keeps_nvidia() {
        let p = plan(&["i915", "nvidia"], true).expect("a plan");
        assert!(!skipped(&p).contains(&"nvidia_icd"));
        assert!(!p.integrated_only);
    }

    #[test]
    fn nvidia_without_an_integrated_gpu_is_kept() {
        let p = plan(&["nvidia"], false).expect("lavapipe and the absent Mesa drivers are skipped");
        let s = skipped(&p);
        assert!(!s.contains(&"nvidia_icd") && s.contains(&"lvp_icd") && s.contains(&"intel_icd"), "{s:?}");
        assert!(!p.integrated_only);
        // An AMD APU + NVIDIA laptop: RADV stays, NVIDIA goes for the low-power pick.
        let p = plan(&["amdgpu", "nvidia"], false).unwrap();
        assert!(!skipped(&p).contains(&"radeon_icd") && skipped(&p).contains(&"nvidia_icd"));
    }

    #[test]
    fn no_visible_gpu_filters_nothing_and_a_vm_keeps_lavapipe() {
        assert_eq!(plan(&[], false), None, "sysfs hidden / no GPU: leave the loader alone");
        let p = plan(&["virtio_gpu"], false).unwrap();
        let s = skipped(&p);
        assert!(!s.contains(&"lvp_icd") && !s.contains(&"virtio_icd"), "{s:?}");
        assert!(!p.lavapipe_skipped);
        // An unknown GPU driver alone: nothing proves a hardware adapter, so
        // lavapipe stays (only drivers with absent kernel drivers go).
        assert!(!plan(&["msm"], false).unwrap().lavapipe_skipped);
    }

    #[test]
    fn acceptance_matches_what_the_unfiltered_loader_would_pick() {
        let hybrid = plan(&["i915", "nvidia"], false).unwrap();
        assert!(hybrid.accepts(&probe(IntegratedGpu)));
        // A discrete pick (an Arc card on i915/xe) or nothing at all: NVIDIA
        // might have ranked differently — retry unfiltered.
        assert!(!hybrid.accepts(&probe(DiscreteGpu)));
        assert!(!hybrid.accepts(&probe(Cpu)));
        let nv = plan(&["nvidia"], false).unwrap();
        assert!(nv.accepts(&probe(DiscreteGpu)));
        assert!(!nv.accepts(&probe(Cpu)), "lavapipe was skipped: a CPU pick is not the unfiltered one");
        let vm = plan(&["virtio_gpu"], false).unwrap();
        assert!(vm.accepts(&probe(VirtualGpu)) && vm.accepts(&probe(Cpu)));
        // A loader that ignores the variable created an unfiltered instance.
        assert!(hybrid.accepts(&Probe { device_type: DiscreteGpu, loader_version: OLD_LOADER }));
    }

    #[test]
    fn steering_variables_turn_the_filter_off() {
        let none = |_: &str| None;
        assert!(!any_set(USER_DRIVER_VARS, &none) && !any_set(GPU_STEERING_VARS, &none));
        let prime = |v: &str| (v == "DRI_PRIME").then(|| std::ffi::OsString::from("1"));
        assert!(any_set(GPU_STEERING_VARS, &prime));
        let empty = |v: &str| (v == "VK_ICD_FILENAMES").then(std::ffi::OsString::new);
        assert!(!any_set(USER_DRIVER_VARS, &empty), "an empty value steers nothing");
        let icd = |v: &str| (v == "VK_ICD_FILENAMES").then(|| std::ffi::OsString::from("/x.json"));
        assert!(any_set(USER_DRIVER_VARS, &icd));
    }

    #[test]
    fn a_rejected_attempt_is_dropped_released_and_retried_unfiltered() {
        let hybrid = plan(&["i915", "nvidia"], false).unwrap();
        // Accepted: one attempt, released afterwards.
        let (mut calls, mut released) = (0, false);
        let r: Result<wgpu::DeviceType, ()> =
            retry_unfiltered(Some(&hybrid), || { calls += 1; Ok(IntegratedGpu) }, |t| probe(*t), || released = true);
        assert_eq!((r, calls, released), (Ok(IntegratedGpu), 1, true));
        // Rejected (discrete): a second, unfiltered attempt decides.
        let (mut calls, mut released) = (0, false);
        let r: Result<wgpu::DeviceType, ()> = retry_unfiltered(
            Some(&hybrid),
            || { calls += 1; Ok(if calls == 1 { DiscreteGpu } else { IntegratedGpu }) },
            |t| probe(*t),
            || released = true,
        );
        assert_eq!((r, calls, released), (Ok(IntegratedGpu), 2, true));
        // Nothing found under the filter: retried unfiltered too.
        let mut calls = 0;
        let r: Result<wgpu::DeviceType, &str> = retry_unfiltered(
            Some(&hybrid),
            || { calls += 1; if calls == 1 { Err("none") } else { Ok(DiscreteGpu) } },
            |t| probe(*t),
            || {},
        );
        assert_eq!((r, calls), (Ok(DiscreteGpu), 2));
        // No filter: exactly one attempt, whatever it yields.
        let mut calls = 0;
        let r: Result<wgpu::DeviceType, ()> = retry_unfiltered(None, || { calls += 1; Ok(Cpu) }, |t| probe(*t), || {});
        assert_eq!((r, calls), (Ok(Cpu), 1));
    }
}
