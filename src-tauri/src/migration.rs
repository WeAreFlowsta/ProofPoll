//! DNA migration orchestration for ProofPoll.
//!
//! Migrates user-authored data from the previous DNA version to the current one:
//!   1. Export user-authored polls and votes from the source DHT
//!   2. Re-create polls on the destination DHT
//!   3. Publish MigratedPoll entries so other users can discover hash mappings
//!   4. Re-cast votes (where the target poll has been migrated)
//!   5. Retry pending votes in a background loop
//!
//! ## For developers forking this app
//!
//! The migration pattern works for any data model. To adapt:
//!   1. Replace the `Poll`/`Vote` structs with your entry types
//!   2. Replace `CreatePollInput`/`CastVoteInput` with your zome input types
//!   3. Update the zome function names in `call_zome_on()` calls
//!   4. Update `ROLE_NAME` to your app's role name
//!
//! You do NOT need to change version numbers or client variable names —
//! `run_migration()` takes source and destination clients as parameters,
//! and the state file name is derived from `ACTIVE_APP_ID` automatically.

use crate::commands::AppState;
use crate::dna::ACTIVE_APP_ID;
use holochain_types::prelude::ActionHash;
use std::path::Path;
use std::sync::Arc;
use tauri::Emitter;

/// Role name for zome calls — must match happ.yaml role id.
const ROLE_NAME: &str = "proofpoll";

// ── Migration state (persisted to disk) ───────────────────────────────

