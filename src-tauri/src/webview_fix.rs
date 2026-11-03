//! A platform fix for the app's own webview: WebKitGTK 2.52 segfaults on a
//! page change when NVIDIA's EGL draws the window (Linux, the NVIDIA driver
//! loaded; seen in three apps and in WebKit's own MiniBrowser, 2026-10).
//!
//! The fix is Mesa's EGL vendor file, set before the webview starts, so
//! WebKit does not draw through NVIDIA's EGL:
//!
//! - beside another GPU (an Intel or AMD laptop GPU, as on Optimus laptops)
//!   Mesa's EGL alone: WebKit draws on that GPU. Adding the GBM switch there
//!   swaps red and blue on X11 (found 2026-11-02 on an Optimus laptop: the
//!   whole app showed brown on XWayland), so it is left alone;
//! - on a machine whose only GPU is NVIDIA, Mesa's EGL alone aborts at
//!   startup ("Could not create GBM EGL display"), so WebKit's GBM path is
//!   switched off too: the pair is the only setup that drew there.
//!
//! Call [`apply`] first thing in `run`, before any thread starts (it sets
//! process environment variables). Nothing is set when the person set either
//! variable themselves, or set `FLOWSTA_WEBVIEW_FIX=0`. Sidecars spawned
//! through [`crate::process_ext::SidecarCommand`] never inherit what was set
//! here: the fix is for the webview only.
//!
//! The same module as `flowsta-hc-lines`' `webview` (Flowsta Vault), until
//! ProofPoll uses that library.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The EGL vendor selection variable (libglvnd).
pub const EGL_VENDOR_VAR: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";
/// WebKitGTK's switch for the GBM buffer path of its DMA-BUF renderer.
pub const DISABLE_GBM_VAR: &str = "WEBKIT_DMABUF_RENDERER_DISABLE_GBM";
/// `0` turns the fix off.
pub const OPT_OUT_VAR: &str = "FLOWSTA_WEBVIEW_FIX";

/// Where distributions install Mesa's EGL vendor file, most common first.
pub const MESA_VENDOR_FILES: &[&str] = &[
    "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
    "/etc/glvnd/egl_vendor.d/50_mesa.json",
];

/// What the machine looks like, as far as the fix cares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The NVIDIA kernel driver is loaded.
    pub nvidia_loaded: bool,
    /// A GPU driven by another driver is present (a render node whose driver
    /// is not `nvidia`: Intel or AMD beside NVIDIA, as on Optimus laptops).
    pub other_gpu: bool,
    /// The person set [`EGL_VENDOR_VAR`] themselves.
    pub egl_vendor_set: bool,
    /// The person set [`DISABLE_GBM_VAR`] themselves.
    pub disable_gbm_set: bool,
    /// [`OPT_OUT_VAR`] is `0`.
    pub opted_out: bool,
    /// Mesa's EGL vendor file, when one exists.
    pub mesa_vendor_file: Option<PathBuf>,
}

/// What [`apply`] did, for the app to log once its logger is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Mesa's EGL set, and WebKit's GBM path switched off when `gbm_off`
    /// (an NVIDIA-only machine).
    Applied { mesa_vendor_file: PathBuf, gbm_off: bool },
    /// Nothing set, and why.
    Skipped(&'static str),
}

/// The decision, from the facts alone.
pub fn decide(f: &Facts) -> Outcome {
    if f.opted_out {
        return Outcome::Skipped("turned off with FLOWSTA_WEBVIEW_FIX=0");
    }
    if !f.nvidia_loaded {
        return Outcome::Skipped("the NVIDIA driver is not loaded");
    }
    if f.egl_vendor_set || f.disable_gbm_set {
        return Outcome::Skipped("the EGL or WebKit renderer variables are already set");
    }
    match &f.mesa_vendor_file {
        // GBM off alone does not avoid the crash, so without Mesa's vendor
        // file nothing is set.
        None => Outcome::Skipped("Mesa's EGL vendor file is not installed"),
        // Mesa's EGL alone aborts on an NVIDIA-only machine; beside another
        // GPU the GBM switch is what swaps the colors.
        Some(p) => Outcome::Applied { mesa_vendor_file: p.clone(), gbm_off: !f.other_gpu },
    }
}

