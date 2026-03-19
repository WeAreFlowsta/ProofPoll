//! Tauri commands for ProofPoll.
//!
//! This is the bridge between the Qwik frontend and the Holochain conductor.
//! All zome calls go through the Rust backend via AppWebsocket — the frontend
//! uses lightweight Tauri `invoke()` calls and never touches @holochain/client.
//!
//! ## For forking developers
//!
//! This file has three sections:
//!   1. **Entry types + response types** (top) — Mirror your zome's Rust structs.
//!      Change these to match your own data model.
//!   2. **Infrastructure** (middle) — `AppState`, `call_zome`, `friendly_error`,
//!      `decode_entry`. Keep these as-is; they work for any Holochain app.
//!   3. **Tauri commands** (bottom) — One `#[tauri::command]` per frontend action.
//!      Replace the poll/vote/flag commands with your own. The identity-linking
//!      and migration commands are reusable infrastructure.
//!
//! After changing commands, register them in `lib.rs` → `invoke_handler`.

use crate::conductor::{ConductorHandle, ConductorStatus};
use holochain_client::AppWebsocket;
use holochain_types::prelude::{
    ActionHash, AgentPubKey, ExternIO, FunctionName, Record, ZomeName,
};
use lair_keystore_api::prelude::LairClient;
use std::path::PathBuf;
use std::sync::Mutex;

// ── Entry types matching the zome definitions ─────────────────────────
//
// These structs must match the entry types in your coordinator zome.
// Holochain entries are MessagePack-encoded, so Serialize + Deserialize
// are required. The frontend never sees these directly — they're decoded
// in the Tauri commands and returned as response types (below).

/// Used to decode MigratedPoll entries when merging v1.0 and v1.1 poll lists.
/// Mirrors the MigratedPoll entry type in the v1.1 integrity zome.
#[derive(serde::Deserialize)]
struct MigratedPollEntry {
    old_action_hash: ActionHash,
    #[allow(dead_code)]
    new_action_hash: ActionHash,
    #[allow(dead_code)]
    migrated_at: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    /// "Anonymous" or "Public". None for v1.0/v1.1 polls (treated as Anonymous).
    #[serde(default)]
    pub poll_type: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Only present on v1.2 public poll votes.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Only present on v1.2 public poll votes.
    #[serde(default)]
    pub profile_picture: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
    /// Only sent for v1.2. Omit for v1.0/v1.1 migration calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_type: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Only sent for v1.2 public polls; None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Only sent for v1.2 public polls; None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
}

// ── Frontend response types ───────────────────────────────────────────
//
// These are what the frontend receives via Tauri invoke().
// ActionHashes are converted to strings because TypeScript can't handle
// Holochain's native hash types. The frontend uses these string hashes
// to reference entries in subsequent calls.

#[derive(serde::Serialize, Clone)]
pub struct PollListItem {
    pub hash: String,
    pub poll: Poll,
    pub author: String,
    /// Which DHT this poll lives on: "1.0", "1.1", or "1.2".
    /// The frontend passes this back when voting so the vote goes to the correct cell.
    pub dna_version: String,
}

#[derive(serde::Serialize)]
pub struct PollDetail {
    pub poll: Poll,
    pub author: String,
    /// Which DHT this poll lives on: "1.0", "1.1", or "1.2".
    pub dna_version: String,
}

#[derive(serde::Serialize, Clone)]
pub struct VoteData {
    pub vote: VoteResponse,
    pub author: String,
    /// Display name from v1.2 public poll votes. None for anonymous or pre-v1.2 votes.
    pub display_name: Option<String>,
    /// Profile picture URL from v1.2 public poll votes. None for anonymous or pre-v1.2 votes.
    pub profile_picture: Option<String>,
}

#[derive(serde::Serialize, Clone)]
pub struct VoteResponse {
    pub poll_action_hash: String,
    pub option_index: u32,
}

// --- App state ---

