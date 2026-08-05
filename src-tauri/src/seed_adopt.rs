//! Adopt / re-key: replace this install's agent seed with verified ordering.
//!
//! Two flows share one engine:
//!   - **Adopt** (machine-death restore): commit the seed escrowed in the
//!     user's Vault backup so this device continues authoring as the SAME
//!     agent as the lost machine.
//!   - **Re-key** (legacy installs): agents created inside lair before the
//!     app-held seed existed cannot be escrowed; a one-time, user-approved
//!     re-key moves the install onto an escrowable seed for all future
//!     authorship.
//!
//! The ordering is the whole point (the YOAI v0.1.1-beta.4 lesson, proven on
//! the 2026-08-04 Windows recovery drive): platform-correct process stop →
//! wipe the key-derived state with retries → VERIFY the wipe → only then
//! commit the new seed - aborting with nothing changed if the wipe cannot
//! complete. Committing first relaunches into a half-wiped hybrid where lair
//! cannot start and nothing works.
//!
//! This module also carries the backup escrow gate: the canonical backup
//! REFUSES to overwrite a Vault backup whose escrowed seed differs from this
//! device's - otherwise a fresh install's first hourly backup would destroy
//! the very seed the user needs to adopt.
//!
//! Effects live behind small traits (`AdoptSeams`, `EscrowProbe`) so the
//! ordering and the gate decisions are unit-tested - same probe-seam pattern
//! as Your Own AI's `GateProbes`.

use crate::commands::AppState;
use crate::device_seed::{self, DeviceSeed};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything derived from the outgoing key, wiped before the new seed is
/// committed. `proofpoll-recovery.json` is deliberately NOT here (it is the
/// commit target, replaced with the old file preserved), and neither are
/// identity-link.json / profile-cache.json (they fall in the binding swap,
/// after the wipe is verified - so an aborted adopt leaves a sign-in the
/// layout's self-heal can repair).
pub(crate) const WIPE_TARGETS: &[&str] = &["conductor", "lair", "lair-passphrase"];
/// 20 attempts x 500 ms = 10 s per target for Windows to release file locks.
pub(crate) const WIPE_ATTEMPTS: usize = 20;
pub(crate) const SETTLE_MS: u64 = 1500;
const WIPE_RETRY_MS: u64 = 500;

/// Written when an adopt/re-key commits; tells the post-restart UI to walk
/// the user through step 2 (sign back in). Cleared by `commit_identity_link`.
pub const RELINK_MARKER: &str = "adopt-relink-pending";

pub(crate) fn relink_marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RELINK_MARKER)
}

pub(crate) fn clear_relink_marker(data_dir: &Path) {
    let _ = std::fs::remove_file(relink_marker_path(data_dir));
}

// ── The adopt engine (ordering under test) ─────────────────────────────

pub(crate) trait AdoptSeams {
    /// Revoke this install's own identity-link entry while the conductor can
    /// still sign as the outgoing agent (the superseded link is revoked as
    /// active but retained as history - delete_entry tombstones, it never
    /// erases).
    async fn revoke_outgoing_link(&self) -> Result<(), String>;
    /// Stop conductor + lair, platform-correct (`taskkill /T /F` on Windows -
    /// plain `kill` is a silent no-op there).
    async fn stop_processes(&self);
    async fn sleep_ms(&self, ms: u64);
    fn target_exists(&self, target: &str) -> bool;
    fn remove_target(&self, target: &str) -> Result<(), String>;
    /// Commit the incoming seed as the recovery file (old file preserved
    /// with a timestamp).
    fn commit_seed(&self, seed: &DeviceSeed) -> Result<(), String>;
    /// Drop the outgoing identity binding (identity-link.json +
    /// profile-cache.json) and leave the step-2 relink marker.
    fn finish_binding_swap(&self);
}

