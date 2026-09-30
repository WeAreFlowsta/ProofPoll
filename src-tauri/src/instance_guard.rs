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
//! (`process_ext.rs`); macOS and Windows have no equivalent, so the fix is
//! to sweep at the NEXT launch, before anything of ours spawns: a sidecar
//! whose parent is gone, or whose parent is not a live ProofPoll, belongs to
//! nobody and is stopped. A sidecar whose parent is a live ProofPoll that is
//! not us belongs to a second running copy and is left alone (that copy owns
//! the port; our own conductor start reports it).
//!
//! Matching is by process NAME (the sidecar file names are ours alone),
//! never by port or directory, and every kill names one pid.

const SIDECAR_STEMS: &[&str] = &["proofpoll-holochain", "proofpoll-lair-keystore"];

/// What we know about a candidate process's parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Parent {
    /// The parent pid is not in the process table (reparented to init /
    /// launchd, or Windows reused the id for nothing we can see).
    Gone,
    /// The parent is this very process: our own child (spawned already).
    Us,
    /// The parent is another live ProofPoll main process.
    OtherProofPoll,
    /// The parent is some live process that is not ProofPoll.
    Foreign,
    /// The parent is itself one of our sidecars: a thread of a running
    /// sidecar (Linux lists every thread as a process) or its own child.
    /// Part of something alive - never stopped.
    Sidecar,
}

/// The process name without a Windows extension, lower-case.
pub(crate) fn stem_of(name: &str) -> String {
    let n = name.to_lowercase();
    n.strip_suffix(".exe").map(str::to_string).unwrap_or(n)
}

pub(crate) fn is_sidecar_name(name: &str) -> bool {
    SIDECAR_STEMS.contains(&stem_of(name).as_str())
}

/// The one decision: a sidecar is reaped when nothing of ours is holding
/// it. `Foreign` covers a sidecar adopted by a subreaper (systemd user
/// session, a terminal that ran the app): its ProofPoll is gone all the
/// same, so it is reaped too. Only a live ProofPoll parent keeps it.
pub(crate) fn should_reap(name: &str, parent: Parent) -> bool {
    is_sidecar_name(name) && matches!(parent, Parent::Gone | Parent::Foreign)
}

/// Stop every orphaned ProofPoll sidecar. Returns how many were stopped.
/// Call before the first sidecar of this launch spawns.
pub fn reap_orphaned_sidecars() -> u32 {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

    let me = sysinfo::Pid::from_u32(std::process::id());
    let my_exe = std::env::current_exe().ok();
    let my_name = my_exe
        .as_ref()
        .and_then(|p| p.file_name().map(|n| stem_of(&n.to_string_lossy())));

    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    );
    let procs = sys.processes();
    // The kernel's short process name is cut to 15 (Linux) / 16 (macOS)
    // bytes - "proofpoll-lair-keystore" never fits - so the executable's
    // file name is the identity, the short name only a fallback (Windows
    // names are complete).
    let full_name = |p: &sysinfo::Process| -> String {
        match p.exe().and_then(|e| e.file_name()) {
            Some(n) => n.to_string_lossy().to_string(),
            None => p.name().to_string_lossy().to_string(),
        }
    };
    let is_proofpoll = |pid: &sysinfo::Pid| -> bool {
        procs.get(pid).map(|p| {
            match (p.exe(), my_exe.as_deref()) {
                (Some(e), Some(mine)) => e == mine,
                _ => Some(stem_of(&full_name(p))) == my_name,
            }
        }).unwrap_or(false)
    };

    let mut reaped = 0u32;
    for (pid, proc_) in procs {
        if *pid == me {
            continue;
        }
        let name = full_name(proc_);
        if !is_sidecar_name(&name) {
            continue;
        }
        // Linux lists each thread as a process; stopping a thread's id stops
        // its whole process, which may be a live sidecar of another app copy.
        if proc_.thread_kind().is_some() {
            continue;
        }
        let parent = match proc_.parent() {
            None => Parent::Gone,
            Some(pp) if pp == me => Parent::Us,
            Some(pp) if !procs.contains_key(&pp) => Parent::Gone,
            Some(pp) if procs.get(&pp).map(|q| is_sidecar_name(&full_name(q))).unwrap_or(false) => Parent::Sidecar,
            Some(pp) if is_proofpoll(&pp) => Parent::OtherProofPoll,
            Some(_) => Parent::Foreign,
        };
        if should_reap(&name, parent) {
            log::warn!(
                "[instance] stopping orphaned sidecar {} (pid {}, parent {:?})",
                name,
                pid,
                parent
            );
            if proc_.kill() {
                reaped += 1;
            } else {
                log::warn!("[instance] could not stop pid {}", pid);
            }
        } else if parent == Parent::OtherProofPoll {
            log::warn!(
                "[instance] sidecar {} (pid {}) belongs to another running ProofPoll - left alone",
                name,
                pid
            );
        }
    }
    if reaped > 0 {
        // Give the kernel a beat to release the conductor port.
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    reaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_sidecar_names_match_with_or_without_exe() {
        assert!(is_sidecar_name("proofpoll-holochain"));
        assert!(is_sidecar_name("proofpoll-lair-keystore.exe"));
        assert!(is_sidecar_name("ProofPoll-Holochain.EXE"));
        assert!(!is_sidecar_name("holochain"));
        assert!(!is_sidecar_name("flowsta-vault-holochain"));
        assert!(!is_sidecar_name("proofpoll"));
        assert!(!is_sidecar_name("yourowai-lair-keystore"));
    }

    #[test]
    fn a_sidecar_is_reaped_only_when_no_live_proofpoll_holds_it() {
        assert!(should_reap("proofpoll-holochain", Parent::Gone));
        assert!(should_reap("proofpoll-lair-keystore", Parent::Foreign), "adopted by a subreaper = its app is gone");
        assert!(!should_reap("proofpoll-holochain", Parent::Us));
        assert!(!should_reap("proofpoll-holochain", Parent::Sidecar), "a thread of a live sidecar is part of it");
        assert!(!should_reap("proofpoll-holochain", Parent::OtherProofPoll), "a second running copy keeps its children");
        assert!(!should_reap("holochain", Parent::Gone), "never anything but our own sidecars");
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