pub struct AppState {
    pub data_dir: PathBuf,
    pub conductor_handle: Mutex<Option<ConductorHandle>>,
    pub conductor_status: Mutex<ConductorStatus>,
    pub agent_pub_key: Mutex<Option<String>>,
    /// Active (v1.2) app client for all reads and writes.
    pub app_client: tokio::sync::Mutex<Option<AppWebsocket>>,
    /// v1.1 app client for migration reads (v1.1 → v1.2).
    pub app_client_v1_1: tokio::sync::Mutex<Option<AppWebsocket>>,
    /// v1.0 app client for legacy reads.
    pub app_client_v1_0: tokio::sync::Mutex<Option<AppWebsocket>>,
    pub passphrase: Mutex<String>,
    pub lair_client: tokio::sync::Mutex<Option<LairClient>>,
    /// Current migration state (persisted to disk).
    pub migration_state: tokio::sync::Mutex<crate::migration::MigrationState>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let passphrase_path = data_dir.join("lair-passphrase");
        let passphrase = if passphrase_path.exists() {
            std::fs::read_to_string(&passphrase_path).unwrap_or_else(|_| generate_passphrase())
        } else {
            let p = generate_passphrase();
            let _ = std::fs::write(&passphrase_path, &p);
            p
        };

        let migration_state = crate::migration::MigrationState::load(&data_dir);

        Self {
            data_dir,
            conductor_handle: Mutex::new(None),
            conductor_status: Mutex::new(ConductorStatus::Stopped),
            agent_pub_key: Mutex::new(None),
            app_client: tokio::sync::Mutex::new(None),
            app_client_v1_1: tokio::sync::Mutex::new(None),
            app_client_v1_0: tokio::sync::Mutex::new(None),
            passphrase: Mutex::new(passphrase),
            lair_client: tokio::sync::Mutex::new(None),
            migration_state: tokio::sync::Mutex::new(migration_state),
        }
    }
}

fn generate_passphrase() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

// ── Helpers (reusable infrastructure) ──────────────────────────────────
//
// These constants and functions work for any Holochain app.
// Change ROLE_NAME to match your happ.yaml role id.
// Change POLLS_ZOME to match your coordinator zome name.

/// Must match the role `id` in your happ.yaml.
const ROLE_NAME: &str = "proofpoll";
/// Your app's coordinator zome name (from dna.yaml).
const POLLS_ZOME: &str = "polls";
/// Flowsta agent-linking zome — keep as-is for identity integration.
const AGENT_LINKING_ZOME: &str = "agent_linking";

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

/// Call a zome function on the active DNA version.
///
/// This is the core helper that all Tauri commands use. It handles:
///   - Routing to the correct zome via ROLE_NAME
///   - Auto-recovery from CellDisabled errors (re-enables the app and retries)
///   - User-friendly error messages via `friendly_error()`
///
/// For forking: you don't need to change this function — just call it
/// with your zome name and function name from your Tauri commands.
async fn call_zome(
    client: &AppWebsocket,
    zome: &str,
    fn_name: &str,
    payload: ExternIO,
) -> Result<ExternIO, String> {
    use holochain_client::ZomeCallTarget;
    use holochain_types::prelude::RoleName;

    let result = client
        .call_zome(
            ZomeCallTarget::RoleName(RoleName::from(ROLE_NAME)),
            ZomeName::from(zome),
            FunctionName::from(fn_name),
            payload.clone(),
        )
        .await;

    match result {
        Ok(r) => Ok(r),
        Err(e) => {
            let err_str = format!("{}", e);
            // Auto-recover from CellDisabled (e.g. after unclean shutdown)
            if err_str.contains("CellDisabled") {
                log::warn!("CellDisabled detected, attempting auto-recovery...");
                if let Err(re) = try_reenable_app().await {
                    log::error!("Auto-recovery failed: {}", re);
                    return Err(friendly_error(&err_str));
                }
                // Retry the zome call once
                log::info!("Retrying zome call after re-enabling app...");
                client
                    .call_zome(
                        ZomeCallTarget::RoleName(RoleName::from(ROLE_NAME)),
                        ZomeName::from(zome),
                        FunctionName::from(fn_name),
                        payload,
                    )
                    .await
                    .map_err(|e2| friendly_error(&format!("{}", e2)))
            } else {
                Err(friendly_error(&err_str))
            }
        }
    }
}