pub(crate) async fn run_adopt<S: AdoptSeams>(
    seams: &S,
    incoming: &DeviceSeed,
    local: Option<&DeviceSeed>,
) -> Result<(), String> {
    // Validate before touching anything.
    incoming.seed_bytes()?;
    if local == Some(incoming) {
        return Err("This device already uses that key - nothing to restore.".into());
    }

    // 1. Supersede the outgoing agent's link while it can still sign the
    //    revocation. Best-effort: a failed revocation leaves an orphaned
    //    link entry (recoverable, known-deferred cleanup); blocking a
    //    machine-death restore on it is worse.
    if let Err(e) = seams.revoke_outgoing_link().await {
        log::warn!("[adopt] could not revoke the outgoing identity link: {}", e);
    }

    // 2. Stop conductor + lair so their databases aren't held open, then
    //    give the OS a beat - Windows releases file locks a moment after
    //    the processes die.
    seams.stop_processes().await;
    seams.sleep_ms(SETTLE_MS).await;

    // 3. Wipe the key-derived state BEFORE committing the new seed, retrying
    //    while locks release and VERIFYING each removal - remove_dir_all can
    //    report success on Windows while children are still pending-delete.
    //    On persistent failure abort with the old seed and sign-in intact.
    for target in WIPE_TARGETS {
        let mut removed = false;
        let mut last_err = String::from("still present");
        for _ in 0..WIPE_ATTEMPTS {
            if !seams.target_exists(target) {
                removed = true;
                break;
            }
            match seams.remove_target(target) {
                Ok(()) => {
                    if !seams.target_exists(target) {
                        removed = true;
                        break;
                    }
                    last_err = "removal reported success but the target is still present".into();
                }
                Err(e) => last_err = e,
            }
            seams.sleep_ms(WIPE_RETRY_MS).await;
        }
        if !removed {
            return Err(format!(
                "Could not release this device's Holochain state ({}: {}). \
                 Close and reopen ProofPoll, then try again - your current key \
                 and sign-in are unchanged.",
                target, last_err
            ));
        }
    }

    // 4. Only now commit the new seed, then swap the identity binding and
    //    leave the step-2 marker for the post-restart sign-in prompt.
    seams.commit_seed(incoming)?;
    seams.finish_binding_swap();
    Ok(())
}

// ── The backup escrow gate (decisions under test) ──────────────────────

/// What the Vault's backup slot holds, seed-wise.
pub(crate) enum EscrowedSeed {
    /// A seed is escrowed (hex string as stored - validity is the
    /// comparison's problem, and an invalid stored seed can never equal a
    /// valid local one, so it refuses).
    Seed(String),
    /// No backup stored at all - an empty slot is safe to write into.
    NoBackup,
    /// A backup exists but carries no adoptable seed (a legacy install's
    /// explicit null, or a pre-seed backup format). Data-only backups were
    /// always overwritten hourly; nothing key-shaped is at risk.
    DataOnly,
}

pub(crate) trait EscrowProbe {
    async fn escrowed_seed(&self) -> Result<EscrowedSeed, String>;
}

/// Refuse a canonical-backup write whenever it would destroy an escrowed
/// seed that is not this device's. This is what keeps the adopt window
/// safe: a fresh install links, its hourly backup fires, and without this
/// gate that backup would replace the lost machine's escrowed seed before
/// the user ever saw the restore offer.
pub(crate) async fn backup_escrow_gate<P: EscrowProbe>(
    probe: &P,
    local: Result<Option<DeviceSeed>, String>,
) -> Result<(), String> {
    let escrowed = probe
        .escrowed_seed()
        .await
        .map_err(|e| format!("backup held: could not check the Vault's escrowed key first ({}) - when in doubt, never write", e))?;
    match escrowed {
        EscrowedSeed::NoBackup | EscrowedSeed::DataOnly => Ok(()),
        EscrowedSeed::Seed(theirs) => match local {
            Ok(Some(mine)) if mine.device_seed_hex == theirs => Ok(()),
            Ok(Some(_)) => Err(
                "backup held: your Vault backup escrows a different authorship key than this \
                 device's. Restore the key from the Identity page first - overwriting would \
                 destroy it."
                    .into(),
            ),
            Ok(None) => Err(
                "backup held: your Vault backup escrows an authorship key this install does \
                 not have. Restore the key from the Identity page first - overwriting would \
                 destroy it."
                    .into(),
            ),
            Err(e) => Err(format!(
                "backup held: this device's recovery file is unreadable ({}) - refusing to \
                 overwrite the escrowed key",
                e
            )),
        },
    }
}

