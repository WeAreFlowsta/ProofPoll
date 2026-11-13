//! Spawning the sidecar processes (lair-keystore, the holochain conductor)
//! tied to the app's lifetime and without a console window, and stopping
//! them cleanly. The code lives in `flowsta-hc-lines`.

pub use flowsta_hc_lines::process::*;

// The app's own stop calls, in the shape it had before the library: each
// asks the conductor to shut down (its ctrl-c) and ends it after `grace`.
// They go when the library's supervisor owns the processes.

/// Stop a conductor the way it expects, then end it if it has not stopped
/// within `grace`.
pub async fn stop_gracefully(child: &mut SidecarChild, grace: std::time::Duration, what: &str) {
    stop_gracefully_blocking(child, grace, what)
}

/// [`stop_gracefully`] for a thread that may block: the app's own exit.
pub fn stop_gracefully_blocking(child: &mut SidecarChild, grace: std::time::Duration, what: &str) {
    match flowsta_hc_lines::process::stop_gracefully(child, StopRequest::Interrupt, grace) {
        Ok(StopOutcome::AlreadyExited) => {}
        Ok(outcome) => log::info!("{} stopped ({:?})", what, outcome),
        Err(e) => log::warn!("{} not stopped cleanly: {}", what, e),
    }
}

/// Terminate a process by PID, platform-correct (Windows has no `kill`:
/// taskkill with /T so the child tree goes too).
pub fn stop_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(CREATE_NO_WINDOW).output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill").arg(pid.to_string()).output();
    }
}
