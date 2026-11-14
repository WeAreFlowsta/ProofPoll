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
//! The engine (`run_adopt`), the gate's decisions and the read of the Vault's
//! backup are `flowsta_vault_client::adopt`, unit-tested there. Here: what
//! ProofPoll wipes, its words for each refusal, the marker that tells the
//! next start which chains to restore from the network, and the commands.

use crate::commands::AppState;
use crate::device_seed::{self, DeviceSeed};
use flowsta_vault_client::adopt::{self as sdk, AdoptError, AdoptSeams, Held};
pub(crate) use flowsta_vault_client::adopt::{classify_escrow_payload, clear_relink_marker, relink_marker_path, EscrowProbe, EscrowedSeed};
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

/// What the next start restores from the network for the key in `backup`:
/// each 0.7 chain the backup shows, up to the seq it holds. None for a
/// backup from ProofPoll 0.3 or 0.4 (its chain is on the 0.6 line only, so
/// the key has no 0.7 chain anywhere: the 0.7 apps start their chains here,
/// and what the key wrote on v1.3 is read from the 0.6 line and carried),
/// and for a backup with no records to tell from.
pub(crate) fn marker_for(backup: &serde_json::Value) -> Result<Option<RestoreMarker>, String> {
    match backup_chain(backup) {
        BackupChain::Line07 { head_seq, dnas, heads } => {
            let agent = backup["agent_pub_key"].as_str().ok_or("Your Vault backup does not name its key - nothing was changed.")?;
            Ok(Some(RestoreMarker { agent: agent.to_string(), backup_head_seq: head_seq, dnas, heads }))
        }
        BackupChain::Line06 | BackupChain::Unknown => Ok(None),
    }
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

// ── ProofPoll's words and its Vault ────────────────────────────────────

/// The read of this app's backup in the person's Vault.
pub(crate) fn escrow_probe(client_id: String) -> sdk::VaultEscrowProbe {
    sdk::VaultEscrowProbe { vault: crate::vault_probe::client().clone(), client_id }
}

fn adopt_refusal(e: AdoptError) -> String {
    match e {
        AdoptError::SameKey => "This device already uses that key - nothing to restore.".into(),
        AdoptError::BadSeed(e) | AdoptError::Commit(e) => e,
        AdoptError::NotReleased { target, why } => format!(
            "Could not release this device's Holochain state ({}: {}). \
             Close and reopen ProofPoll, then try again - your current key \
             and sign-in are unchanged.",
            target, why
        ),
    }
}

fn held(e: Held) -> String {
    match e {
        Held::CheckFailed(e) => format!("backup held: could not check the Vault's escrowed key first ({}) - when in doubt, never write", e),
        Held::OtherKey => "backup held: your Vault backup escrows a different authorship key than this \
                           device's. Restore the key from the Identity page first - overwriting would \
                           destroy it."
            .into(),
        Held::NoLocalKey => "backup held: your Vault backup escrows an authorship key this install does \
                             not have. Restore the key from the Identity page first - overwriting would \
                             destroy it."
            .into(),
        Held::LocalUnreadable(e) => format!(
            "backup held: this device's recovery file is unreadable ({}) - refusing to \
             overwrite the escrowed key",
            e
        ),
    }
}

/// Refuse a canonical-backup write whenever it would destroy an escrowed
/// seed that is not this device's. This is what keeps the adopt window
/// safe: a fresh install links, its hourly backup fires, and without this
/// gate that backup would replace the lost machine's escrowed seed before
/// the user ever saw the restore offer.
pub(crate) async fn backup_escrow_gate<P: EscrowProbe>(probe: &P, local: Result<Option<DeviceSeed>, String>) -> Result<(), String> {
    sdk::backup_escrow_gate(probe, local).await.map_err(held)
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
        sdk::target_exists(&self.state.data_dir, target)
    }

    fn remove_target(&self, target: &str) -> Result<(), String> {
        sdk::remove_target(&self.state.data_dir, target)
    }

    fn commit_seed(&self, seed: &DeviceSeed) -> Result<(), String> {
        device_seed::replace(&self.state.data_dir, seed)
    }

    fn finish_binding_swap(&self) {
        sdk::finish_binding_swap(&self.state.data_dir);
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
    let backup = escrow_probe(client_id)
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
    let marker = marker_for(&backup)?;
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
    if let Err(e) = sdk::run_adopt(&seams, WIPE_TARGETS, &incoming, local.as_ref()).await {
        clear_restore_marker(&state.data_dir);
        return Err(adopt_refusal(e));
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
    sdk::run_adopt(&seams, WIPE_TARGETS, &incoming, None).await.map_err(adopt_refusal)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The words the person reads are ProofPoll's, whatever the engine calls
    /// the reason.
    #[test]
    fn refusals_keep_this_app_s_words() {
        assert!(adopt_refusal(AdoptError::SameKey).contains("nothing to restore"));
        let stuck = adopt_refusal(AdoptError::NotReleased { target: "lair".into(), why: "still present".into() });
        assert!(stuck.contains("(lair: still present)") && stuck.contains("Close and reopen ProofPoll") && stuck.contains("unchanged"), "{stuck}");
        assert_eq!(adopt_refusal(AdoptError::Commit("disk full".into())), "disk full");
        for h in [Held::OtherKey, Held::NoLocalKey, Held::LocalUnreadable("x".into()), Held::CheckFailed("x".into())] {
            assert!(held(h).starts_with("backup held"));
        }
        assert!(held(Held::OtherKey).contains("from the Identity page"));
        assert!(held(Held::CheckFailed("x".into())).contains("never write"));
    }

    /// Both lines' folders and the key store's passphrase are wiped: the 0.7
    /// store holds the same outgoing key.
    #[test]
    fn the_07_line_is_wiped_with_the_06_line() {
        for dir in crate::conductor::LINE_DIRS {
            assert!(WIPE_TARGETS.contains(dir), "{dir} is wiped");
        }
        assert!(WIPE_TARGETS.contains(&"lair-passphrase"));
        assert!(!WIPE_TARGETS.contains(&crate::device_seed::RECOVERY_FILE), "the seed file is the commit's, not the wipe's");
    }

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

    /// What a restore does with each age of backup. From 0.3 or 0.4 (v1.3
    /// on the 0.6 line): nothing is restored on 0.7, where the key never
    /// had a chain, so no second chain can be started under it there. From
    /// 0.5.0 (v1.4, no line named): v1.4 is restored to the backup's head.
    /// From 0.6.0: each 0.7 chain to its own head.
    #[test]
    fn a_restore_asks_the_network_only_for_chains_the_backup_shows() {
        let key = "uhCAkf5HNWEmQnKbf-3x-4i-Lmu9U1rC2XbcE0x8qVEKJLvODocb0";
        let b040 = serde_json::json!({"agent_pub_key": key, "cells": [{"records": [rec06(0), rec06(12)]}]});
        assert_eq!(marker_for(&b040), Ok(None), "a 0.3 or 0.4 backup: the 0.7 apps start their chains here");
        let empty = serde_json::json!({"agent_pub_key": key, "cells": []});
        assert_eq!(marker_for(&empty), Ok(None));

        let b050 = serde_json::json!({"agent_pub_key": key, "cells": [{"role_name": "polls", "records": [rec07(0), rec07(31)]}]});
        let m = marker_for(&b050).unwrap().expect("a 0.5.0 backup names a v1.4 chain");
        assert_eq!((m.agent.as_str(), m.backup_head_seq, m.dnas.clone(), m.heads.get("1.4").copied()), (key, 31, vec!["1.4".to_string()], Some(31)));

        let b060 = serde_json::json!({"agent_pub_key": key, "cells": [
            {"records": [rec06(0), rec06(40)]},
            {"line": "holochain-0.7", "dna_version": "1.4", "records": [rec07(18)]},
            {"line": "holochain-0.7", "dna_version": "1.5", "records": [rec07(48)]},
        ]});
        let m = marker_for(&b060).unwrap().expect("a 0.6.0 backup names both 0.7 chains");
        assert_eq!(m.dnas, vec!["1.4".to_string(), "1.5".to_string()]);
        assert_eq!((m.heads.get("1.4").copied(), m.heads.get("1.5").copied(), m.backup_head_seq), (Some(18), Some(48), 48));

        // A backup with 0.7 records and no key named is refused, not guessed.
        let nameless = serde_json::json!({"cells": [{"line": "holochain-0.7", "records": [rec07(4)]}]});
        assert!(marker_for(&nameless).is_err());
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