/// Classify a retrieved canonical-backup payload's `app_keys` block.
/// A payload that isn't even an object is an error, never "safe to
/// overwrite" - when in doubt, never write.
pub(crate) fn classify_escrow_payload(data: &serde_json::Value) -> Result<EscrowedSeed, String> {
    if !data.is_object() {
        return Err("escrow_unreadable: backup payload is not an object".into());
    }
    match data.get("app_keys") {
        None => Ok(EscrowedSeed::DataOnly),
        Some(keys) => match keys.get("device_seed_hex") {
            Some(serde_json::Value::String(s)) => Ok(EscrowedSeed::Seed(s.clone())),
            Some(serde_json::Value::Null) => Ok(EscrowedSeed::DataOnly),
            _ => Err("escrow_unreadable: app_keys carries no recognisable seed".into()),
        },
    }
}

// ── Production implementations ─────────────────────────────────────────

/// The Origin header the Vault's bridge maps to this app's client_id. The
/// linked-apps registry records the origin the WEBVIEW presented during the
/// link ceremony, so a Rust-side request must present the same one (a bare
/// reqwest carries none and is refused - the /revoke-identity lesson).
#[cfg(debug_assertions)]
const WEBVIEW_ORIGIN: &str = "http://localhost:5174"; // devUrl in tauri.conf.json
#[cfg(all(not(debug_assertions), target_os = "windows"))]
const WEBVIEW_ORIGIN: &str = "http://tauri.localhost";
#[cfg(all(not(debug_assertions), not(target_os = "windows")))]
const WEBVIEW_ORIGIN: &str = "tauri://localhost";

pub(crate) struct VaultEscrowProbe {
    pub client_id: String,
}

impl EscrowProbe for VaultEscrowProbe {
    async fn escrowed_seed(&self) -> Result<EscrowedSeed, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post("http://127.0.0.1:27777/backup/retrieve")
            .header("Origin", WEBVIEW_ORIGIN)
            .json(&serde_json::json!({ "client_id": self.client_id }))
            .send()
            .await
            .map_err(|e| format!("vault unreachable: {}", e))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(EscrowedSeed::NoBackup);
        }
        if !resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            return Err(body["error"]
                .as_str()
                .unwrap_or("retrieve_failed")
                .to_string());
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("bad retrieve response: {}", e))?;
        classify_escrow_payload(&v["data"])
    }
}

struct AppSeams {
    state: Arc<AppState>,
}

impl AdoptSeams for AppSeams {
    async fn revoke_outgoing_link(&self) -> Result<(), String> {
        let Some(link) = crate::commands::load_identity_link(&self.state.data_dir) else {
            return Ok(()); // nothing bound - nothing to supersede
        };
        let hash = holochain_types::prelude::ActionHash::try_from(link.entry_action_hash)
            .map_err(|e| format!("stored link hash unparseable: {:?}", e))?;
        let client = self.state.app_client.lock().await;
        let client = client.as_ref().ok_or("conductor not ready")?;
        let payload =
            holochain_types::prelude::ExternIO::encode(hash).map_err(|e| e.to_string())?;
        crate::commands::call_zome(client, crate::commands::AGENT_LINKING_ZOME, "revoke_link", payload)
            .await?;
        Ok(())
    }

    async fn stop_processes(&self) {
        let handle = self.state.conductor_handle.lock().unwrap().take();
        match handle {
            Some(h) => {
                crate::process_ext::stop_pid(h.conductor_child.id());
                crate::process_ext::stop_pid(h.lair_child.id());
                log::info!("[adopt] signalled conductor + lair to stop");
            }
            None => log::warn!("[adopt] no conductor handle held - nothing to stop"),
        }
    }

