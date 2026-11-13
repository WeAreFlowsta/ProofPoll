//! Resolve the on-disk path of a bundled Tauri sidecar binary.
//!
//! Sidecars are prefixed with `proofpoll-` so they don't collide with
//! identically-named binaries shipped by other Tauri/Holochain apps.
//! The resolution itself lives in `flowsta-hc-lines`: `src-tauri/binaries`
//! in a debug build, next to the executable in a release, the bare name
//! (a PATH lookup) when neither has it.

use std::path::PathBuf;

pub fn sidecar_path(name: &str) -> PathBuf {
    flowsta_hc_lines::resolve_sidecar(name, &crate::kits::sidecar_source()).unwrap_or_else(|_| PathBuf::from(name))
}
