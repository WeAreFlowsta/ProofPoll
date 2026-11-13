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
/// Both lines' directories are listed: the 0.7 store is encrypted under the
/// same passphrase and holds the same outgoing key.
pub(crate) const WIPE_TARGETS: &[&str] = &["conductor", "lair", "conductor-07", "lair-07", "lair-passphrase"];
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

/// Written by an adopt whose Vault backup shows the key already has chains
/// on the Holochain 0.7 line (DNA v1.4, and v1.5 once installed). The next
/// start rebuilds each from the network (`restore_from_dht`) instead of
/// starting a second chain under the same key: in 0.7.1 the new chain and the
/// old one, arriving from peers, share one store, so the new device's writes
/// fail or fork the chain. Cleared once the restored apps are enabled.
pub const RESTORE_MARKER: &str = "restore-chain-07";

/// What the restore marker records.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct RestoreMarker {
    /// The adopted agent; a start under any other key ignores the marker.
    pub agent: String,
    /// The highest action seq the backup holds; the restored chain reaches
    /// at least this far.
    pub backup_head_seq: u32,
    /// The DNA versions whose chain the key has (`dna::APPS_07`); a marker
    /// from before v1.5 names v1.4 alone.
    #[serde(default = "only_v1_4")]
    pub dnas: Vec<String>,
    /// The highest action seq the backup holds for each DNA version. A
    /// restore that comes back shorter than this is missing records the old
    /// device wrote (`conductor.rs` asks again instead of enabling it). A
    /// marker from before this field has none, and nothing is compared.
    #[serde(default)]
    pub heads: std::collections::BTreeMap<String, u32>,
}

fn only_v1_4() -> Vec<String> {
    vec!["1.4".into()]
}

fn restore_marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RESTORE_MARKER)
}

pub fn read_restore_marker(data_dir: &Path) -> Option<RestoreMarker> {
    let bytes = std::fs::read(restore_marker_path(data_dir)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(m) => Some(m),
        Err(e) => {
            log::warn!("[restore] the restore marker is unreadable ({}); ignoring it", e);
            None
        }
    }
}

fn write_restore_marker(data_dir: &Path, marker: &RestoreMarker) -> Result<(), String> {
    let bytes = serde_json::to_vec(marker).map_err(|e| e.to_string())?;
    let tmp = data_dir.join(format!("{}.tmp", RESTORE_MARKER));
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, restore_marker_path(data_dir)).map_err(|e| e.to_string())
}

pub fn clear_restore_marker(data_dir: &Path) {
    let _ = std::fs::remove_file(restore_marker_path(data_dir));
}

/// Which Holochain line a canonical backup's chain comes from: the cell's
/// `line` (backups from 0.6.0 on), else the records' shape (a 0.7 dump's
/// action is `{header, data}`, a 0.6 dump's a tagged `{type, ..}`).
#[derive(Debug, PartialEq)]
pub(crate) enum BackupChain {
    /// The key has 0.7 chains: the highest seq the backup holds, and each
    /// chain's DNA version (the cell's `dna_version`; before 0.6.0, v1.4).
    /// A cell is a chain even with no records listed: the app was installed.
    /// `heads`: the highest seq per DNA version, for the versions with records.
    Line07 { head_seq: u32, dnas: Vec<String>, heads: std::collections::BTreeMap<String, u32> },
    Line06,
    /// No records to tell from.
    Unknown,
}

pub(crate) fn backup_chain(data: &serde_json::Value) -> BackupChain {
    let mut line06 = false;
    let mut head_seq = 0u32;
    let mut dnas: Vec<String> = Vec::new();
    let mut heads: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let cells = data.get("cells").and_then(|c| c.as_array()).map(|c| c.as_slice()).unwrap_or(&[]);
    for cell in cells {
        let mut cell07 = false;
        let mut cell_head: Option<u32> = None;
        match cell.get("line").and_then(|l| l.as_str()) {
            Some("holochain-0.7") => cell07 = true,
            Some("holochain-0.6") => line06 = true,
            _ => {}
        }
        let records = cell.get("records").and_then(|r| r.as_array()).map(|r| r.as_slice()).unwrap_or(&[]);
        for rec in records {
            let action = &rec["raw_record"]["action"];
            if let Some(header) = action.get("header").filter(|h| h.is_object()) {
                cell07 = true;
                let seq = header.get("action_seq").and_then(|s| s.as_u64()).unwrap_or(0);
                let seq = u32::try_from(seq).unwrap_or(u32::MAX);
                head_seq = head_seq.max(seq);
                cell_head = Some(cell_head.map_or(seq, |h| h.max(seq)));
            } else if action.get("type").is_some() {
                line06 = true;
            }
        }
        if cell07 {
            let dna = cell.get("dna_version").and_then(|v| v.as_str()).unwrap_or("1.4").to_string();
            if let Some(h) = cell_head {
                let e = heads.entry(dna.clone()).or_insert(h);
                *e = (*e).max(h);
            }
            if !dnas.contains(&dna) {
                dnas.push(dna);
            }
        }
    }
    if !dnas.is_empty() {
        BackupChain::Line07 { head_seq, dnas, heads }
    } else if line06 {
        BackupChain::Line06
    } else {
        BackupChain::Unknown
    }
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

use crate::vault_probe::webview_origin;

pub(crate) struct VaultEscrowProbe {
    pub client_id: String,
}

impl VaultEscrowProbe {
    /// The app's backup payload in the linked Vault; None when there is none.
    pub(crate) async fn retrieve(&self) -> Result<Option<serde_json::Value>, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(format!("{}/backup/retrieve", crate::vault_probe::vault_base_url().await))
            .header("Origin", webview_origin())
            .json(&serde_json::json!({ "client_id": self.client_id }))
            .send()
            .await
            .map_err(|e| format!("vault unreachable: {}", e))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            return Err(body["error"]
                .as_str()
                .unwrap_or("retrieve_failed")
                .to_string());
        }
        let mut v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("bad retrieve response: {}", e))?;
        Ok(Some(v["data"].take()))
    }
}