    async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    fn target_exists(&self, target: &str) -> bool {
        self.state.data_dir.join(target).exists()
    }

    fn remove_target(&self, target: &str) -> Result<(), String> {
        let p = self.state.data_dir.join(target);
        let res = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        match res {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn commit_seed(&self, seed: &DeviceSeed) -> Result<(), String> {
        device_seed::replace(&self.state.data_dir, seed)
    }

    fn finish_binding_swap(&self) {
        let dir = &self.state.data_dir;
        let _ = std::fs::remove_file(dir.join("identity-link.json"));
        let _ = std::fs::remove_file(dir.join("profile-cache.json"));
        if let Err(e) = std::fs::write(relink_marker_path(dir), b"") {
            log::warn!("[adopt] could not write the relink marker: {}", e);
        }
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct SeedEscrowReport {
    /// "synced" | "conflict" | "legacy" | "local_unreadable"
    pub state: String,
    pub local_seed_present: bool,
    /// An adopt/re-key committed and the user hasn't signed back in yet
    /// (step 2 of the restore story).
    pub relink_pending: bool,
}

/// Compare the seed escrowed in the Vault backup (fetched by the WEBVIEW,
/// whose origin the Vault authorizes) against this install's. Pass `None`
/// when the backup is absent or carries no seed.
#[tauri::command]
pub fn get_seed_escrow_state(
    state: tauri::State<'_, Arc<AppState>>,
    escrowed_seed_hex: Option<String>,
) -> Result<SeedEscrowReport, String> {
    if let Some(ref hex_str) = escrowed_seed_hex {
        let probe = DeviceSeed { device_seed_hex: hex_str.clone(), version: 1 };
        probe
            .seed_bytes()
            .map_err(|e| format!("escrowed seed unreadable: {}", e))?;
    }
    let relink_pending = relink_marker_path(&state.data_dir).exists();
    let (st, present) = match (device_seed::load(&state.data_dir), &escrowed_seed_hex) {
        (Ok(Some(local)), Some(theirs)) if &local.device_seed_hex == theirs => ("synced", true),
        (Ok(Some(_)), Some(_)) => ("conflict", true),
        // Local seed with nothing escrowed yet: the next hourly backup
        // carries it - synced from the user's point of view.
        (Ok(Some(_)), None) => ("synced", true),
        // No local seed while the Vault escrows one: the restore case the
        // spec names ("local absent with escrow present") - adoptable.
        (Ok(None), Some(_)) => ("conflict", false),
        (Ok(None), None) => ("legacy", false),
        // Corrupt local file + an escrowed seed: adoption is the repair
        // (the unreadable file is preserved, never silently regenerated).
        (Err(e), Some(_)) => {
            log::warn!("[adopt] local recovery file unreadable: {}", e);
            ("conflict", false)
        }
        (Err(e), None) => {
            log::warn!("[adopt] local recovery file unreadable: {}", e);
            ("local_unreadable", false)
        }
    };
    Ok(SeedEscrowReport {
        state: st.into(),
        local_seed_present: present,
        relink_pending,
    })
}

/// Step 1 of the restore story: adopt the seed escrowed in the user's Vault
/// backup, so this device continues as the SAME agent. Restarts the app on
/// success; aborts with nothing changed if the wipe cannot complete.
#[tauri::command]
pub async fn adopt_escrowed_seed(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    seed_hex: String,
    accept_data_loss: bool,
) -> Result<(), String> {
    // No silent adoption path: every caller acknowledges that this install's
    // drafts/private notes die with its outgoing key.
    if !accept_data_loss {
        return Err("adoption must be explicitly confirmed".into());
    }
    // The seed being adopted came from the linked identity's Vault slot -
    // require that identity to be present and confirmed, like every other
    // identity-bearing operation.
    crate::commands::require_identity_match(&state).await?;

    let incoming = DeviceSeed { device_seed_hex: seed_hex, version: 1 };
    incoming
        .seed_bytes()
        .map_err(|e| format!("escrowed seed unreadable: {}", e))?;
    let local = match device_seed::load(&state.data_dir) {
        Ok(o) => o,
        Err(e) => {
            log::warn!(
                "[adopt] local recovery file unreadable ({}) - it will be preserved and replaced",
                e
            );
            None
        }
    };

    let seams = AppSeams { state: state.inner().clone() };
    run_adopt(&seams, &incoming, local.as_ref()).await?;
    log::info!("[adopt] escrowed seed committed - relaunching");
    restart_app(app)
}

/// The one-time re-key for installs whose agent predates the app-held seed:
/// moves future authorship onto an escrowable seed. Published records stay
/// on the network under the old key (its link is revoked as active but
/// retained as history). Never forced - refuses without explicit approval.
#[tauri::command]
pub async fn rekey_device_agent(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    accept_new_key: bool,
) -> Result<(), String> {
    if !accept_new_key {
        return Err("re-keying must be explicitly confirmed".into());
    }
    crate::commands::require_identity_match(&state).await?;
    match device_seed::load(&state.data_dir) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(
                "this install already has an escrowable seed - re-keying is only for installs \
                 that predate it"
                    .into(),
            )
        }
        Err(e) => {
            return Err(format!(
                "recovery file unreadable ({}) - not generating a new key over it",
                e
            ))
        }
    }

    let incoming = device_seed::generate()?;
    let seams = AppSeams { state: state.inner().clone() };
    run_adopt(&seams, &incoming, None).await?;
    log::info!("[rekey] new escrowable seed committed - relaunching");
    restart_app(app)
}

fn restart_app(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(not(debug_assertions))]
    {
        app.restart()
    }
    #[cfg(debug_assertions)]
    {
        log::info!("[adopt] dev build - exiting; re-run `npm run tauri dev` to relaunch");
        app.exit(0);
        Ok(())
    }
}

// ── Tests (the GateProbes pattern: scripted seams + ordered call logs) ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashSet, VecDeque};

