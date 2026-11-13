//! Orphaned sidecars from an earlier launch.
//!
//! ProofPoll runs two child processes, `proofpoll-lair-keystore` and
//! `proofpoll-holochain`, and the conductor listens on a fixed local port.
//! When the app dies without running its exit path (macOS kills a running
//! app whose bundle was replaced under it; a crash; a forced quit; an
//! installer), the children live on, the conductor keeps the port, and the
//! next launch cannot start its own conductor. Seen on the 2026-09-27 Mac
//! drive of 0.4.0-beta.1: an old conductor whose data folder had just been
//! moved into a profile answered the port with "unable to open database
//! file" and every retry leaked one more key store.
//!
//! Linux ties the children to the parent with `PR_SET_PDEATHSIG`
//! (`flowsta-hc-lines`); macOS and Windows have no equivalent, so the fix is
//! to sweep at the NEXT launch, before anything of ours spawns: a sidecar
//! whose parent is gone, or whose parent is not a live ProofPoll, belongs to
//! nobody and is stopped. A sidecar whose parent is a live ProofPoll that is
//! not us belongs to a second running copy and is left alone (that copy owns
//! the port; our own conductor start reports it).
//!
//! Matching is by process NAME (the sidecar file names are ours alone),
//! never by port or directory, and every kill names one pid.

/// All four sidecars, both lines. With only the first two listed, an
/// orphaned 0.6 conductor kept port 4466 and the next launch refused with
/// "Another ProofPoll is already running".
const SIDECAR_STEMS: &[&str] = &[
    "proofpoll-holochain",
    "proofpoll-holochain-06",
    "proofpoll-lair-keystore",
    "proofpoll-lair-keystore-07",
];

/// Stop every orphaned ProofPoll sidecar (the rule and the sweep live in
/// `flowsta-hc-lines`). Returns how many were stopped. Call before the
/// first sidecar of this launch spawns.
pub fn reap_orphaned_sidecars() -> u32 {
    flowsta_hc_lines::reap::reap_orphaned_sidecars(SIDECAR_STEMS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowsta_hc_lines::reap::{is_sidecar_name, should_reap, Parent};

    #[test]
    fn only_our_sidecar_names_match_with_or_without_exe() {
        let ours = |name: &str| is_sidecar_name(name, SIDECAR_STEMS);
        assert!(ours("proofpoll-holochain"));
        assert!(ours("proofpoll-lair-keystore.exe"));
        assert!(ours("ProofPoll-Holochain.EXE"));
        assert!(ours("proofpoll-holochain-06"));
        assert!(ours("proofpoll-lair-keystore-07.exe"));
        assert!(!ours("holochain"));
        assert!(!ours("flowsta-vault-holochain"));
        assert!(!ours("proofpoll"));
        assert!(!ours("yourowai-lair-keystore"));
    }

    #[test]
    fn a_sidecar_is_reaped_only_when_no_live_proofpoll_holds_it() {
        assert!(should_reap("proofpoll-holochain", Parent::Gone, SIDECAR_STEMS));
        assert!(should_reap("proofpoll-lair-keystore", Parent::Foreign, SIDECAR_STEMS), "adopted by a subreaper = its app is gone");
        assert!(!should_reap("proofpoll-holochain", Parent::Us, SIDECAR_STEMS));
        assert!(!should_reap("holochain", Parent::Gone, SIDECAR_STEMS), "never anything but our own sidecars");
    }

    /// Live: a process NAMED like our sidecar, double-forked so its parent is
    /// gone, is stopped by the sweep; one we spawned ourselves is kept.
    /// Ignored because it stops any orphaned ProofPoll sidecar on the box
    /// (that is its job); run it by hand with nothing of ProofPoll running.
    #[test]
    #[ignore = "kills real orphaned sidecars on this machine"]
    #[cfg(unix)]
    fn live_sweep_stops_a_parentless_sidecar_and_keeps_our_own_child() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("proofpoll-holochain");
        std::fs::copy("/bin/sleep", &fake).unwrap();
        // orphan: sh spawns it in the background and exits
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("nohup {} 300 >/dev/null 2>&1 &", fake.display()))
            .status()
            .unwrap();
        // our own child
        let mut mine = std::process::Command::new(&fake).arg("300").spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let reaped = reap_orphaned_sidecars();
        assert!(reaped >= 1, "the parentless one is stopped ({} reaped)", reaped);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(mine.try_wait().unwrap().is_none(), "our own child is kept");
        let _ = mine.kill();
        let _ = mine.wait();
        let left = std::process::Command::new("pgrep").arg("-f").arg(fake.to_string_lossy().as_ref()).output().unwrap();
        assert!(String::from_utf8_lossy(&left.stdout).trim().is_empty(), "no fake sidecar left: {:?}", left);
    }
}

/// The test data folder a debug build was started with, if any.
fn test_data_dir() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("PROOFPOLL_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(dir.to_string_lossy().to_string());
    }
    None
}
