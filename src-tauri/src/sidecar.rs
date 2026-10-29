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
/// - **Release**: returns `<exe_dir>/<name>` (`<name>.exe` on Windows) — Tauri's bundlers
///   bundlers install externalBin contents next to the main executable.
///   Falls back to a bare name (PATH lookup) if not found there.
/// Whether a file-name suffix is a target triple (`x86_64-unknown-linux-gnu`,
/// `aarch64-apple-darwin`, ...), not another sidecar's `06-`/`07-` suffix.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn is_target_triple(rest: &str) -> bool {
    matches!(rest.split('-').next(), Some("x86_64" | "aarch64" | "i686" | "armv7"))
}

pub fn sidecar_path(name: &str) -> PathBuf {
    #[cfg(debug_assertions)]
    {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let binaries = std::path::Path::new(manifest_dir).join("binaries");
        let prefix = format!("{}-", name);
        if let Ok(entries) = std::fs::read_dir(&binaries) {
            for entry in entries.flatten() {
                if let Some(s) = entry.file_name().to_str() {
                    // `<name>-<triple>` only: `proofpoll-holochain-` is also
                    // the start of `proofpoll-holochain-06-<triple>`, and which
                    // one matched depended on directory order.
                    if s.strip_prefix(&prefix).map(is_target_triple).unwrap_or(false) {
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
                let candidate = if cfg!(target_os = "windows") {
                    parent.join(format!("{}.exe", name))
                } else {
                    parent.join(name)
                };
                if candidate.exists() {
                    return candidate;
                }
            }
        }
        PathBuf::from(name)
    }
}

#[cfg(test)]
mod tests {
    use super::is_target_triple;

    #[test]
    fn only_a_target_triple_follows_the_name() {
        assert!(is_target_triple("x86_64-unknown-linux-gnu"));
        assert!(is_target_triple("aarch64-apple-darwin"));
        assert!(!is_target_triple("06-x86_64-unknown-linux-gnu"), "proofpoll-holochain must not match the 0.6 binary");
        assert!(!is_target_triple("07-aarch64-apple-darwin"), "proofpoll-lair-keystore must not match the 0.7 binary");
    }
}
