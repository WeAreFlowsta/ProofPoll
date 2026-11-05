//! The kit cache (flowsta-hc-lines `kit`): once a run, after the conductors
//! are up, each line's kit (its conductor, its key store and the hApp it
//! loads) is set aside compressed in `<data dir>/kits/`. A later ProofPoll
//! that stops bundling the 0.6 line reads a device's own 0.6 chain from it,
//! offline, after checking every file against the hashes it compiles in.
//! 0.6.0 keeps both lines, so both kits are cached.

use flowsta_hc_lines::kit::{KitBinary, KitCache, KitDescriptor, KitFile};
use flowsta_hc_lines::Line;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The 0.6.1 line's kit. `accepted` stays empty while a release bundles the
/// binaries: their post-signing hashes come out of this release's build and
/// go into the next one.
pub const KIT_06: KitDescriptor = KitDescriptor {
    line: Line::V06,
    conductor: KitBinary { sidecar: "proofpoll-holochain-06", version: "0.6.1", accepted: &[] },
    lair: KitBinary { sidecar: "proofpoll-lair-keystore", version: "0.6.3", accepted: &[] },
    files: &[KitFile { name: "proofpoll_v1_3_happ.happ", sha256: "7cac687ab376a5dbdad453a8b77768edec30c5bd656adb57f7dad5ad69eeb197" }],
};

/// The 0.7.1 line's kit.
pub const KIT_07: KitDescriptor = KitDescriptor {
    line: Line::V07,
    conductor: KitBinary { sidecar: "proofpoll-holochain", version: "0.7.1", accepted: &[] },
    lair: KitBinary { sidecar: "proofpoll-lair-keystore-07", version: "0.7.1", accepted: &[] },
    files: &[
        KitFile { name: "proofpoll_v1_4_happ.happ", sha256: "160f768b90a5e5ae27252af472ea95166782c0949f9df137ad3fd96507928dac" },
        KitFile { name: "proofpoll_v1_5_happ.happ", sha256: "7600fdd4747b6f0d596d29f92002e137a1eba10dced0b060ab5c402ea2230ea0" },
    ],
};

/// Where the sidecars are: next to the app, or `src-tauri/binaries` in a
/// debug build.
pub fn sidecar_source() -> flowsta_hc_lines::SidecarSource {
    flowsta_hc_lines::SidecarSource {
        dev_dir: cfg!(debug_assertions).then(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries")),
        exe_dir: None,
        kit_dirs: Vec::new(),
        allow_path_lookup: true,
    }
}

pub fn kits_dir(device_root: &Path) -> PathBuf {
    device_root.join("kits")
}

static REFRESHED: AtomicBool = AtomicBool::new(false);

/// `refresh`, once a run, on a blocking thread.
pub fn refresh_once(device_root: PathBuf, resources: PathBuf) {
    if REFRESHED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || refresh(&device_root, &resources));
}

pub fn refresh(device_root: &Path, resources: &Path) {
    let cache = KitCache::new(kits_dir(device_root));
    let sidecars = sidecar_source();
    for kit in [&KIT_07, &KIT_06] {
        match cache.put(kit, &sidecars, resources) {
            Ok(p) if p.written.is_empty() => log::debug!("[kit] the {} kit is cached as it is ({} KB)", p.line, p.packed / 1024),
            Ok(p) => log::info!("[kit] the {} kit cached: {} written, {} as before ({} KB)", p.line, p.written.join(", "), p.kept, p.packed / 1024),
            Err(e) => log::warn!("[kit] the {} kit not cached: {}", kit.line, e),
        }
    }
}

/// Each line's kit by the cache's own record (for the harness).
#[cfg(feature = "harness")]
pub fn status(device_root: &Path) -> serde_json::Value {
    let cache = KitCache::new(kits_dir(device_root));
    let line = |l: Line| match cache.manifest(l) {
        Some(m) => serde_json::json!({ "entries": m.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(), "packed": m.packed_bytes(), "target": m.target }),
        None => serde_json::Value::Null,
    };
    serde_json::json!({ "holochain-0.6": line(Line::V06), "holochain-0.7": line(Line::V07) })
}