/// Attempt to re-enable the app via admin websocket.
async fn try_reenable_app() -> Result<(), String> {
    use holochain_client::AdminWebsocket;

    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", crate::conductor::ADMIN_WS_PORT),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WS for recovery: {}", e))?;

    admin_ws
        .enable_app(crate::dna::ACTIVE_APP_ID.to_string())
        .await
        .map_err(|e| format!("Failed to re-enable app: {}", e))?;

    log::info!("App re-enabled successfully");
    Ok(())
}

/// Translate raw Holochain errors into user-friendly messages.
fn friendly_error(raw: &str) -> String {
    if raw.contains("CellDisabled") {
        "Your data cell was temporarily disabled and could not be recovered automatically. Please restart the app.".into()
    } else if raw.contains("WebsocketError") || raw.contains("ConnectionReset") || raw.contains("Io(") {
        "Lost connection to the Holochain conductor. It may have stopped unexpectedly.".into()
    } else if raw.contains("timeout") || raw.contains("Timeout") {
        "The request timed out. The network may be slow or unreachable.".into()
    } else if raw.contains("Conductor returned an error") {
        // Strip the nested conductor error wrapper for clarity
        if let Some(inner) = raw.split("InternalError(\"").nth(1) {
            let inner = inner.trim_end_matches("\")");
            format!("Conductor error: {}", inner)
        } else {
            format!("Conductor error: {}", raw)
        }
    } else {
        format!("Something went wrong: {}", raw)
    }
}

/// Parse a uhCAk... agent key string into an AgentPubKey.
/// Decodes the base64url body and preserves the exact 39 bytes (including
/// DHT location) so the key matches what the external signer used.
fn parse_agent_pub_key_string(s: &str) -> Result<AgentPubKey, String> {
    // Strip the "u" multibase prefix
    let b64 = s.strip_prefix('u').ok_or("Agent key must start with 'u'")?;

    // Decode base64url (no padding)
    use base64::Engine;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|e| format!("Invalid base64 in agent key: {}", e))?;

    // Expect 39 bytes: 3 prefix + 32 key + 4 location
    if raw.len() != 39 {
        return Err(format!("Agent key wrong length: {} (expected 39)", raw.len()));
    }

    Ok(AgentPubKey::from_raw_39(raw))
}

// ── Status command (infrastructure — keep as-is) ──────────────────────

#[derive(serde::Serialize)]
pub struct AppStatus {
    pub ready: bool,
    pub agent_pub_key: Option<String>,
    pub conductor_status: ConductorStatus,
}

#[tauri::command]
pub fn get_app_status(state: tauri::State<'_, std::sync::Arc<AppState>>) -> AppStatus {
    let status = state.conductor_status.lock().unwrap().clone();
    let agent_key = state.agent_pub_key.lock().unwrap().clone();
    let ready = matches!(status, ConductorStatus::Ready { .. });

    AppStatus {
        ready,
        agent_pub_key: agent_key,
        conductor_status: status,
    }
}

// ── Poll commands (replace with your app's commands) ──────────────────
//
// Each command follows the same pattern:
//   1. Lock the AppWebsocket from state
//   2. Parse input (convert string hashes to ActionHash)
//   3. Encode payload with ExternIO::encode()
//   4. Call call_zome() with your zome name and function
//   5. Decode the result and return a frontend-friendly type

#[tauri::command]
pub async fn create_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    poll_type: Option<String>,
) -> Result<String, String> {
    // Require a linked Flowsta identity — frontend gating alone is bypassable.
    if load_identity_link(&state.data_dir).is_none() {
        return Err("Sign in with Flowsta to create polls".to_string());
    }

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let input = CreatePollInput {
        title,
        description,
        options,
        closes_at,
        poll_type: Some(poll_type.unwrap_or_else(|| "Anonymous".to_string())),
    };
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "create_poll", payload).await?;

    let action_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(action_hash.to_string())
}