    fn seed(byte: &str) -> DeviceSeed {
        DeviceSeed { device_seed_hex: byte.repeat(32), version: 1 }
    }

    struct Scripted {
        /// Targets currently "on disk".
        present: RefCell<HashSet<String>>,
        /// Scripted outcome per remove_target call (one shared queue, in call
        /// order); exhausted queue defaults to Ok.
        removals: RefCell<VecDeque<Result<(), String>>>,
        /// When false, a successful removal does NOT clear `present` - the
        /// lying-remove case (Windows remove_dir_all returning Ok with
        /// children still pending-delete).
        honest_removals: bool,
        revoke: RefCell<VecDeque<Result<(), String>>>,
        commit: RefCell<VecDeque<Result<(), String>>>,
        calls: RefCell<Vec<String>>,
        retry_sleeps: RefCell<usize>,
    }

    impl Scripted {
        fn new(present: &[&str]) -> Self {
            Self {
                present: RefCell::new(present.iter().map(|s| s.to_string()).collect()),
                removals: RefCell::new(VecDeque::new()),
                honest_removals: true,
                revoke: RefCell::new(VecDeque::new()),
                commit: RefCell::new(VecDeque::new()),
                calls: RefCell::new(Vec::new()),
                retry_sleeps: RefCell::new(0),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl AdoptSeams for Scripted {
        async fn revoke_outgoing_link(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("revoke".into());
            self.revoke.borrow_mut().pop_front().unwrap_or(Ok(()))
        }
        async fn stop_processes(&self) {
            self.calls.borrow_mut().push("stop".into());
        }
        async fn sleep_ms(&self, ms: u64) {
            if ms == SETTLE_MS {
                self.calls.borrow_mut().push("settle".into());
            } else {
                *self.retry_sleeps.borrow_mut() += 1;
            }
        }
        fn target_exists(&self, target: &str) -> bool {
            self.present.borrow().contains(target)
        }
        fn remove_target(&self, target: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("remove:{}", target));
            let r = self.removals.borrow_mut().pop_front().unwrap_or(Ok(()));
            if r.is_ok() && self.honest_removals {
                self.present.borrow_mut().remove(target);
            }
            r
        }
        fn commit_seed(&self, _seed: &DeviceSeed) -> Result<(), String> {
            self.calls.borrow_mut().push("commit".into());
            self.commit.borrow_mut().pop_front().unwrap_or(Ok(()))
        }
        fn finish_binding_swap(&self) {
            self.calls.borrow_mut().push("swap".into());
        }
    }

    #[tokio::test]
    async fn happy_path_order_is_revoke_stop_settle_wipe_commit_swap() {
        let s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        run_adopt(&s, &seed("aa"), Some(&seed("bb"))).await.unwrap();
        assert_eq!(
            s.calls(),
            vec![
                "revoke",
                "stop",
                "settle",
                "remove:conductor",
                "remove:lair",
                "remove:lair-passphrase",
                "commit",
                "swap",
            ]
        );
    }

    #[tokio::test]
    async fn same_seed_refuses_before_touching_anything() {
        let s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        let err = run_adopt(&s, &seed("aa"), Some(&seed("aa"))).await.unwrap_err();
        assert!(err.contains("nothing to restore"), "{}", err);
        assert!(s.calls().is_empty(), "must not touch anything: {:?}", s.calls());
    }

    #[tokio::test]
    async fn invalid_incoming_seed_refuses_before_touching_anything() {
        let s = Scripted::new(&["conductor"]);
        let bad = DeviceSeed { device_seed_hex: "not-hex".into(), version: 1 };
        assert!(run_adopt(&s, &bad, None).await.is_err());
        assert!(s.calls().is_empty());
    }

    #[tokio::test]
    async fn revoke_failure_is_nonfatal_and_order_continues() {
        let s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        s.revoke.borrow_mut().push_back(Err("conductor not ready".into()));
        run_adopt(&s, &seed("aa"), None).await.unwrap();
        let calls = s.calls();
        assert_eq!(calls.first().map(String::as_str), Some("revoke"));
        assert!(calls.contains(&"commit".to_string()));
        assert!(calls.contains(&"swap".to_string()));
    }

    #[tokio::test]
    async fn locked_target_retries_until_the_lock_releases() {
        let s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        s.removals.borrow_mut().push_back(Err("locked".into()));
        s.removals.borrow_mut().push_back(Err("locked".into()));
        // Third attempt (default Ok) succeeds; later targets default Ok too.
        run_adopt(&s, &seed("aa"), None).await.unwrap();
        let calls = s.calls();
        let conductor_removes = calls.iter().filter(|c| *c == "remove:conductor").count();
        assert_eq!(conductor_removes, 3, "{:?}", calls);
        assert!(*s.retry_sleeps.borrow() >= 2);
        assert!(calls.contains(&"commit".to_string()));
    }

    #[tokio::test]
    async fn unwipeable_target_aborts_with_nothing_committed() {
        let s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        for _ in 0..WIPE_ATTEMPTS {
            s.removals.borrow_mut().push_back(Err("held open".into()));
        }
        let err = run_adopt(&s, &seed("aa"), Some(&seed("bb"))).await.unwrap_err();
        assert!(err.contains("unchanged"), "abort copy must say nothing changed: {}", err);
        let calls = s.calls();
        assert!(!calls.contains(&"commit".to_string()), "commit must never run: {:?}", calls);
        assert!(!calls.contains(&"swap".to_string()));
        // It kept retrying the first target and never advanced to the next.
        assert!(!calls.contains(&"remove:lair".to_string()));
    }

    #[tokio::test]
    async fn lying_removal_is_caught_by_verification() {
        // Every removal reports Ok but the target never leaves the disk -
        // the real Windows failure mode. The exists() verification must
        // catch it and abort without committing.
        let mut s = Scripted::new(&["conductor", "lair", "lair-passphrase"]);
        s.honest_removals = false;
        let err = run_adopt(&s, &seed("aa"), None).await.unwrap_err();
        assert!(err.contains("still present"), "{}", err);
        let calls = s.calls();
        assert_eq!(
            calls.iter().filter(|c| *c == "remove:conductor").count(),
            WIPE_ATTEMPTS
        );
        assert!(!calls.contains(&"commit".to_string()));
    }

    #[tokio::test]
    async fn absent_targets_skip_removal_and_commit_runs() {
        let s = Scripted::new(&[]);
        run_adopt(&s, &seed("aa"), None).await.unwrap();
        assert_eq!(s.calls(), vec!["revoke", "stop", "settle", "commit", "swap"]);
    }

    #[tokio::test]
    async fn commit_failure_propagates_and_swap_never_runs() {
        let s = Scripted::new(&[]);
        s.commit.borrow_mut().push_back(Err("disk full".into()));
        let err = run_adopt(&s, &seed("aa"), None).await.unwrap_err();
        assert!(err.contains("disk full"));
        assert!(!s.calls().contains(&"swap".to_string()));
    }

    // ── The backup escrow gate ──────────────────────────────────────────

    struct ScriptedProbe {
        result: RefCell<VecDeque<Result<EscrowedSeed, String>>>,
    }
    impl ScriptedProbe {
        fn one(r: Result<EscrowedSeed, String>) -> Self {
            Self { result: RefCell::new(VecDeque::from([r])) }
        }
    }
    impl EscrowProbe for ScriptedProbe {
        async fn escrowed_seed(&self) -> Result<EscrowedSeed, String> {
            self.result
                .borrow_mut()
                .pop_front()
                .expect("gate probed the escrow more often than scripted")
        }
    }

    #[tokio::test]
    async fn gate_allows_matching_seed_empty_slot_and_data_only() {
        for escrowed in [
            Ok(EscrowedSeed::Seed("aa".repeat(32))),
            Ok(EscrowedSeed::NoBackup),
            Ok(EscrowedSeed::DataOnly),
        ] {
            let p = ScriptedProbe::one(escrowed);
            backup_escrow_gate(&p, Ok(Some(seed("aa")))).await.unwrap();
        }
    }

    #[tokio::test]
    async fn gate_refuses_foreign_seed_missing_local_and_unreadable_local() {
        let cases: Vec<Result<Option<DeviceSeed>, String>> = vec![
            Ok(Some(seed("bb"))),
            Ok(None),
            Err("corrupt".into()),
        ];
        for local in cases {
            let p = ScriptedProbe::one(Ok(EscrowedSeed::Seed("aa".repeat(32))));
            let err = backup_escrow_gate(&p, local).await.unwrap_err();
            assert!(err.contains("backup held"), "{}", err);
        }
    }

    #[tokio::test]
    async fn gate_refuses_when_the_probe_fails() {
        let p = ScriptedProbe::one(Err("vault unreachable".into()));
        let err = backup_escrow_gate(&p, Ok(Some(seed("aa")))).await.unwrap_err();
        assert!(err.contains("never write"), "{}", err);
    }

    #[test]
    fn classify_escrow_payload_matrix() {
        let seeded = serde_json::json!({"app_keys": {"device_seed_hex": "ab"}});
        assert!(matches!(
            classify_escrow_payload(&seeded),
            Ok(EscrowedSeed::Seed(s)) if s == "ab"
        ));
        let legacy_null = serde_json::json!({"app_keys": {"device_seed_hex": null}});
        assert!(matches!(classify_escrow_payload(&legacy_null), Ok(EscrowedSeed::DataOnly)));
        let pre_seed = serde_json::json!({"version": 1, "cells": []});
        assert!(matches!(classify_escrow_payload(&pre_seed), Ok(EscrowedSeed::DataOnly)));
        let not_object = serde_json::json!(null);
        assert!(classify_escrow_payload(&not_object).is_err());
        let weird = serde_json::json!({"app_keys": {"device_seed_hex": 7}});
        assert!(classify_escrow_payload(&weird).is_err());
    }
}