/// State file name derived from the active DNA version.
/// Each version gets its own state file so previous migrations don't interfere.
fn state_file_name() -> String {
    format!("migration-{}-state.json", ACTIVE_APP_ID)
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct MigrationState {
    pub status: MigrationStatus,
    pub polls_migrated: Vec<MigratedPollRecord>,
    pub votes_pending: Vec<PendingVote>,
    pub votes_migrated: Vec<MigratedVoteRecord>,
    /// Old (v1.3) hashes of the drafts and vote reasons already copied to
    /// v1.4 by `carry_private`. Defaults keep 0.5.0 state files loading: a
    /// file that fails to parse resets to NotStarted and migrates again.
    #[serde(default)]
    pub private_copied: Vec<String>,
    /// True once `carry_private` finished with nothing left to copy.
    #[serde(default)]
    pub private_carried: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum MigrationStatus {
    NotStarted,
    InProgress,
    Complete,
    Error(String),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct MigratedPollRecord {
    pub old_hash: String,
    pub new_hash: String,
    pub title: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PendingVote {
    /// Hash of the poll on the source DHT.
    pub source_poll_hash: String,
    pub option_index: u32,
    pub poll_title: String,
    pub retry_count: u32,
    /// The poll's closing time (seconds). Once it passes, the vote cannot
    /// be cast on the new line and is dropped from the retries; it stays
    /// where it was cast, read there. None in state files from 0.5.0.
    #[serde(default)]
    pub closes_at: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct MigratedVoteRecord {
    pub old_poll_hash: String,
    pub new_poll_hash: String,
    pub option_index: u32,
}

impl Default for MigrationState {
    fn default() -> Self {
        Self {
            status: MigrationStatus::NotStarted,
            polls_migrated: Vec::new(),
            votes_pending: Vec::new(),
            votes_migrated: Vec::new(),
            private_copied: Vec::new(),
            private_carried: false,
        }
    }
}

impl MigrationState {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(state_file_name());
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        }
    }

    pub fn save(&self, data_dir: &Path) {
        let path = data_dir.join(state_file_name());
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

// ── Zome entry types (for deserialization) ────────────────────────────
//
// These mirror the integrity zome structs. Replace with your own entry
// types when forking.

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Poll {
    title: String,
    description: String,
    options: Vec<String>,
    created_at: i64,
    closes_at: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Vote {
    poll_action_hash: ActionHash,
    option_index: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct MigratedPollEntry {
    pub old_action_hash: ActionHash,
    pub new_action_hash: ActionHash,
    pub migrated_at: i64,
    /// v1.4+ carries the original record's origin; absent on v1.3 entries.
    #[serde(default)]
    pub carried_from: Option<CarriedFrom>,
}

/// Where a carried record came from (the DNA v1.4 `CarriedFrom`): who wrote
/// it, when, the original signed action when the migrating agent still held
/// it, who re-published it here, and a slot for a device certificate chain.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CarriedFrom {
    pub original_author: holochain_types::prelude::AgentPubKey,
    pub original_timestamp: i64,
    pub original_signed_action: Option<Vec<u8>>,
    pub witness: holochain_types::prelude::AgentPubKey,
    pub certificate: Option<Vec<u8>>,
}

// ── Zome input types ──────────────────────────────────────────────────

#[derive(serde::Serialize, Debug)]
struct CreatePollInput {
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    poll_type: String,
}

/// An encrypted draft or vote reason, as both v1.3 and v1.4 store it.
#[derive(serde::Deserialize)]
struct EncryptedEntryCopy {
    cipher: Vec<u8>,
    nonce: Vec<u8>,
}

#[derive(serde::Serialize, Debug)]
struct CreateEncryptedEntryInput {
    cipher: Vec<u8>,
    nonce: Vec<u8>,
    link_as: String,
    related_hash: Option<ActionHash>,
}

#[derive(serde::Serialize, Debug)]
struct CastVoteInput {
    poll_action_hash: ActionHash,
    option_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_picture: Option<String>,
}

#[derive(serde::Serialize, Debug)]
struct RegisterMigratedPollInput {
    old_action_hash: ActionHash,
    new_action_hash: ActionHash,
    /// v1.4+: where the poll came from, so a reader can check it without
    /// the network it came from.
    original_author: Option<holochain_types::prelude::AgentPubKey>,
    original_timestamp: Option<i64>,
    /// MessagePack of the original signed action, when the record carried it.
    original_signed_action: Option<Vec<u8>>,
}

// ── Helper: decode entry from Record ──────────────────────────────────

/// A closed poll cannot be re-created on the new line (`create_poll`
/// refuses a closing time in the past, `cast_vote` a closed poll). It and
/// the votes on it stay on their own line, where the app still reads them,
/// until a carrier moves them. Before this, one closed poll failed the whole
/// migration on every launch. `closes_at` is in seconds.
fn is_closed(closes_at: Option<i64>, now_secs: i64) -> bool {
    matches!(closes_at, Some(c) if c <= now_secs)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn decode_entry<T: serde::de::DeserializeOwned>(record: &crate::hc::RecordView) -> Result<T, String> {
    record.entry_as()
}

// ── Helper: call zome on a specific client ────────────────────────────

async fn call_zome_on(
    client: &crate::hc::AppClient,
    zome: &str,
    fn_name: &str,
    payload: Vec<u8>,
) -> Result<Vec<u8>, String> {
    client
        .call_zome(ROLE_NAME, zome, fn_name, payload)
        .await
        .map_err(|e| format!("Zome call {}/{} failed: {}", zome, fn_name, e))
}

// ── Main migration function ───────────────────────────────────────────

/// Run the migration from the previous version to the active version.
///
/// ## For forking developers
///
/// Update `SOURCE_CLIENT` below to point to your previous version's
/// AppWebsocket field in AppState. The destination is always the active
/// client (`state.app_client`). Everything else is version-agnostic.
pub async fn run_migration(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    // Check if already complete
    {
        let ms = state.migration_state.lock().await;
        if ms.status == MigrationStatus::Complete {
            log::info!("Migration already complete, skipping");
            return Ok(());
        }
    }

    // The migration re-authors this agent's data onto the new DHT - a
    // write, so it takes the same tier-1 gate as every other basic write:
    // a signed-in install proceeds without an unlock; only a DIFFERENT
    // identity demonstrably present makes it WAIT (rechecking every
    // minute this session, then again next launch) rather than erroring.
    // An install that never linked has nothing to mismatch and proceeds.
    // We already run inside a spawned task, so waiting blocks nothing.
    let mut waited_minutes = 0u32;
    loop {
        match crate::commands::require_identity_link(state).await {
            Ok(_) => break,
            Err(e) if e == crate::commands::ERR_NOT_LINKED => break,
            Err(e) => {
                if waited_minutes == 0 {
                    log::info!("Migration waiting for identity confirmation: {}", e);
                }
                if waited_minutes >= 30 {
                    log::info!("Migration deferred to the next launch (identity still unconfirmable)");
                    return Ok(());
                }
                waited_minutes += 1;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        }
    }

    // Give the conductor a moment to fully initialize cells after startup.
    log::info!("Waiting 10s for conductor cells to initialize...");
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    //
    // ── FORKING: change this to your previous version's client field ──
    //
    // e.g. for v1.4, change `app_client_v1_2` to `app_client_v1_3`
    //
    let source_guard = state.app_client_v1_3.lock().await;
    let source = match source_guard.as_ref() {
        Some(c) => c,
        None => {
            log::warn!("No previous-version client available, skipping migration");
            return Ok(());
        }
    };

    let dest_guard = state.app_client.lock().await;
    let dest = match dest_guard.as_ref() {
        Some(c) => c,
        None => return Err("Active client not available".to_string()),
    };

    let my_agent = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // Mark in progress
    {
        let mut ms = state.migration_state.lock().await;
        ms.status = MigrationStatus::InProgress;
        ms.save(&state.data_dir);
    }

    let _ = app_handle.emit("migration-progress", serde_json::json!({
        "phase": "exporting",
        "message": "Reading your data from previous version...",
    }));

    // ── Phase 1: Export from source DHT ──────────────────────────────

    let payload = crate::hc::encode(&())?;
    let result = call_zome_on(source, "polls", "get_all_polls", payload).await?;
    let all_polls = crate::hc::records(source.line(), &result)?;

    // Filter to my polls and collect my votes
    let mut my_polls: Vec<(ActionHash, Poll)> = Vec::new();
    let mut my_votes: Vec<(ActionHash, u32, String, Option<i64>)> = Vec::new();
    let now_secs = now_secs();
    let mut kept_closed = 0usize;

    for record in &all_polls {
        let poll: Poll = decode_entry(record)?;
        let hash = crate::hc::action_hash(&record.action_hash)?;
        let author = record.author.clone();

        if is_closed(poll.closes_at, now_secs) {
            if author == my_agent {
                kept_closed += 1;
            }
            continue;
        }

        if author == my_agent {
            my_polls.push((hash.clone(), poll.clone()));
        }

        // Check if I voted on this poll
        let vote_payload = crate::hc::encode(&hash.clone())?;
        match call_zome_on(source, "polls", "get_poll_votes", vote_payload).await {
            Ok(vote_result) => {
                let vote_records = crate::hc::records(source.line(), &vote_result).unwrap_or_default();
                for vr in &vote_records {
                    if vr.author.clone() == my_agent {
                        let vote: Vote = decode_entry(vr)?;
                        my_votes.push((hash.clone(), vote.option_index, poll.title.clone(), poll.closes_at));
                    }
                }
            }
            Err(e) => log::warn!("Could not fetch votes for poll: {}", e),
        }
    }

    log::info!(
        "Migration export: {} polls, {} votes to migrate ({} closed polls stay on the previous version)",
        my_polls.len(),
        my_votes.len(),
        kept_closed
    );

    // ── Phase 2: Migrate polls ───────────────────────────────────────

    let _ = app_handle.emit("migration-progress", serde_json::json!({
        "phase": "polls",
        "message": format!("Migrating {} polls...", my_polls.len()),
        "total_polls": my_polls.len(),
    }));

    let mut failed_polls = 0usize;
    for (i, (old_hash, poll)) in my_polls.iter().enumerate() {
        // Check if already migrated (idempotency — survives crashes)
        {
            let ms = state.migration_state.lock().await;
            if ms.polls_migrated.iter().any(|p| p.old_hash == old_hash.to_string()) {
                log::info!("Poll '{}' already migrated, skipping", poll.title);
                continue;
            }
        }

        // Also check DHT for existing mapping (in case we crashed mid-migration)
        let mapping_payload = crate::hc::encode(&old_hash.clone())?;
        let mapping_result = call_zome_on(dest, "polls", "get_migration_mapping", mapping_payload).await;
        if let Ok(result) = mapping_result {
            let existing: Option<ActionHash> = crate::hc::decode(&result).unwrap_or(None);
            if existing.is_some() {
                log::info!("Poll '{}' already has DHT mapping, skipping", poll.title);
                let mut ms = state.migration_state.lock().await;
                ms.polls_migrated.push(MigratedPollRecord {
                    old_hash: old_hash.to_string(),
                    new_hash: existing.unwrap().to_string(),
                    title: poll.title.clone(),
                });
                ms.save(&state.data_dir);
                continue;
            }
        }

        // Create poll on destination — migrated polls default to Anonymous.
        let create_input = CreatePollInput {
            title: poll.title.clone(),
            description: poll.description.clone(),
            options: poll.options.clone(),
            closes_at: poll.closes_at,
            poll_type: "Anonymous".to_string(),
        };
        let create_payload = crate::hc::encode(&create_input)?;
        let create_result = match call_zome_on(dest, "polls", "create_poll", create_payload).await {
            Ok(r) => r,
            Err(e) if e.contains("closing time must be in the future") => {
                log::info!("Poll '{}' closed during the migration - it stays on the previous version", poll.title);
                continue;
            }
            Err(e) => {
                // One poll must not stop the others. The run ends in Error
                // and is retried at the next launch, skipping what moved.
                log::warn!("Poll '{}' not migrated this run: {}", poll.title, e);
                failed_polls += 1;
                continue;
            }
        };
        let new_hash: ActionHash = crate::hc::decode(&create_result)?;

        // Register the mapping on destination
        let register_input = RegisterMigratedPollInput {
            old_action_hash: old_hash.clone(),
            new_action_hash: new_hash.clone(),
            original_author: crate::commands::parse_agent_pub_key_string(&my_agent).ok(),
            original_timestamp: Some(poll.created_at),
            original_signed_action: None,
        };
        let register_payload = crate::hc::encode(&register_input)?;
        call_zome_on(dest, "polls", "register_migrated_poll", register_payload).await?;

        // Save progress
        {
            let mut ms = state.migration_state.lock().await;
            ms.polls_migrated.push(MigratedPollRecord {
                old_hash: old_hash.to_string(),
                new_hash: new_hash.to_string(),
                title: poll.title.clone(),
            });
            ms.save(&state.data_dir);
        }

        log::info!(
            "Migrated poll {}/{}: '{}' ({} → {})",
            i + 1,
            my_polls.len(),
            poll.title,
            old_hash,
            new_hash
        );

        let _ = app_handle.emit("migration-progress", serde_json::json!({
            "phase": "polls",
            "migrated": i + 1,
            "total_polls": my_polls.len(),
        }));
    }

    // ── Phase 3: Migrate votes ───────────────────────────────────────

    let _ = app_handle.emit("migration-progress", serde_json::json!({
        "phase": "votes",
        "message": format!("Migrating {} votes...", my_votes.len()),
        "total_votes": my_votes.len(),
    }));

    let mut migrated_count = 0;
    let mut pending_count = 0;

    for (old_poll_hash, option_index, poll_title, closes_at) in &my_votes {
        // Check if already migrated
        {
            let ms = state.migration_state.lock().await;
            if ms.votes_migrated.iter().any(|v| {
                v.old_poll_hash == old_poll_hash.to_string() && v.option_index == *option_index
            }) {
                continue;
            }
        }

        // Look up the new hash for this poll on destination
        let mapping_payload = crate::hc::encode(&old_poll_hash.clone())?;
        let mapping_result = call_zome_on(dest, "polls", "get_migration_mapping", mapping_payload).await;

        let new_poll_hash: Option<ActionHash> = match mapping_result {
            Ok(r) => crate::hc::decode(&r).unwrap_or(None),
            Err(_) => None,
        };

        if let Some(new_hash) = new_poll_hash {
            // Cast vote on destination — migrated votes are always anonymous
            let vote_input = CastVoteInput {
                poll_action_hash: new_hash.clone(),
                option_index: *option_index,
                display_name: None,
                profile_picture: None,
            };
            let vote_payload = crate::hc::encode(&vote_input)?;

            match call_zome_on(dest, "polls", "cast_vote", vote_payload).await {
                Ok(_) => {
                    let mut ms = state.migration_state.lock().await;
                    ms.votes_migrated.push(MigratedVoteRecord {
                        old_poll_hash: old_poll_hash.to_string(),
                        new_poll_hash: new_hash.to_string(),
                        option_index: *option_index,
                    });
                    ms.save(&state.data_dir);
                    migrated_count += 1;
                }
                Err(e) => {
                    if e.contains("already voted") {
                        let mut ms = state.migration_state.lock().await;
                        ms.votes_migrated.push(MigratedVoteRecord {
                            old_poll_hash: old_poll_hash.to_string(),
                            new_poll_hash: new_hash.to_string(),
                            option_index: *option_index,
                        });
                        ms.save(&state.data_dir);
                    } else {
                        log::warn!("Failed to migrate vote: {}", e);
                    }
                }
            }
        } else {
            // Poll not yet migrated by its author — add to pending
            let mut ms = state.migration_state.lock().await;
            if !ms.votes_pending.iter().any(|v| {
                v.source_poll_hash == old_poll_hash.to_string() && v.option_index == *option_index
            }) {
                ms.votes_pending.push(PendingVote {
                    source_poll_hash: old_poll_hash.to_string(),
                    option_index: *option_index,
                    poll_title: poll_title.clone(),
                    retry_count: 0,
                    closes_at: *closes_at,
                });
                ms.save(&state.data_dir);
            }
            pending_count += 1;
        }
    }

    if failed_polls > 0 {
        return Err(format!(
            "{} poll(s) could not be migrated this run; the rest moved, and these are retried at the next launch",
            failed_polls
        ));
    }

    // Mark complete
    {
        let mut ms = state.migration_state.lock().await;
        ms.status = MigrationStatus::Complete;
        ms.save(&state.data_dir);
    }

    log::info!(
        "Migration complete: {} polls, {} votes migrated, {} votes pending",
        my_polls.len(),
        migrated_count,
        pending_count
    );

    let _ = app_handle.emit("migration-complete", serde_json::json!({
        "polls_migrated": my_polls.len(),
        "votes_migrated": migrated_count,
        "votes_pending": pending_count,
    }));

    Ok(())
}

// ── Background retry loop ─────────────────────────────────────────────

/// Spawn a background task that periodically retries pending votes.
///
/// When a poll author upgrades and migrates their poll, the migration
/// mapping appears on the destination DHT. This loop discovers those
/// mappings and re-casts the pending votes.
/// Copy this person's own encrypted drafts and vote reasons from v1.3 to
/// v1.4, once. 0.5.0 read them only from v1.4, so everything written on 0.4.x
/// vanished from view and from new backups. The ciphertext is copied as it
/// is: it is encrypted to this agent's own key, the same key on both lines.
/// Idempotent by the old entry's hash; the old entries are never touched.
/// A reason follows its vote only when the vote itself moved (a vote left on
/// v1.3 keeps its reason there, and `get_vote_rationale` reads it there).
/// Returns (drafts copied, reasons copied).
pub async fn carry_private(state: &Arc<AppState>) -> Result<(usize, usize), String> {
    if state.migration_state.lock().await.private_carried {
        return Ok((0, 0));
    }
    // A write, under the same gate as the migration: wait for another launch
    // rather than write while a different identity is present.
    match crate::commands::require_identity_link(state).await {
        Ok(_) => {}
        Err(e) if e == crate::commands::ERR_NOT_LINKED => {}
        Err(_) => return Ok((0, 0)),
    }
    let my_agent = state.agent_pub_key.lock().unwrap().clone().ok_or("Agent key not available")?;
    let source_guard = state.app_client_v1_3.lock().await;
    let Some(source) = source_guard.as_ref() else { return Ok((0, 0)) };
    let dest_guard = state.app_client.lock().await;
    let dest = dest_guard.as_ref().ok_or("Active client not available")?;

    let mut failed = 0usize;

    // Drafts.
    let mut drafts = 0usize;
    let result = call_zome_on(source, "polls", "get_my_drafts", crate::hc::encode(&())?).await?;
    for rec in crate::hc::records(source.line(), &result)? {
        let old = rec.action_hash.to_string();
        if rec.author != my_agent || state.migration_state.lock().await.private_copied.contains(&old) {
            continue;
        }
        let ee: EncryptedEntryCopy = match decode_entry(&rec) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let input = CreateEncryptedEntryInput { cipher: ee.cipher, nonce: ee.nonce, link_as: "draft_poll".into(), related_hash: None };
        match call_zome_on(dest, "polls", "create_encrypted_entry", crate::hc::encode(&input)?).await {
            Ok(_) => {
                let mut ms = state.migration_state.lock().await;
                ms.private_copied.push(old);
                ms.save(&state.data_dir);
                drafts += 1;
            }
            Err(e) => {
                log::warn!("A draft was not copied this run: {}", e);
                failed += 1;
            }
        }
    }

    // Reasons, for votes that moved: my vote on the old poll -> its reason ->
    // my vote on the new poll.
    let mut reasons = 0usize;
    let moved = state.migration_state.lock().await.votes_migrated.clone();
    for v in &moved {
        let (Ok(old_poll), Ok(new_poll)) = (ActionHash::try_from(v.old_poll_hash.clone()), ActionHash::try_from(v.new_poll_hash.clone())) else { continue };
        let Some(old_vote) = my_vote_on(source, &old_poll, &my_agent).await else { continue };
        let r = match call_zome_on(source, "polls", "get_vote_rationale", crate::hc::encode(&old_vote)?).await {
            Ok(r) => r,
            Err(_) => { failed += 1; continue; }
        };
        let Some(rec) = crate::hc::record_opt(source.line(), &r)? else { continue };
        let old = rec.action_hash.to_string();
        if rec.author != my_agent || state.migration_state.lock().await.private_copied.contains(&old) {
            continue;
        }
        let Some(new_vote) = my_vote_on(dest, &new_poll, &my_agent).await else { failed += 1; continue };
        let ee: EncryptedEntryCopy = match decode_entry(&rec) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let input = CreateEncryptedEntryInput { cipher: ee.cipher, nonce: ee.nonce, link_as: "vote_rationale".into(), related_hash: Some(new_vote) };
        match call_zome_on(dest, "polls", "create_encrypted_entry", crate::hc::encode(&input)?).await {
            Ok(_) => {
                let mut ms = state.migration_state.lock().await;
                ms.private_copied.push(old);
                ms.save(&state.data_dir);
                reasons += 1;
            }
            Err(e) => {
                log::warn!("A vote reason was not copied this run: {}", e);
                failed += 1;
            }
        }
    }

    if failed == 0 {
        let mut ms = state.migration_state.lock().await;
        ms.private_carried = true;
        ms.save(&state.data_dir);
    }
    log::info!("Own drafts and vote reasons copied to the new version: {} drafts, {} reasons ({} retried next launch)", drafts, reasons, failed);
    Ok((drafts, reasons))
}

/// This agent's vote on a poll, on the given line.
async fn my_vote_on(client: &crate::hc::AppClient, poll: &ActionHash, my_agent: &str) -> Option<ActionHash> {
    let r = call_zome_on(client, "polls", "get_poll_votes", crate::hc::encode(poll).ok()?).await.ok()?;
    let recs = crate::hc::records(client.line(), &r).ok()?;
    let rec = recs.into_iter().find(|v| v.author == my_agent)?;
    crate::hc::action_hash(&rec.action_hash).ok()
}

pub fn spawn_migration_retry_loop(
    state: Arc<AppState>,
    app_handle: tauri::AppHandle,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            let has_pending = {
                let ms = state.migration_state.lock().await;
                !ms.votes_pending.is_empty()
            };

            if !has_pending {
                log::info!("No pending votes, stopping migration retry loop");
                break;
            }

            // Use the active (destination) client for retries
            let dest_client = state.app_client.lock().await;
            let dest = match dest_client.as_ref() {
                Some(c) => c,
                None => continue,
            };

            let pending = {
                let ms = state.migration_state.lock().await;
                ms.votes_pending.clone()
            };

            let mut newly_migrated = Vec::new();

            let mut closed_now = Vec::new();
            for vote in &pending {
                if is_closed(vote.closes_at, now_secs()) {
                    closed_now.push((vote.source_poll_hash.clone(), vote.option_index));
                    continue;
                }
                let old_hash = match ActionHash::try_from(vote.source_poll_hash.clone()) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let mapping_payload = match crate::hc::encode(&old_hash) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let mapping_result =
                    call_zome_on(dest, "polls", "get_migration_mapping", mapping_payload).await;

                let new_hash: Option<ActionHash> = match mapping_result {
                    Ok(r) => crate::hc::decode(&r).unwrap_or(None),
                    Err(_) => None,
                };

                if let Some(new_hash) = new_hash {
                    let vote_input = CastVoteInput {
                        poll_action_hash: new_hash.clone(),
                        option_index: vote.option_index,
                        display_name: None,
                        profile_picture: None,
                    };
                    let vote_payload = match crate::hc::encode(&vote_input) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    match call_zome_on(dest, "polls", "cast_vote", vote_payload).await {
                        Ok(_) => {
                            newly_migrated.push((
                                vote.source_poll_hash.clone(),
                                vote.option_index,
                                new_hash.to_string(),
                            ));
                            log::info!(
                                "Retry succeeded: vote on '{}' migrated",
                                vote.poll_title
                            );
                        }
                        Err(e) => {
                            if e.contains("already voted") {
                                newly_migrated.push((
                                    vote.source_poll_hash.clone(),
                                    vote.option_index,
                                    new_hash.to_string(),
                                ));
                            } else if e.contains("Poll is closed") {
                                closed_now.push((vote.source_poll_hash.clone(), vote.option_index));
                            } else {
                                log::debug!("Retry failed for vote on '{}': {}", vote.poll_title, e);
                            }
                        }
                    }
                }
            }

            if !closed_now.is_empty() {
                let mut ms = state.migration_state.lock().await;
                ms.votes_pending.retain(|v| !closed_now.contains(&(v.source_poll_hash.clone(), v.option_index)));
                ms.save(&state.data_dir);
                log::info!("{} pending vote(s) on closed polls stay where they were cast", closed_now.len());
            }

            // Update state
            if !newly_migrated.is_empty() {
                let mut ms = state.migration_state.lock().await;
                for (old_hash, option_index, new_hash) in &newly_migrated {
                    ms.votes_pending.retain(|v| {
                        !(v.source_poll_hash == *old_hash && v.option_index == *option_index)
                    });
                    ms.votes_migrated.push(MigratedVoteRecord {
                        old_poll_hash: old_hash.clone(),
                        new_poll_hash: new_hash.clone(),
                        option_index: *option_index,
                    });
                }
                ms.save(&state.data_dir);

                let _ = app_handle.emit("migration-progress", serde_json::json!({
                    "phase": "retry",
                    "votes_migrated": newly_migrated.len(),
                    "votes_pending": ms.votes_pending.len(),
                }));
            }

            // Increment retry counts
            {
                let mut ms = state.migration_state.lock().await;
                for vote in &mut ms.votes_pending {
                    vote.retry_count += 1;
                }
                ms.save(&state.data_dir);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_past_its_closing_time_stays_on_its_line() {
        let now = 1_761_800_000;
        assert!(is_closed(Some(now - 1), now));
        assert!(is_closed(Some(now), now), "create_poll refuses closes_at == now");
        assert!(!is_closed(Some(now + 60), now));
        assert!(!is_closed(None, now), "a poll with no closing time is open");
    }
}
