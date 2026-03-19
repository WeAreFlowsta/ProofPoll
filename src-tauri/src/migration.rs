//! DNA migration orchestration for ProofPoll.
//!
//! Handles the v1.1 → v1.2 upgrade by:
//!   1. Exporting user-authored polls and votes from v1.1 DHT
//!   2. Re-creating polls on v1.2 DHT (as Anonymous poll type)
//!   3. Publishing MigratedPoll entries so other users can discover hash mappings
//!   4. Re-casting votes (where the target poll has been migrated)
//!   5. Retrying pending votes in a background loop
//!
//! ## For developers forking this app
//!
//! The migration pattern works for any data model. To adapt:
//!   1. Replace the `Poll`/`Vote` structs (lines ~100-120) with your entry types
//!   2. Replace `CreatePollInput`/`CastVoteInput` with your zome input types
//!   3. Update the zome function names in `call_zome_on()` calls
//!   4. Update `RoleName::from("proofpoll")` to your app's role name
//!
//! The orchestration pattern (export → create → register mapping → retry loop)
//! is identical regardless of version numbers or data model.

use crate::commands::AppState;
use holochain_types::prelude::{ActionHash, ExternIO, Record};
use std::path::Path;
use std::sync::Arc;
use tauri::Emitter;

// ── Migration state (persisted to disk) ───────────────────────────────

const STATE_FILE: &str = "migration-v1.2-state.json";

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct MigrationState {
    pub status: MigrationStatus,
    pub polls_migrated: Vec<MigratedPollRecord>,
    pub votes_pending: Vec<PendingVote>,
    pub votes_migrated: Vec<MigratedVoteRecord>,
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
    pub v1_0_poll_hash: String,
    pub option_index: u32,
    pub poll_title: String,
    pub retry_count: u32,
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
        }
    }
}

impl MigrationState {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(STATE_FILE);
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
        let path = data_dir.join(STATE_FILE);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

// ── Zome entry types (for deserialization) ────────────────────────────

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
struct MigratedPollEntry {
    old_action_hash: ActionHash,
    new_action_hash: ActionHash,
    migrated_at: i64,
}

// ── Zome input types ──────────────────────────────────────────────────

#[derive(serde::Serialize, Debug)]
struct CreatePollInput {
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    /// v1.2 field — migrated polls default to Anonymous.
    poll_type: String,
}

#[derive(serde::Serialize, Debug)]
struct CastVoteInput {
    poll_action_hash: ActionHash,
    option_index: u32,
    /// v1.2 field — None for migrated votes (always anonymous).
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    /// v1.2 field — None for migrated votes (always anonymous).
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_picture: Option<String>,
}

#[derive(serde::Serialize, Debug)]
struct RegisterMigratedPollInput {
    old_action_hash: ActionHash,
    new_action_hash: ActionHash,
}

// ── Helper: decode entry from Record ──────────────────────────────────

fn decode_entry<T: serde::de::DeserializeOwned>(record: &Record) -> Result<T, String> {
    let entry = record
        .entry()
        .as_option()
        .ok_or("Record has no entry")?;
    let app_bytes = entry
        .as_app_entry()
        .ok_or("Not an app entry")?;
    let sb = app_bytes.as_ref();
    rmp_serde::from_slice(sb.bytes()).map_err(|e| format!("Failed to decode entry: {}", e))
}

// ── Helper: call zome on a specific client ────────────────────────────

async fn call_zome_on(
    client: &holochain_client::AppWebsocket,
    zome: &str,
    fn_name: &str,
    payload: ExternIO,
) -> Result<ExternIO, String> {
    use holochain_client::ZomeCallTarget;
    use holochain_types::prelude::{FunctionName, RoleName, ZomeName};

    client
        .call_zome(
            ZomeCallTarget::RoleName(RoleName::from("proofpoll")),
            ZomeName::from(zome),
            FunctionName::from(fn_name),
            payload,
        )
        .await
        .map_err(|e| format!("Zome call {}/{} failed: {}", zome, fn_name, e))
}

// ── Main migration function ───────────────────────────────────────────

/// Run the v1.1 → v1.2 migration.
///
/// Exports user-authored polls and votes from v1.1, re-creates them on
/// v1.2, and publishes migration mappings for other users.
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

