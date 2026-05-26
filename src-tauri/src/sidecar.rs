//! Resolve the on-disk path of a bundled Tauri sidecar binary.
//!
//! Sidecars are prefixed with `proofpoll-` so they don't collide with
//! identically-named binaries shipped by other Tauri/Holochain apps.
//! See <https://v2.tauri.app/develop/sidecar/>.

use std::path::PathBuf;

/// Resolve the path of a sidecar binary by its declared name in
/// `tauri.conf.json` (e.g. `"proofpoll-lair-keystore"`).
///
/// - **Dev (`cargo tauri dev`)**: looks for `<manifest>/binaries/<name>-<triple>`.
///   This mirrors how Tauri resolves externalBin in dev mode but uses a
///   prefix match so we don't have to hard-code the target triple.
/// - **Release**: returns `<exe_dir>/<name>` — Tauri's debian/macOS
///   bundlers install externalBin contents next to the main executable.
///   Falls back to a bare name (PATH lookup) if not found there.
pub fn sidecar_path(name: &str) -> PathBuf {
    #[cfg(debug_assertions)]
    {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let binaries = std::path::Path::new(manifest_dir).join("binaries");
        let prefix = format!("{}-", name);
        if let Ok(entries) = std::fs::read_dir(&binaries) {
            for entry in entries.flatten() {
                if let Some(s) = entry.file_name().to_str() {
                    if s.starts_with(&prefix) {
                        return entry.path();
                    }
                }
            }
        }
        return PathBuf::from(name);
    }

    #[allow(unreachable_code)]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                let candidate = parent.join(name);
                if candidate.exists() {
                    return candidate;
                }
            }
        }
        PathBuf::from(name)
    }
}