impl EscrowProbe for VaultEscrowProbe {
    async fn escrowed_seed(&self) -> Result<EscrowedSeed, String> {
        match self.retrieve().await? {
            None => Ok(EscrowedSeed::NoBackup),
            Some(data) => classify_escrow_payload(&data),
        }
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
        let payload = crate::hc::encode(&hash)?;
        crate::commands::call_zome(client, crate::commands::AGENT_LINKING_ZOME, "revoke_link", payload)
            .await?;
        Ok(())
    }

    async fn stop_processes(&self) {
        // Each conductor asked to shut down, then its key store, and all
        // four processes gone when this returns.
        let _ = tokio::task::spawn_blocking(crate::conductor::stop_lines).await;
        log::info!("[adopt] both conductors and key stores stopped");
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
    client_id: Option<String>,
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

    // The backup the seed came from says whether the key already has a
    // chain on the 0.7 line; if so the next start rebuilds it from the
    // network rather than starting a second one under the same key. An
    // unreadable backup stops the adopt: guessing either way is unsafe.
    let client_id = client_id.ok_or(
        "restore held: no client_id supplied, so the backup's chain cannot be checked first",
    )?;
    let backup = VaultEscrowProbe { client_id }
        .retrieve()
        .await
        .map_err(|e| format!("Could not read your Vault backup ({}). Try again in a moment - nothing was changed.", e))?
        .ok_or("Your Vault holds no ProofPoll backup any more - nothing was changed.")?;
    match classify_escrow_payload(&backup)? {
        EscrowedSeed::Seed(hex) if hex == incoming.device_seed_hex => {}
        _ => {
            return Err("Your Vault backup no longer holds that key. Reopen the Identity page and try again - nothing was changed.".into())
        }
    }
    let marker = match backup_chain(&backup) {
        BackupChain::Line07 { head_seq, dnas, heads } => {
            let agent = backup["agent_pub_key"]
                .as_str()
                .ok_or("Your Vault backup does not name its key - nothing was changed.")?;
            Some(RestoreMarker { agent: agent.to_string(), backup_head_seq: head_seq, dnas, heads })
        }
        BackupChain::Line06 | BackupChain::Unknown => None,
    };
    // Before the commit, so a marker that cannot be written leaves nothing
    // changed; a marker left by an adopt that aborts names a key the next
    // start does not use, and is ignored there.
    match &marker {
        Some(m) => {
            write_restore_marker(&state.data_dir, m)
                .map_err(|e| format!("Could not prepare the restore ({}) - nothing was changed.", e))?;
            log::info!(
                "[adopt] the backup holds 0.7 chains (v{}) up to seq {}: the next start restores them from the network",
                m.dnas.join(", v"),
                m.backup_head_seq
            );
        }
        None => clear_restore_marker(&state.data_dir),
    }

    let seams = AppSeams { state: state.inner().clone() };
    if let Err(e) = run_adopt(&seams, &incoming, local.as_ref()).await {
        clear_restore_marker(&state.data_dir);
        return Err(e);
    }
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
    async fn the_07_line_is_wiped_with_the_06_line() {
        let s = Scripted::new(&["conductor", "lair", "conductor-07", "lair-07", "lair-passphrase"]);
        run_adopt(&s, &seed("aa"), Some(&seed("bb"))).await.unwrap();
        let removed: Vec<String> = s.calls().into_iter().filter(|c| c.starts_with("remove:")).collect();
        assert_eq!(
            removed,
            vec!["remove:conductor", "remove:lair", "remove:conductor-07", "remove:lair-07", "remove:lair-passphrase"]
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
    /// A 0.7 dump record (`action` = `{header, data}`) and a 0.6 one (a
    /// tagged `{type, ..}`), in the shapes `build_canonical_backup` writes.
    fn rec07(seq: u64) -> serde_json::Value {
        serde_json::json!({"entryType": "Poll", "raw_record": {"action": {
            "header": {"author": [1, 2], "timestamp": 1, "action_seq": seq, "prev_action": null},
            "data": {"type": "Create"}}}})
    }
    fn rec06(seq: u64) -> serde_json::Value {
        serde_json::json!({"entryType": "Poll", "raw_record": {"action": {
            "type": "Create", "author": [1, 2], "timestamp": 1, "action_seq": seq}}})
    }

    #[test]
    fn backup_chain_tells_the_lines_apart() {
        // 0.5.0's backups: v1.4 records, no `line`; the highest seq counts.
        let b050 = serde_json::json!({"cells": [{"role_name": "polls", "records": [rec07(0), rec07(31), rec07(7)]}]});
        let v14 = || vec!["1.4".to_string()];
        let heads = |pairs: &[(&str, u32)]| pairs.iter().map(|(v, s)| (v.to_string(), *s)).collect::<std::collections::BTreeMap<String, u32>>();
        assert_eq!(backup_chain(&b050), BackupChain::Line07 { head_seq: 31, dnas: v14(), heads: heads(&[("1.4", 31)]) });
        // 0.3/0.4's backups: v1.3 records from the 0.6 line.
        let b040 = serde_json::json!({"cells": [{"records": [rec06(0), rec06(12)]}]});
        assert_eq!(backup_chain(&b040), BackupChain::Line06);
        // From 0.6.0 the cell names its line.
        let b060 = serde_json::json!({"cells": [{"line": "holochain-0.7", "records": [rec07(4)]}]});
        assert_eq!(backup_chain(&b060), BackupChain::Line07 { head_seq: 4, dnas: v14(), heads: heads(&[("1.4", 4)]) });
        let named_only = serde_json::json!({"cells": [{"line": "holochain-0.7", "records": []}]});
        assert_eq!(backup_chain(&named_only), BackupChain::Line07 { head_seq: 0, dnas: v14(), heads: heads(&[]) });
        // From v1.5 each 0.7 cell names its DNA version; the 0.6 one does not count.
        let b15 = serde_json::json!({"cells": [
            {"line": "holochain-0.7", "dna_version": "1.4", "records": [rec07(9)]},
            {"line": "holochain-0.7", "dna_version": "1.5", "records": [rec07(3)]},
            {"line": "holochain-0.6", "dna_version": "1.3", "records": [rec06(40)]}]});
        // Each version keeps its own head: v1.5's is 3, not the 9 of v1.4.
        assert_eq!(backup_chain(&b15), BackupChain::Line07 { head_seq: 9, dnas: vec!["1.4".into(), "1.5".into()], heads: heads(&[("1.4", 9), ("1.5", 3)]) });
        // v1.5 installed with only its genesis listed is still a chain to restore.
        let genesis_only = serde_json::json!({"cells": [
            {"line": "holochain-0.7", "dna_version": "1.4", "records": [rec07(9)]},
            {"line": "holochain-0.7", "dna_version": "1.5", "records": []}]});
        assert_eq!(backup_chain(&genesis_only), BackupChain::Line07 { head_seq: 9, dnas: vec!["1.4".into(), "1.5".into()], heads: heads(&[("1.4", 9)]) });
        // Nothing to tell from.
        assert_eq!(backup_chain(&serde_json::json!({"cells": [{"records": []}]})), BackupChain::Unknown);
        assert_eq!(backup_chain(&serde_json::json!({"version": 1})), BackupChain::Unknown);
        assert_eq!(backup_chain(&serde_json::json!(null)), BackupChain::Unknown);
    }

    #[test]
    fn restore_marker_round_trips_and_clears() {
        let dir = std::env::temp_dir().join(format!("pp-restore-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_restore_marker(&dir), None);
        let m = RestoreMarker {
            agent: "uhCAkf5HNWEmQnKbf-3x-4i-Lmu9U1rC2XbcE0x8qVEKJLvODocb0".into(),
            backup_head_seq: 31,
            dnas: vec!["1.4".into(), "1.5".into()],
            heads: [("1.4".to_string(), 31u32), ("1.5".to_string(), 12u32)].into_iter().collect(),
        };
        write_restore_marker(&dir, &m).unwrap();
        assert_eq!(read_restore_marker(&dir), Some(m));
        assert!(!dir.join(format!("{}.tmp", RESTORE_MARKER)).exists());
        // A marker written before v1.5 names v1.4 alone.
        std::fs::write(
            dir.join(RESTORE_MARKER),
            br#"{"agent":"uhCAkf5HNWEmQnKbf-3x-4i-Lmu9U1rC2XbcE0x8qVEKJLvODocb0","backup_head_seq":31}"#,
        )
        .unwrap();
        assert_eq!(read_restore_marker(&dir).map(|m| m.dnas), Some(vec!["1.4".to_string()]));
        // ... and holds no per-version heads, so nothing is compared.
        assert_eq!(read_restore_marker(&dir).map(|m| m.heads.len()), Some(0));
        // A damaged marker is ignored, never trusted.
        std::fs::write(dir.join(RESTORE_MARKER), b"{not json").unwrap();
        assert_eq!(read_restore_marker(&dir), None);
        clear_restore_marker(&dir);
        assert!(!dir.join(RESTORE_MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