static SET_HERE: OnceLock<Vec<&'static str>> = OnceLock::new();

/// The variables [`apply`] set in this process (empty when it set none or
/// was not called). Sidecar spawns remove them from the child.
pub fn set_here() -> &'static [&'static str] {
    SET_HERE.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Read the facts and set the pair when they call for it. Linux only; a
/// no-op elsewhere. Call before any thread starts.
pub fn apply() -> Outcome {
    #[cfg(target_os = "linux")]
    {
        use std::path::Path;
        let facts = Facts {
            nvidia_loaded: Path::new("/proc/driver/nvidia/version").exists() || Path::new("/sys/module/nvidia").exists(),
            other_gpu: other_gpu_present(),
            egl_vendor_set: std::env::var_os(EGL_VENDOR_VAR).is_some(),
            disable_gbm_set: std::env::var_os(DISABLE_GBM_VAR).is_some(),
            opted_out: std::env::var_os(OPT_OUT_VAR).as_deref() == Some(std::ffi::OsStr::new("0")),
            mesa_vendor_file: MESA_VENDOR_FILES.iter().map(PathBuf::from).find(|p| p.is_file()),
        };
        let outcome = decide(&facts);
        if let Outcome::Applied { mesa_vendor_file, gbm_off } = &outcome {
            std::env::set_var(EGL_VENDOR_VAR, mesa_vendor_file.as_os_str());
            if *gbm_off {
                std::env::set_var(DISABLE_GBM_VAR, "1");
                let _ = SET_HERE.set(vec![EGL_VENDOR_VAR, DISABLE_GBM_VAR]);
            } else {
                let _ = SET_HERE.set(vec![EGL_VENDOR_VAR]);
            }
        }
        outcome
    }
    #[cfg(not(target_os = "linux"))]
    {
        Outcome::Skipped("not Linux")
    }
}

/// Whether a render node is driven by something other than `nvidia`.
#[cfg(target_os = "linux")]
fn other_gpu_present() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else { return false };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("renderD")
            && std::fs::read_link(e.path().join("device/driver"))
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy() != "nvidia"))
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvidia() -> Facts {
        Facts { nvidia_loaded: true, mesa_vendor_file: Some(PathBuf::from(MESA_VENDOR_FILES[0])), ..Facts::default() }
    }

    #[test]
    fn nvidia_only_gets_the_pair() {
        assert_eq!(decide(&nvidia()), Outcome::Applied { mesa_vendor_file: PathBuf::from(MESA_VENDOR_FILES[0]), gbm_off: true });
    }

    #[test]
    fn beside_another_gpu_mesa_alone() {
        // The GBM switch beside another GPU swaps red and blue on X11.
        assert_eq!(
            decide(&Facts { other_gpu: true, ..nvidia() }),
            Outcome::Applied { mesa_vendor_file: PathBuf::from(MESA_VENDOR_FILES[0]), gbm_off: false }
        );
    }

    #[test]
    fn skips_without_the_nvidia_driver() {
        assert!(matches!(decide(&Facts { nvidia_loaded: false, ..nvidia() }), Outcome::Skipped(_)));
    }

    #[test]
    fn never_overrides_the_persons_own_settings() {
        assert!(matches!(decide(&Facts { egl_vendor_set: true, ..nvidia() }), Outcome::Skipped(_)));
        assert!(matches!(decide(&Facts { disable_gbm_set: true, ..nvidia() }), Outcome::Skipped(_)));
    }

    #[test]
    fn opt_out_wins() {
        assert!(matches!(decide(&Facts { opted_out: true, ..nvidia() }), Outcome::Skipped(_)));
    }

    #[test]
    fn never_gbm_off_alone() {
        // Without Mesa's vendor file nothing is set, not even GBM off.
        assert!(matches!(decide(&Facts { mesa_vendor_file: None, ..nvidia() }), Outcome::Skipped(_)));
    }

    #[test]
    fn nothing_recorded_when_not_applied() {
        assert!(set_here().is_empty());
    }
}