#[tauri::command]
pub async fn get_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    action_hash: String,
) -> Result<Option<PollDetail>, String> {
    let hash = ActionHash::try_from(action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    // Try v1.2 first — most polls will be here for current users.
    {
        let client = state.app_client.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(hash.clone()).map_err(|e| e.to_string())?;
            let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
            let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;
            if let Some(record) = record {
                let poll: Poll = decode_entry(&record)?;
                return Ok(Some(PollDetail {
                    poll,
                    author: record.action().author().to_string(),
                    dna_version: "1.2".to_string(),
                }));
            }
        }
    } // v1.2 lock released

    // Fall back to v1.1 — poll author hasn't migrated to v1.2 yet.
    {
        let client = state.app_client_v1_1.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(hash.clone()).map_err(|e| e.to_string())?;
            let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
            let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;
            if let Some(record) = record {
                let poll: Poll = decode_entry(&record)?;
                return Ok(Some(PollDetail {
                    poll,
                    author: record.action().author().to_string(),
                    dna_version: "1.1".to_string(),
                }));
            }
        }
    } // v1.1 lock released

    // Fall back to v1.0 — legacy poll.
    let client = state.app_client_v1_0.lock().await;
    if let Some(client) = client.as_ref() {
        let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
        let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;
        if let Some(record) = record {
            let poll: Poll = decode_entry(&record)?;
            return Ok(Some(PollDetail {
                poll,
                author: record.action().author().to_string(),
                dna_version: "1.0".to_string(),
            }));
        }
    }

    Ok(None)
}

#[tauri::command]
pub async fn get_all_polls(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<PollListItem>, String> {
    // Phase 1: fetch v1.2 polls (active DHT) and collect migration mappings.
    // Release each lock before acquiring the next to avoid deadlocks.
    let (mut polls, migrated_v1_1_hashes, migrated_v1_0_hashes) = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;

        let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;
        let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;

        let mut items = Vec::new();
        for record in &records {
            if let Ok(poll) = decode_entry::<Poll>(record) {
                items.push(PollListItem {
                    hash: record.action_address().to_string(),
                    poll,
                    author: record.action().author().to_string(),
                    dna_version: "1.2".to_string(),
                });
            }
        }

        // Fetch migration mappings to know which v1.1 hashes are already on v1.2.
        let migrated_v1_1: std::collections::HashSet<String> = {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            match call_zome(client, POLLS_ZOME, "get_all_migration_mappings", payload).await {
                Ok(r) => {
                    let mapping_records: Vec<Record> = r.decode().unwrap_or_default();
                    let mut set = std::collections::HashSet::new();
                    for rec in &mapping_records {
                        if let Ok(entry) = decode_entry::<MigratedPollEntry>(rec) {
                            set.insert(entry.old_action_hash.to_string());
                        }
                    }
                    set
                }
                Err(e) => {
                    log::warn!("Could not fetch v1.2 migration mappings: {}", e);
                    std::collections::HashSet::new()
                }
            }
        };

        (items, migrated_v1_1, std::collections::HashSet::<String>::new())
    }; // v1.2 lock released here

    // Phase 2: fetch v1.1 polls, excluding those already migrated to v1.2.
    // Also collect their migration mappings to de-dupe against v1.0.
    let migrated_v1_0_hashes = {
        let client = state.app_client_v1_1.lock().await;
        let mut migrated_v1_0 = migrated_v1_0_hashes;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            match call_zome(client, POLLS_ZOME, "get_all_polls", payload).await {
                Ok(result) => {
                    let records: Vec<Record> = result.decode().unwrap_or_default();
                    for record in &records {
                        let hash = record.action_address().to_string();
                        if migrated_v1_1_hashes.contains(&hash) {
                            continue; // already on v1.2
                        }
                        if let Ok(poll) = decode_entry::<Poll>(record) {
                            polls.push(PollListItem {
                                hash,
                                poll,
                                author: record.action().author().to_string(),
                                dna_version: "1.1".to_string(),
                            });
                        }
                    }
                }
                Err(e) => log::warn!("Could not fetch v1.1 polls (skipping): {}", e),
            }

            // Fetch v1.1 migration mappings to de-dupe v1.0 polls
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            if let Ok(r) = call_zome(client, POLLS_ZOME, "get_all_migration_mappings", payload).await {
                let mapping_records: Vec<Record> = r.decode().unwrap_or_default();
                for rec in &mapping_records {
                    if let Ok(entry) = decode_entry::<MigratedPollEntry>(rec) {
                        migrated_v1_0.insert(entry.old_action_hash.to_string());
                    }
                }
            }
        }
        migrated_v1_0
    }; // v1.1 lock released here

    // Phase 3: fetch v1.0 polls, excluding those migrated to v1.1 or v1.2.
    {
        let client = state.app_client_v1_0.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            match call_zome(client, POLLS_ZOME, "get_all_polls", payload).await {
                Ok(result) => {
                    let records: Vec<Record> = result.decode().unwrap_or_default();
                    for record in &records {
                        let hash = record.action_address().to_string();
                        if migrated_v1_0_hashes.contains(&hash) {
                            continue; // already on v1.1 or v1.2
                        }
                        if let Ok(poll) = decode_entry::<Poll>(record) {
                            polls.push(PollListItem {
                                hash,
                                poll,
                                author: record.action().author().to_string(),
                                dna_version: "1.0".to_string(),
                            });
                        }
                    }
                }
                Err(e) => log::warn!("Could not fetch v1.0 polls (skipping): {}", e),
            }
        }
    } // v1.0 lock released here

    Ok(polls)
}