    // Give the conductor a moment to fully initialize cells after startup.
    // Without this, zome calls to v1.1 may timeout because cells aren't
    // ready yet (especially on first run after installing v1.2).
    log::info!("Waiting 10s for conductor cells to initialize...");
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // Get both clients: v1.1 as source, v1.2 as destination
    let v1_1_client = state.app_client_v1_1.lock().await;
    let v1_1 = match v1_1_client.as_ref() {
        Some(c) => c,
        None => {
            log::warn!("No v1.1 client available, skipping migration");
            return Ok(());
        }
    };

    let v1_2_client = state.app_client.lock().await;
    let v1_2 = match v1_2_client.as_ref() {
        Some(c) => c,
        None => return Err("v1.2 client not available".to_string()),
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
        "message": "Reading your data from v1.1...",
    }));

    // ── Phase 1: Export from v1.1 ─────────────────────────────────────

    // Get all polls from v1.1
    let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
    let result = call_zome_on(v1_1, "polls", "get_all_polls", payload).await?;
    let all_polls: Vec<Record> = result.decode().map_err(|e| e.to_string())?;

    // Filter to my polls and collect my votes
    let mut my_polls: Vec<(ActionHash, Poll)> = Vec::new();
    let mut my_votes: Vec<(ActionHash, u32, String)> = Vec::new(); // (poll_hash, option_index, poll_title)

    for record in &all_polls {
        let poll: Poll = decode_entry(record)?;
        let hash = record.action_address().clone();
        let author = record.action().author().to_string();
        let is_mine = author == my_agent;

        if is_mine {
            my_polls.push((hash.clone(), poll.clone()));
        }

        // Check if I voted on this poll
        let vote_payload = ExternIO::encode(hash.clone()).map_err(|e| e.to_string())?;
        match call_zome_on(v1_1, "polls", "get_poll_votes", vote_payload).await {
            Ok(vote_result) => {
                let vote_records: Vec<Record> = vote_result.decode().unwrap_or_default();
                for vr in &vote_records {
                    if vr.action().author().to_string() == my_agent {
                        let vote: Vote = decode_entry(vr)?;
                        my_votes.push((hash.clone(), vote.option_index, poll.title.clone()));
                    }
                }
            }
            Err(e) => log::warn!("Could not fetch votes for poll: {}", e),
        }
    }

    log::info!(
        "Migration export: {} polls, {} votes to migrate",
        my_polls.len(),
        my_votes.len()
    );

    // ── Phase 2: Migrate polls ────────────────────────────────────────

    let _ = app_handle.emit("migration-progress", serde_json::json!({
        "phase": "polls",
        "message": format!("Migrating {} polls...", my_polls.len()),
        "total_polls": my_polls.len(),
    }));

    for (i, (old_hash, poll)) in my_polls.iter().enumerate() {
        // Check if already migrated (idempotency)
        {
            let ms = state.migration_state.lock().await;
            if ms.polls_migrated.iter().any(|p| p.old_hash == old_hash.to_string()) {
                log::info!("Poll '{}' already migrated, skipping", poll.title);
                continue;
            }
        }

        // Also check DHT for existing mapping (in case we crashed mid-migration)
        let mapping_payload = ExternIO::encode(old_hash.clone()).map_err(|e| e.to_string())?;
        let mapping_result = call_zome_on(v1_2, "polls", "get_migration_mapping", mapping_payload).await;
        if let Ok(result) = mapping_result {
            let existing: Option<ActionHash> = result.decode().unwrap_or(None);
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

        // Create poll on v1.2 — migrated polls default to Anonymous poll type.
        let create_input = CreatePollInput {
            title: poll.title.clone(),
            description: poll.description.clone(),
            options: poll.options.clone(),
            closes_at: poll.closes_at,
            poll_type: "Anonymous".to_string(),
        };
        let create_payload = ExternIO::encode(create_input).map_err(|e| e.to_string())?;
        let create_result = call_zome_on(v1_2, "polls", "create_poll", create_payload).await?;
        let new_hash: ActionHash = create_result.decode().map_err(|e| e.to_string())?;

        // Register the mapping on v1.2
        let register_input = RegisterMigratedPollInput {
            old_action_hash: old_hash.clone(),
            new_action_hash: new_hash.clone(),
        };
        let register_payload = ExternIO::encode(register_input).map_err(|e| e.to_string())?;
        call_zome_on(v1_2, "polls", "register_migrated_poll", register_payload).await?;

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

    // ── Phase 3: Migrate votes ────────────────────────────────────────

    let _ = app_handle.emit("migration-progress", serde_json::json!({
        "phase": "votes",
        "message": format!("Migrating {} votes...", my_votes.len()),
        "total_votes": my_votes.len(),
    }));

    let mut migrated_count = 0;
    let mut pending_count = 0;

    for (old_poll_hash, option_index, poll_title) in &my_votes {
        // Check if already migrated
        {
            let ms = state.migration_state.lock().await;
            if ms.votes_migrated.iter().any(|v| {
                v.old_poll_hash == old_poll_hash.to_string() && v.option_index == *option_index
            }) {
                continue;
            }
        }

        // Look up the new hash for this poll on v1.2
        let mapping_payload = ExternIO::encode(old_poll_hash.clone()).map_err(|e| e.to_string())?;
        let mapping_result = call_zome_on(v1_2, "polls", "get_migration_mapping", mapping_payload).await;

        let new_poll_hash: Option<ActionHash> = match mapping_result {
            Ok(r) => r.decode().unwrap_or(None),
            Err(_) => None,
        };

        if let Some(new_hash) = new_poll_hash {
            // Cast vote on v1.2 — migrated votes are always anonymous
            let vote_input = CastVoteInput {
                poll_action_hash: new_hash.clone(),
                option_index: *option_index,
                display_name: None,
                profile_picture: None,
            };
            let vote_payload = ExternIO::encode(vote_input).map_err(|e| e.to_string())?;

            match call_zome_on(v1_2, "polls", "cast_vote", vote_payload).await {
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
                        // Already migrated via another path — mark as done
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
                v.v1_0_poll_hash == old_poll_hash.to_string() && v.option_index == *option_index
            }) {
                ms.votes_pending.push(PendingVote {
                    v1_0_poll_hash: old_poll_hash.to_string(),
                    option_index: *option_index,
                    poll_title: poll_title.clone(),
                    retry_count: 0,
                });
                ms.save(&state.data_dir);
            }
            pending_count += 1;
        }
    }

    // Identity re-linking is handled by the frontend (layout.tsx).
    // It detects local identity-link.json + empty DHT link and auto-triggers
    // a fresh signature request from the Vault via IPC.

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
/// mapping appears on the v1.1 DHT. This loop discovers those mappings
/// and re-casts the pending votes.
pub fn spawn_migration_retry_loop(
    state: Arc<AppState>,
    app_handle: tauri::AppHandle,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            // Check if there are pending votes
            let has_pending = {
                let ms = state.migration_state.lock().await;
                !ms.votes_pending.is_empty()
            };

            if !has_pending {
                log::info!("No pending votes, stopping migration retry loop");
                break;
            }

            // Try to migrate each pending vote using v1.2 client
            let v1_2_client = state.app_client.lock().await;
            let v1_2 = match v1_2_client.as_ref() {
                Some(c) => c,
                None => continue,
            };

            let pending = {
                let ms = state.migration_state.lock().await;
                ms.votes_pending.clone()
            };

            let mut newly_migrated = Vec::new();

            for vote in &pending {
                let old_hash = match ActionHash::try_from(vote.v1_0_poll_hash.clone()) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let mapping_payload = match ExternIO::encode(old_hash) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let mapping_result =
                    call_zome_on(v1_2, "polls", "get_migration_mapping", mapping_payload).await;

                let new_hash: Option<ActionHash> = match mapping_result {
                    Ok(r) => r.decode().unwrap_or(None),
                    Err(_) => None,
                };

                if let Some(new_hash) = new_hash {
                    let vote_input = CastVoteInput {
                        poll_action_hash: new_hash.clone(),
                        option_index: vote.option_index,
                        display_name: None,
                        profile_picture: None,
                    };
                    let vote_payload = match ExternIO::encode(vote_input) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    match call_zome_on(v1_2, "polls", "cast_vote", vote_payload).await {
                        Ok(_) => {
                            newly_migrated.push((
                                vote.v1_0_poll_hash.clone(),
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
                                    vote.v1_0_poll_hash.clone(),
                                    vote.option_index,
                                    new_hash.to_string(),
                                ));
                            } else {
                                log::debug!("Retry failed for vote on '{}': {}", vote.poll_title, e);
                            }
                        }
                    }
                }
            }

            // Update state
            if !newly_migrated.is_empty() {
                let mut ms = state.migration_state.lock().await;
                for (old_hash, option_index, new_hash) in &newly_migrated {
                    ms.votes_pending.retain(|v| {
                        !(v.v1_0_poll_hash == *old_hash && v.option_index == *option_index)
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