#[tauri::command]
pub async fn delete_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    action_hash: String,
) -> Result<String, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash =
        ActionHash::try_from(action_hash).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "delete_poll", payload).await?;

    let delete_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(delete_hash.to_string())
}

#[tauri::command]
pub async fn cast_vote(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    option_index: u32,
    // "1.0", "1.1", or "1.2" — routes the vote to the correct DHT cell.
    // Obtained from dna_version on PollListItem or PollDetail.
    dna_version: String,
    // "Anonymous" or "Public" — only relevant for dna_version "1.2".
    poll_type: Option<String>,
) -> Result<String, String> {
    // Require a linked Flowsta identity — frontend gating alone is bypassable.
    if load_identity_link(&state.data_dir).is_none() {
        return Err("Sign in with Flowsta to vote".to_string());
    }

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    let action_hash: ActionHash = if dna_version == "1.0" {
        // Vote on a legacy poll — write to v1.0 DHT (no display_name field).
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name: None,
            profile_picture: None,
        };
        let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
        let client = state.app_client_v1_0.lock().await;
        let client = client.as_ref().ok_or("v1.0 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    } else if dna_version == "1.1" {
        // Vote on a v1.1 poll — write to v1.1 DHT (no display_name field).
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name: None,
            profile_picture: None,
        };
        let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
        let client = state.app_client_v1_1.lock().await;
        let client = client.as_ref().ok_or("v1.1 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    } else {
        // v1.2 poll — include profile for public polls.
        let (display_name, profile_picture) = if poll_type.as_deref() == Some("Public") {
            let profile = load_cached_profile(&state.data_dir);
            let dn = profile.as_ref().and_then(|p| p.display_name.clone());
            let pp = profile.as_ref().and_then(|p| p.profile_picture.clone());
            (dn, pp)
        } else {
            (None, None)
        };
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name,
            profile_picture,
        };
        let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    };

    Ok(action_hash.to_string())
}

#[tauri::command]
pub async fn get_poll_votes(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    // "1.0", "1.1", or "1.2" — reads votes from the correct DHT cell.
    // Obtained from dna_version on PollListItem or PollDetail.
    dna_version: String,
) -> Result<Vec<VoteData>, String> {
    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;

    let records: Vec<Record> = if dna_version == "1.0" {
        let client = state.app_client_v1_0.lock().await;
        let client = client.as_ref().ok_or("v1.0 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    } else if dna_version == "1.1" {
        let client = state.app_client_v1_1.lock().await;
        let client = client.as_ref().ok_or("v1.1 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    } else {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let result = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    };

    let mut votes = Vec::new();
    for record in &records {
        let vote: Vote = decode_entry(record)?;
        votes.push(VoteData {
            vote: VoteResponse {
                poll_action_hash: vote.poll_action_hash.to_string(),
                option_index: vote.option_index,
            },
            author: record.action().author().to_string(),
            display_name: vote.display_name,
            profile_picture: vote.profile_picture,
        });
    }
    Ok(votes)
}

// ── Identity link persistence (Flowsta infrastructure — keep as-is) ───
//
// The identity link is persisted to a JSON file so the app can detect
// a previously-linked Vault user across restarts and DNA migrations.
// This data is separate from the DHT — it's local to this device.
//
// Two files work together:
//   - identity-link.json — stores vault_agent_pub_key + entry_action_hash
//     (proves this user linked with Flowsta; survives DNA migrations)
//   - profile-cache.json — stores display_name + profile_picture
//     (allows the app to show the user's identity without Vault running)

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct IdentityLinkData {
    pub vault_agent_pub_key: String,
    pub entry_action_hash: String,
    pub linked_at: i64,
}

fn identity_link_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("identity-link.json")
}

fn load_identity_link(data_dir: &std::path::Path) -> Option<IdentityLinkData> {
    let path = identity_link_path(data_dir);
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

fn save_identity_link(data_dir: &std::path::Path, data: &IdentityLinkData) {
    let path = identity_link_path(data_dir);
    if let Ok(json) = serde_json::to_string_pretty(data) {
        let _ = std::fs::write(path, json);
    }
}

// ── Profile persistence (survives Vault being closed/locked) ──────────
//
// The Flowsta Vault only needs to be running for the initial identity
// linking ceremony. After that, the display name and profile picture
// are cached locally in profile-cache.json. On subsequent startups,
// the app loads from cache immediately — no Vault dependency.
//
// The cache is refreshed whenever the Vault is available (layout.tsx
// polls /status every 3s). If the user changes their name or picture
// in the Vault, the cache updates automatically on next poll.

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CachedProfile {
    pub display_name: Option<String>,
    pub profile_picture: Option<String>,
}

fn profile_cache_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("profile-cache.json")
}

fn load_cached_profile(data_dir: &std::path::Path) -> Option<CachedProfile> {
    let path = profile_cache_path(data_dir);
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

fn save_cached_profile(data_dir: &std::path::Path, profile: &CachedProfile) {
    let path = profile_cache_path(data_dir);
    if let Ok(json) = serde_json::to_string_pretty(profile) {
        let _ = std::fs::write(path, json);
    }
}

fn delete_identity_link(data_dir: &std::path::Path) {
    let path = identity_link_path(data_dir);
    let _ = std::fs::remove_file(path);
}

#[tauri::command]
pub fn get_cached_profile(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Option<CachedProfile> {
    load_cached_profile(&state.data_dir)
}

#[tauri::command]
pub fn save_profile_cache(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    display_name: Option<String>,
    profile_picture: Option<String>,
) {
    save_cached_profile(
        &state.data_dir,
        &CachedProfile {
            display_name,
            profile_picture,
        },
    );
}

// ── Identity linking commands (Flowsta infrastructure — keep as-is) ───
//
// These commands handle the Flowsta Vault ↔ app identity linking ceremony.
// The Vault signs an attestation that this app's agent key belongs to the
// same person as the Vault's agent key. This attestation is stored on the
// DHT via the agent_linking zome so anyone can verify it.

#[tauri::command]
pub async fn commit_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    vault_agent_pub_key: String,
    vault_signature: String,
) -> Result<String, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let external_agent = parse_agent_pub_key_string(&vault_agent_pub_key)?;

    // Decode base64 signature to bytes.
    use base64::Engine;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&vault_signature)
        .map_err(|e| format!("Invalid signature: {}", e))?;

    #[derive(serde::Serialize, Debug)]
    struct CreateExternalLinkInput {
        external_agent: AgentPubKey,
        external_signature: Vec<u8>,
    }

    let input = CreateExternalLinkInput {
        external_agent,
        external_signature: sig_bytes,
    };
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "create_external_link", payload).await?;

    let action_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    let action_hash_str = action_hash.to_string();

    // Persist the link data for later revocation
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    save_identity_link(
        &state.data_dir,
        &IdentityLinkData {
            vault_agent_pub_key: vault_agent_pub_key.clone(),
            entry_action_hash: action_hash_str.clone(),
            linked_at: now,
        },
    );

    Ok(action_hash_str)
}

#[tauri::command]
pub fn get_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Option<IdentityLinkData> {
    load_identity_link(&state.data_dir)
}

#[tauri::command]
pub async fn revoke_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let Some(link_data) = load_identity_link(&state.data_dir) else {
        // Already unlinked locally — nothing to do.
        return Ok(());
    };

    // Call revoke_link on the agent_linking zome
    let action_hash = ActionHash::try_from(link_data.entry_action_hash.clone())
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let payload = ExternIO::encode(action_hash).map_err(|e| e.to_string())?;
    call_zome(client, AGENT_LINKING_ZOME, "revoke_link", payload).await?;

    // Delete local persistence
    delete_identity_link(&state.data_dir);

    // Best-effort: notify Vault via IPC
    let agent_key = state.agent_pub_key.lock().unwrap().clone();
    if let Some(agent_key) = agent_key {
        let _ = notify_vault_revoke("ProofPoll", &agent_key).await;
    }

    Ok(())
}

/// Best-effort notification to Vault that identity link was revoked.
async fn notify_vault_revoke(app_name: &str, app_agent_pub_key: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;

    let body = serde_json::json!({
        "app_name": app_name,
        "app_agent_pub_key": app_agent_pub_key,
    });

    let _ = client
        .post("http://127.0.0.1:27777/revoke-identity")
        .json(&body)
        .send()
        .await;

    Ok(())
}

/// Export this user's ProofPoll data for Vault auto-backup.
/// Only includes the user's own data (CAL compliance):
///   - Polls they created (with all votes for context)
///   - Their votes on other people's polls
///   - Cryptographic keys to recreate their identity
#[tauri::command]
pub async fn get_export_data(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let my_key = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // Fetch all polls from the DHT
    let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;
    let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;

    let mut my_polls = Vec::new();
    let mut my_votes = Vec::new();

    for record in &records {
        let poll: Poll = decode_entry(record)?;
        let hash = record.action_address().to_string();
        let author = record.action().author().to_string();
        let is_my_poll = author == my_key;

        // Fetch votes for this poll
        let vote_payload =
            ExternIO::encode(record.action_address().clone()).map_err(|e| e.to_string())?;
        let vote_result = call_zome(client, POLLS_ZOME, "get_poll_votes", vote_payload).await;

        let all_votes: Vec<(u32, String)> = match vote_result {
            Ok(vr) => {
                let vote_records: Vec<Record> = vr.decode().unwrap_or_default();
                vote_records
                    .iter()
                    .filter_map(|vr| {
                        let vote: Vote = decode_entry(vr).ok()?;
                        Some((vote.option_index, vr.action().author().to_string()))
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        // If I created this poll, include it with all its votes
        if is_my_poll {
            let votes_json: Vec<serde_json::Value> = all_votes
                .iter()
                .map(|(idx, voter)| serde_json::json!({
                    "option_index": idx,
                    "voter": voter,
                }))
                .collect();

            my_polls.push(serde_json::json!({
                "hash": hash,
                "title": poll.title,
                "description": poll.description,
                "options": poll.options,
                "created_at": poll.created_at,
                "closes_at": poll.closes_at,
                "total_votes": votes_json.len(),
                "votes": votes_json,
            }));
        }

        // If I voted on this poll (whether mine or someone else's), record it
        for (option_index, voter) in &all_votes {
            if voter == &my_key {
                my_votes.push(serde_json::json!({
                    "poll_hash": hash,
                    "poll_title": poll.title,
                    "option_index": option_index,
                    "option_text": poll.options.get(*option_index as usize),
                }));
            }
        }
    }

    // CAL compliance: include cryptographic key access information
    let passphrase = state.passphrase.lock().unwrap().clone();
    let lair_dir = state.data_dir.join("lair");

    // Include lair keystore data (store_file) for portable key backup.
    let store_file_path = lair_dir.join("store_file");
    let lair_keystore_data = if store_file_path.exists() {
        use base64::Engine;
        match std::fs::read(&store_file_path) {
            Ok(bytes) => Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(serde_json::json!({
        "_readme": "Your ProofPoll data. Only includes polls you created and votes you cast.",

        "format": {
            "version": 3,
            "exported_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        },

        "you": {
            "agent_pub_key": my_key,
        },

        "keys": {
            "_readme": "Your lair keystore contains the private signing key for your ProofPoll identity. The passphrase unlocks it. To restore: decode lair_keystore_data from base64, save as 'store_file' in a lair directory, and run lair-keystore with the passphrase.",
            "lair_passphrase": passphrase,
            "lair_keystore_data": lair_keystore_data,
        },

        "polls_created": {
            "count": my_polls.len(),
            "polls": my_polls,
        },

        "votes_cast": {
            "count": my_votes.len(),
            "votes": my_votes,
        },
    }))
}

#[tauri::command]
pub async fn get_linked_agents(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    agent_pub_key: String,
) -> Result<Vec<String>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let agent = AgentPubKey::try_from(agent_pub_key)
        .map_err(|e| format!("Invalid agent key: {:?}", e))?;
    let payload = ExternIO::encode(agent).map_err(|e| e.to_string())?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "get_linked_agents", payload).await?;

    let agents: Vec<AgentPubKey> = result.decode().map_err(|e| e.to_string())?;
    Ok(agents.iter().map(|a| a.to_string()).collect())
}

// ── Flag types and commands (v1.1 — replace with your moderation system) ──
//
// Community flagging: users can flag content with a reason. The UI hides
// content that exceeds a configurable flag threshold. Data is never deleted
// from the DHT — censorship resistance is preserved at the data layer,
// moderation happens at the UI layer.

#[derive(serde::Serialize, Clone)]
pub struct FlagData {
    pub hash: String,
    pub flag: FlagResponse,
    pub author: String,
}

#[derive(serde::Serialize, Clone)]
pub struct FlagResponse {
    pub poll_action_hash: String,
    pub reason: String,
    pub created_at: i64,
}

#[derive(serde::Deserialize)]
struct FlagEntry {
    poll_action_hash: ActionHash,
    reason: FlagReason,
    created_at: i64,
}

#[derive(serde::Deserialize, Debug)]
enum FlagReason {
    Spam,
    Misleading,
    OffTopic,
    Inappropriate,
}

impl std::fmt::Display for FlagReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlagReason::Spam => write!(f, "Spam"),
            FlagReason::Misleading => write!(f, "Misleading"),
            FlagReason::OffTopic => write!(f, "OffTopic"),
            FlagReason::Inappropriate => write!(f, "Inappropriate"),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct FlagPollInput {
    poll_action_hash: ActionHash,
    reason: String,
}

#[tauri::command]
pub async fn flag_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    reason: String,
) -> Result<String, String> {
    // Require a linked Flowsta identity — frontend gating alone is bypassable.
    if load_identity_link(&state.data_dir).is_none() {
        return Err("Sign in with Flowsta to flag polls".to_string());
    }

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    #[derive(serde::Serialize, Debug)]
    struct ZomeFlagInput {
        poll_action_hash: ActionHash,
        reason: serde_json::Value,
    }

    let zome_input = ZomeFlagInput {
        poll_action_hash: hash,
        reason: serde_json::Value::String(reason),
    };

    let payload = ExternIO::encode(zome_input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "flag_poll", payload).await?;

    let action_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(action_hash.to_string())
}

#[tauri::command]
pub async fn get_poll_flags(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
) -> Result<Vec<FlagData>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_poll_flags", payload).await?;

    let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;
    let mut flags = Vec::new();
    for record in &records {
        let flag: FlagEntry = decode_entry(record)?;
        flags.push(FlagData {
            hash: record.action_address().to_string(),
            flag: FlagResponse {
                poll_action_hash: flag.poll_action_hash.to_string(),
                reason: format!("{}", flag.reason),
                created_at: flag.created_at,
            },
            author: record.action().author().to_string(),
        });
    }
    Ok(flags)
}

#[tauri::command]
pub async fn remove_flag(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    flag_action_hash: String,
) -> Result<String, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash = ActionHash::try_from(flag_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "remove_flag", payload).await?;

    let delete_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(delete_hash.to_string())
}

#[tauri::command]
pub async fn get_flag_threshold(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u32, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_flag_threshold", payload).await?;

    let threshold: u32 = result.decode().map_err(|e| e.to_string())?;
    Ok(threshold)
}

// ── Migration status commands (infrastructure — keep as-is) ───────────
//
// These let the frontend show migration progress to the user.
// The actual migration logic is in migration.rs.

#[tauri::command]
pub async fn get_migration_status(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<crate::migration::MigrationState, String> {
    let state = state.migration_state.lock().await;
    Ok(state.clone())
}

#[tauri::command]
pub async fn abandon_pending_votes(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let mut migration = state.migration_state.lock().await;
    migration.votes_pending.clear();
    migration.save(&state.data_dir);
    Ok(())
}
