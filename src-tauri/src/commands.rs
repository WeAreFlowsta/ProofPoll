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
use std::path::{Path, PathBuf};
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

/// Mirrors PollType in the v1.2 integrity zome.
///
/// Must use the same serde representation so `decode_entry` works: rmp_serde
/// encodes unit enum variants as externally-tagged maps `{"Public": nil}`, not
/// as strings. Using a matching enum here round-trips correctly. Tauri then
/// re-serialises to JSON (human-readable) which produces the string `"Public"`
/// that the frontend already expects.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum PollType {
    Anonymous,
    Public,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    /// None for v1.0/v1.1 polls (no poll_type field — treated as Anonymous).
    #[serde(default)]
    pub poll_type: Option<PollType>,
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
    pub hash: String,
    pub poll_action_hash: String,
    pub option_index: u32,
}

// --- App state ---

pub struct AppState {
    pub data_dir: PathBuf,
    pub conductor_handle: Mutex<Option<ConductorHandle>>,
    pub conductor_status: Mutex<ConductorStatus>,
    pub agent_pub_key: Mutex<Option<String>>,
    /// Active (v1.3) app client for all reads and writes.
    pub app_client: tokio::sync::Mutex<Option<AppWebsocket>>,
    /// v1.2 app client for migration reads (v1.2 → v1.3).
    pub app_client_v1_2: tokio::sync::Mutex<Option<AppWebsocket>>,
    /// v1.1 app client for legacy reads.
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
        let lair_config_path = data_dir.join("lair").join("lair-keystore-config.yaml");
        let lair_store_path = data_dir.join("lair").join("store_file");
        let conductor_dir = data_dir.join("conductor");

        // These three files are encryption-paired: lair-passphrase derives the
        // crypto key, the config holds matching salts, store_file is encrypted
        // under them. Either ALL three are present (a working install) or ALL
        // three are absent (a true fresh install). Any other combination means
        // an uninstall+reinstall removed some files but not others, leaving
        // orphaned state encrypted under a passphrase or salts that no longer
        // exist — every attempt to start lair from that state crashes with
        // `sqlcipher_page_cipher: hmac check failed`. Detect that mismatch
        // here and wipe everything for a clean restart.
        //
        // Nothing user-recoverable lives in either dir: agent keys are
        // regenerated every install, and user-authored polls/votes come back
        // through the Vault backup restore flow on next sign-in.
        let pp = passphrase_path.exists();
        let cfg = lair_config_path.exists();
        let store = lair_store_path.exists();
        let all_present = pp && cfg && store;
        let all_absent = !pp && !cfg && !store;
        if !all_present && !all_absent {
            log::warn!(
                "Inconsistent lair state on startup (lair-passphrase={}, config={}, store_file={}). Wiping lair + conductor data dirs to recover.",
                pp, cfg, store,
            );
            let _ = std::fs::remove_file(&passphrase_path);
            let _ = std::fs::remove_dir_all(data_dir.join("lair"));
            let _ = std::fs::remove_dir_all(&conductor_dir);
        }

        let passphrase = if passphrase_path.exists() {
            std::fs::read_to_string(&passphrase_path).unwrap_or_else(|_| generate_passphrase())
        } else {
            let p = generate_passphrase();
            if let Err(e) = std::fs::write(&passphrase_path, &p) {
                log::error!("Failed to persist lair passphrase to {:?}: {} — next launch will regenerate and wipe state", passphrase_path, e);
            } else {
                log::info!("Generated new lair passphrase at {:?}", passphrase_path);
            }
            p
        };

        let migration_state = crate::migration::MigrationState::load(&data_dir);

        Self {
            data_dir,
            conductor_handle: Mutex::new(None),
            conductor_status: Mutex::new(ConductorStatus::Stopped),
            agent_pub_key: Mutex::new(None),
            app_client: tokio::sync::Mutex::new(None),
            app_client_v1_2: tokio::sync::Mutex::new(None),
            app_client_v1_1: tokio::sync::Mutex::new(None),
            app_client_v1_0: tokio::sync::Mutex::new(None),
            passphrase: Mutex::new(passphrase),
            lair_client: tokio::sync::Mutex::new(None),
            migration_state: tokio::sync::Mutex::new(migration_state),
        }
    }
}

/// Wipe every encryption-paired file under data_dir, generate a fresh
/// random passphrase, persist it, and update the AppState mutex so any
/// subsequent code path sees the new value. Returns the new passphrase
/// for the caller to use immediately.
///
/// Called from `conductor::start_holochain` when the first attempt fails
/// with a SQLCipher hmac mismatch — the only recoverable response to
/// that error is to drop the orphaned encrypted state and start fresh.
/// Nothing user-recoverable lives in either directory: agent keys are
/// regenerated every install, and user-authored polls/votes come back
/// through the Vault backup restore flow on next sign-in.
pub fn nuke_state_and_regenerate_passphrase(data_dir: &Path, app_state: &AppState) -> String {
    log::warn!(
        "Performing full state reset under {:?} (lair-passphrase + lair/ + conductor/)",
        data_dir,
    );
    let passphrase_path = data_dir.join("lair-passphrase");
    if let Err(e) = std::fs::remove_file(&passphrase_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("remove lair-passphrase: {}", e);
        }
    }
    if let Err(e) = std::fs::remove_dir_all(data_dir.join("lair")) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("remove_dir_all lair: {}", e);
        }
    }
    if let Err(e) = std::fs::remove_dir_all(data_dir.join("conductor")) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("remove_dir_all conductor: {}", e);
        }
    }

    let new_passphrase = generate_passphrase();
    if let Err(e) = std::fs::write(&passphrase_path, &new_passphrase) {
        log::error!(
            "Failed to write regenerated lair-passphrase to {:?}: {}",
            passphrase_path, e,
        );
    }

    *app_state.passphrase.lock().unwrap() = new_passphrase.clone();
    log::info!("State reset complete; new passphrase persisted and in-memory");
    new_passphrase
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
pub(crate) fn parse_agent_pub_key_string(s: &str) -> Result<AgentPubKey, String> {
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

    log::info!("get_app_status called: ready={}", ready);

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

    // Try v1.3 first — most polls will be here for current users.
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
                    dna_version: "1.3".to_string(),
                }));
            }
        }
    } // v1.3 lock released

    // Fall back to v1.2.
    {
        let client = state.app_client_v1_2.lock().await;
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

    // Fall back to v1.1.
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
    // Step 1: Fetch polls from the active version (always available).
    let mut polls = Vec::new();
    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;
        let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;
        for record in &records {
            if let Ok(poll) = decode_entry::<Poll>(record) {
                polls.push(PollListItem {
                    hash: record.action_address().to_string(),
                    poll,
                    author: record.action().author().to_string(),
                    dna_version: "1.3".to_string(),
                });
            }
        }
    }

    // Step 2: Collect ALL migrated hashes from every version that has
    // migration mappings. A hash in this set means the poll was migrated
    // to a newer version — don't show it from the older version.
    let mut migrated_hashes = std::collections::HashSet::<String>::new();

    // Collect from active version (maps previous→active)
    {
        let client = state.app_client.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            if let Ok(r) = call_zome(client, POLLS_ZOME, "get_all_migration_mappings", payload).await {
                let records: Vec<Record> = r.decode().unwrap_or_default();
                for rec in &records {
                    if let Ok(entry) = decode_entry::<MigratedPollEntry>(rec) {
                        migrated_hashes.insert(entry.old_action_hash.to_string());
                    }
                }
            }
        }
    }

    // Collect from each older version (chains the dedup across all hops)
    for older_client_mutex in [&state.app_client_v1_2, &state.app_client_v1_1] {
        let client = older_client_mutex.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            if let Ok(r) = call_zome(client, POLLS_ZOME, "get_all_migration_mappings", payload).await {
                let records: Vec<Record> = r.decode().unwrap_or_default();
                for rec in &records {
                    if let Ok(entry) = decode_entry::<MigratedPollEntry>(rec) {
                        migrated_hashes.insert(entry.old_action_hash.to_string());
                    }
                }
            }
        }
    }

    // Step 3: Fetch polls from older versions, skipping any that have been migrated.
    // Each version is checked independently — order doesn't matter since we
    // have the complete set of migrated hashes.
    let older_versions: Vec<(&tokio::sync::Mutex<Option<AppWebsocket>>, &str)> = vec![
        (&state.app_client_v1_2, "1.2"),
        (&state.app_client_v1_1, "1.1"),
        (&state.app_client_v1_0, "1.0"),
    ];

    for (client_mutex, version) in &older_versions {
        let client = client_mutex.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
            match call_zome(client, POLLS_ZOME, "get_all_polls", payload).await {
                Ok(result) => {
                    let records: Vec<Record> = result.decode().unwrap_or_default();
                    for record in &records {
                        let hash = record.action_address().to_string();
                        if migrated_hashes.contains(&hash) {
                            continue; // already migrated to a newer version
                        }
                        if let Ok(poll) = decode_entry::<Poll>(record) {
                            polls.push(PollListItem {
                                hash,
                                poll,
                                author: record.action().author().to_string(),
                                dna_version: version.to_string(),
                            });
                        }
                    }
                }
                Err(e) => log::warn!("Could not fetch {} polls (skipping): {}", version, e),
            }
        }
    }

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
    log::info!(
        "cast_vote: dna_version={}, poll_type={:?}, option_index={}",
        dna_version, poll_type, option_index,
    );
    // Require a linked Flowsta identity — frontend gating alone is bypassable.
    if load_identity_link(&state.data_dir).is_none() {
        log::error!("cast_vote: rejected — no identity link in data dir");
        return Err("Sign in with Flowsta to vote".to_string());
    }
    let cached = load_cached_profile(&state.data_dir);
    log::info!(
        "cast_vote: identity link present, cached profile: display_name={:?}, has_picture={}",
        cached.as_ref().and_then(|p| p.display_name.as_deref()),
        cached.as_ref().and_then(|p| p.profile_picture.as_deref()).is_some(),
    );

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
    } else if dna_version == "1.2" {
        // Vote on a v1.2 poll — include profile for public polls.
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
        let client = state.app_client_v1_2.lock().await;
        let client = client.as_ref().ok_or("v1.2 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    } else {
        // v1.3 poll — include profile for public polls.
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
    } else if dna_version == "1.2" {
        let client = state.app_client_v1_2.lock().await;
        let client = client.as_ref().ok_or("v1.2 conductor not available")?;
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
                hash: record.action_address().to_string(),
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

/// **DEPRECATED (v0.2.0+).** Replaced by the SDK's `dumpCellStateForBackup`
/// (which goes through `decode_record_for_export` below). This function is
/// kept only so the existing `startAutoBackup({ getData })` wiring in
/// `routes/layout.tsx` keeps working while we transition; once the new
/// `startAutoBackup({ adminWebsocket, cellIds, decodeRecordForExport })`
/// signature ships in production this function should be removed.
///
/// Export this user's ProofPoll data for Vault auto-backup.
/// Only includes the user's own data (CAL compliance):
///   - Polls they created (with all votes for context)
///   - Their votes on other people's polls
///   - Cryptographic keys to recreate their identity
#[tauri::command]
pub async fn get_export_data(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let my_key = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // Fetch all polls and votes from the active DHT
    let mut my_polls = Vec::new();
    let mut my_votes = Vec::new();
    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;

        let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;
        let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;

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
    } // app_client lock released

    // Fetch encrypted private data (vote rationales + drafts)

    let agent_bytes = get_agent_ed25519_bytes(&state)?;
    let mut my_rationales = Vec::new();
    let mut my_drafts = Vec::new();

    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let lair = state.lair_client.lock().await;
        let lair = lair.as_ref().ok_or("Lair not connected")?;

        // Fetch vote rationales for each of my votes
        for vote_json in &my_votes {
            if let Some(poll_hash) = vote_json.get("poll_hash").and_then(|h| h.as_str()) {
                // Get all votes for this poll to find mine
                if let Ok(hash) = ActionHash::try_from(poll_hash.to_string()) {
                    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
                    if let Ok(result) = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await {
                        let vote_records: Vec<Record> = result.decode().unwrap_or_default();
                        for vr in &vote_records {
                            if vr.action().author().to_string() == my_key {
                                let vote_hash = vr.action_address().clone();
                                let rat_payload = ExternIO::encode(vote_hash).map_err(|e| e.to_string())?;
                                if let Ok(rat_result) = call_zome(client, POLLS_ZOME, "get_vote_rationale", rat_payload).await {
                                    let rat_record: Option<Record> = rat_result.decode().unwrap_or(None);
                                    if let Some(rec) = rat_record {
                                        if let Ok(ee) = decode_entry::<EncryptedEntryData>(&rec) {
                                            let mut nonce = [0u8; 24];
                                            if ee.nonce.len() == 24 {
                                                nonce.copy_from_slice(&ee.nonce);
                                                if let Ok(plaintext) = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await {
                                                    if let Ok(text) = String::from_utf8(plaintext) {
                                                        my_rationales.push(serde_json::json!({
                                                            "poll_title": vote_json.get("poll_title"),
                                                            "voted_for": vote_json.get("option_text"),
                                                            "rationale": text,
                                                        }));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fetch and decrypt draft polls
        let drafts_payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
        if let Ok(drafts_result) = call_zome(client, POLLS_ZOME, "get_my_drafts", drafts_payload).await {
            let draft_records: Vec<Record> = drafts_result.decode().unwrap_or_default();
            for record in &draft_records {
                if let Ok(ee) = decode_entry::<EncryptedEntryData>(record) {
                    let mut nonce = [0u8; 24];
                    if ee.nonce.len() == 24 {
                        nonce.copy_from_slice(&ee.nonce);
                        if let Ok(plaintext) = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await {
                            if let Ok(draft) = serde_json::from_slice::<serde_json::Value>(&plaintext) {
                                my_drafts.push(draft);
                            }
                        }
                    }
                }
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

    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(serde_json::json!({
        "_readme": concat!(
            "Your complete ProofPoll data export. Includes polls you created, ",
            "votes you cast, private vote rationales (decrypted), and draft polls (decrypted). ",
            "Your cryptographic keys are included for full data portability (CAL compliance).",
        ),

        "format": {
            "version": 4,
            "exported_at": exported_at,
        },

        "you": {
            "agent_pub_key": my_key,
        },

        "keys": {
            "_readme": concat!(
                "Your lair keystore contains the private signing key for your ProofPoll identity. ",
                "The passphrase unlocks it. To restore: decode lair_keystore_data from base64, ",
                "save as 'store_file' in a lair directory, and run lair-keystore with the passphrase.",
            ),
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

        "private_data": {
            "_readme": concat!(
                "Your private data, decrypted from encrypted entries on the DHT. ",
                "On the network, this data is stored as opaque ciphertext that only you can read. ",
                "This export includes the decrypted plaintext for your records.",
            ),

            "vote_rationales": {
                "count": my_rationales.len(),
                "rationales": my_rationales,
            },

            "draft_polls": {
                "count": my_drafts.len(),
                "drafts": my_drafts,
            },
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

    let agent = AgentPubKey::try_from(agent_pub_key.clone())
        .map_err(|e| format!("Invalid agent key: {:?}", e))?;
    let payload = ExternIO::encode(agent).map_err(|e| e.to_string())?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "get_linked_agents", payload).await?;

    let agents: Vec<AgentPubKey> = result.decode().map_err(|e| e.to_string())?;
    let out: Vec<String> = agents.iter().map(|a| a.to_string()).collect();
    // Diagnostic: surface what the link graph returns so cross-device identity
    // recognition can be verified from the log rather than assumed.
    log::info!(
        "[identity] get_linked_agents({}) -> {} agent(s): {:?}",
        agent_pub_key, out.len(), out,
    );
    Ok(out)
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

// ── Encrypted entry commands (v1.3) ──────────────────────────────────

/// Extract the raw 32-byte Ed25519 public key from the agent key string.
/// AgentPubKey is 39 bytes: 3-byte prefix + 32-byte key + 4-byte DHT location.
fn get_agent_ed25519_bytes(state: &AppState) -> Result<[u8; 32], String> {
    let key_str = state
        .agent_pub_key
        .lock()
        .unwrap()
        .clone()
        .ok_or("Agent key not available")?;
    let agent_key = parse_agent_pub_key_string(&key_str)?;
    let raw = agent_key.get_raw_39();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&raw[3..35]);
    Ok(bytes)
}

/// Encrypted entry as decoded from a DHT Record.
#[derive(serde::Deserialize)]
struct EncryptedEntryData {
    cipher: Vec<u8>,
    nonce: Vec<u8>,
    entry_type_hint: String,
    related_hash: Option<ActionHash>,
}

/// Draft poll as returned to the frontend (decrypted).
#[derive(serde::Serialize)]
pub struct DraftPollItem {
    pub hash: String,
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
    pub poll_type: String,
    pub created_at: i64,
}

/// Save a private vote rationale (encrypted on the DHT).
#[tauri::command]
pub async fn save_vote_rationale(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    vote_action_hash: String,
    rationale_text: String,
) -> Result<String, String> {
    let agent_bytes = get_agent_ed25519_bytes(&state)?;

    // Encrypt the rationale — acquire lair first, then app client (lock ordering)
    let (nonce, cipher) = {
        let lair = state.lair_client.lock().await;
        let lair = lair.as_ref().ok_or("Lair not connected")?;
        crate::crypto::encrypt_to_self(lair, agent_bytes, rationale_text.as_bytes()).await?
    };

    let vote_hash = parse_action_hash(&vote_action_hash)?;

    #[derive(serde::Serialize, Debug)]
    struct Input {
        cipher: Vec<u8>,
        nonce: Vec<u8>,
        link_as: String,
        related_hash: Option<ActionHash>,
    }

    let input = Input {
        cipher,
        nonce: nonce.to_vec(),
        link_as: "vote_rationale".to_string(),
        related_hash: Some(vote_hash),
    };

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "create_encrypted_entry", payload).await?;
    let hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(hash.to_string())
}

/// Get and decrypt a vote rationale.
#[tauri::command]
pub async fn get_vote_rationale(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    vote_action_hash: String,
) -> Result<Option<String>, String> {
    let vote_hash = parse_action_hash(&vote_action_hash)?;

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = ExternIO::encode(vote_hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_vote_rationale", payload).await?;
    let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;

    let record = match record {
        Some(r) => r,
        None => return Ok(None),
    };

    // Only decrypt if we're the author
    let my_agent = state.agent_pub_key.lock().unwrap().clone();
    if my_agent.as_deref() != Some(&record.action().author().to_string()) {
        return Ok(None);
    }

    let ee: EncryptedEntryData = decode_entry(&record)?;
    let agent_bytes = get_agent_ed25519_bytes(&state)?;

    let mut nonce = [0u8; 24];
    if ee.nonce.len() != 24 {
        return Err("Invalid nonce length".into());
    }
    nonce.copy_from_slice(&ee.nonce);

    let lair = state.lair_client.lock().await;
    let lair = lair.as_ref().ok_or("Lair not connected")?;
    let plaintext = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await?;
    let text = String::from_utf8(plaintext).map_err(|e| format!("Invalid UTF-8: {}", e))?;
    Ok(Some(text))
}

/// Save a draft poll (encrypted on the DHT).
#[tauri::command]
pub async fn save_draft_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    poll_type: Option<String>,
) -> Result<String, String> {
    let agent_bytes = get_agent_ed25519_bytes(&state)?;

    let draft = serde_json::json!({
        "title": title,
        "description": description,
        "options": options,
        "closes_at": closes_at,
        "poll_type": poll_type.unwrap_or_else(|| "Anonymous".to_string()),
    });
    let plaintext = serde_json::to_vec(&draft).map_err(|e| e.to_string())?;

    let (nonce, cipher) = {
        let lair = state.lair_client.lock().await;
        let lair = lair.as_ref().ok_or("Lair not connected")?;
        crate::crypto::encrypt_to_self(lair, agent_bytes, &plaintext).await?
    };
    #[derive(serde::Serialize, Debug)]
    struct Input {
        cipher: Vec<u8>,
        nonce: Vec<u8>,
        link_as: String,
        related_hash: Option<ActionHash>,
    }

    let input = Input {
        cipher,
        nonce: nonce.to_vec(),
        link_as: "draft_poll".to_string(),
        related_hash: None,
    };

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "create_encrypted_entry", payload).await?;
    let hash: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(hash.to_string())
}

/// Get all draft polls (decrypted).
#[tauri::command]
pub async fn get_my_drafts(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<DraftPollItem>, String> {
    log::info!("get_my_drafts: fetching from DHT...");
    let records: Vec<Record> = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_my_drafts", payload).await?;
        result.decode().map_err(|e| e.to_string())?
    }; // app_client lock released here

    log::info!("get_my_drafts: {} records found, decrypting...", records.len());
    let agent_bytes = get_agent_ed25519_bytes(&state)?;
    let lair = state.lair_client.lock().await;
    let lair = lair.as_ref().ok_or("Lair not connected")?;

    let mut drafts = Vec::new();
    for record in &records {
        let hash = record.action_address().to_string();
        let created_at = record.action().timestamp().as_seconds_and_nanos().0;

        let ee: EncryptedEntryData = match decode_entry(record) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mut nonce = [0u8; 24];
        if ee.nonce.len() != 24 {
            continue;
        }
        nonce.copy_from_slice(&ee.nonce);

        let plaintext = match crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to decrypt draft {}: {}", hash, e);
                continue;
            }
        };

        let draft: serde_json::Value = match serde_json::from_slice(&plaintext) {
            Ok(v) => v,
            Err(_) => continue,
        };

        drafts.push(DraftPollItem {
            hash,
            title: draft["title"].as_str().unwrap_or("").to_string(),
            description: draft["description"].as_str().unwrap_or("").to_string(),
            options: draft["options"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            closes_at: draft["closes_at"].as_i64(),
            poll_type: draft["poll_type"].as_str().unwrap_or("Anonymous").to_string(),
            created_at,
        });
    }

    Ok(drafts)
}

/// Publish a draft: decrypt it, create a real poll, delete the draft.
#[tauri::command]
pub async fn publish_draft(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    draft_action_hash: String,
) -> Result<String, String> {
    let draft_hash = parse_action_hash(&draft_action_hash)?;
    let agent_bytes = get_agent_ed25519_bytes(&state)?;

    // Fetch and decrypt the draft
    let (draft_json, _ee) = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = ExternIO::encode(draft_hash.clone()).map_err(|e| e.to_string())?;
        let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
        let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;
        let record = record.ok_or("Draft not found")?;

        let ee: EncryptedEntryData = decode_entry(&record)?;
        let mut nonce = [0u8; 24];
        if ee.nonce.len() != 24 {
            return Err("Invalid nonce".into());
        }
        nonce.copy_from_slice(&ee.nonce);

        let lair = state.lair_client.lock().await;
        let lair = lair.as_ref().ok_or("Lair not connected")?;
        let plaintext = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await?;
        let json: serde_json::Value = serde_json::from_slice(&plaintext)
            .map_err(|e| format!("Failed to parse draft: {}", e))?;
        (json, ee)
    };

    // Create the real poll using the existing CreatePollInput struct
    let input = CreatePollInput {
        title: draft_json["title"].as_str().unwrap_or("").to_string(),
        description: draft_json["description"].as_str().unwrap_or("").to_string(),
        options: draft_json["options"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        closes_at: draft_json["closes_at"].as_i64(),
        poll_type: Some(draft_json["poll_type"].as_str().unwrap_or("Anonymous").to_string()),
    };

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "create_poll", payload).await?;
    let poll_hash: ActionHash = result.decode().map_err(|e| e.to_string())?;

    // Delete the draft
    let delete_payload = ExternIO::encode(draft_hash).map_err(|e| e.to_string())?;
    if let Err(e) = call_zome(client, POLLS_ZOME, "delete_encrypted_entry", delete_payload).await {
        log::warn!("Failed to delete draft after publishing: {}", e);
    }

    Ok(poll_hash.to_string())
}

/// Delete a draft poll.
#[tauri::command]
pub async fn delete_draft(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    draft_action_hash: String,
) -> Result<String, String> {
    let hash = parse_action_hash(&draft_action_hash)?;
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "delete_encrypted_entry", payload).await?;
    let deleted: ActionHash = result.decode().map_err(|e| e.to_string())?;
    Ok(deleted.to_string())
}

/// Parse an action hash string into an ActionHash.
fn parse_action_hash(s: &str) -> Result<ActionHash, String> {
    ActionHash::try_from(s.to_string()).map_err(|e| format!("Invalid action hash: {:?}", e))
}

// ── CAL-compliant backup helpers (v0.2.0+) ────────────────────────
//
// `decode_record_for_export` decodes an entry by type into plain JSON so the
// Flowsta Vault can embed a human-readable view of each record in the user's
// data export — what the Cryptographic Autonomy License (§4.2.1) obliges
// every Holochain app to provide.
//
// `restore_record` is the symmetric path: given a stored entry, dispatch to
// the appropriate zome function to re-create the entry on the current cell.
// This is called by the SDK's `restoreFromVault` when the user restores from
// a backup on a fresh install.
//
// Adding a new entry type to ProofPoll means adding ONE `match` arm in each
// of these functions, mirroring the entry type's existing structs.

use base64::Engine as _;

/// Decode an entry's MessagePack bytes into a human-readable JSON view, used
/// when the SDK builds a backup for Vault. Each arm uses the existing local
/// struct (Poll / Vote) so all fields stay in sync with the DNA via serde.
#[tauri::command]
pub async fn decode_record_for_export(
    entry_type: String,
    entry_bytes_b64: String,
) -> Result<serde_json::Value, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&entry_bytes_b64)
        .map_err(|e| format!("base64 decode: {}", e))?;

    match entry_type.as_str() {
        "Poll" => {
            let poll: Poll = rmp_serde::from_slice(&bytes)
                .map_err(|e| format!("Poll decode: {}", e))?;
            serde_json::to_value(poll).map_err(|e| e.to_string())
        }
        "Vote" => {
            let vote: Vote = rmp_serde::from_slice(&bytes)
                .map_err(|e| format!("Vote decode: {}", e))?;
            serde_json::to_value(vote).map_err(|e| e.to_string())
        }
        other => Ok(serde_json::json!({
            "_warning": format!("Unknown entry type: {}", other),
            "raw_bytes_hex": hex::encode(&bytes),
        })),
    }
}

// Note: the old per-entry `restore_record` dispatcher was removed in favour of
// source-chain `graft_records` (see `recovery.rs` + `conductor.rs`). Graft
// rebuilds the chain with the original action hashes and signatures preserved
// — no new writes, no DHT duplicates — which the dispatcher approach could not
// achieve.

/// Build the user's canonical-shape backup payload for Vault.
///
/// Replaces `get_export_data` for the Vault auto-backup path. Produces the
/// same payload shape the SDK's `dumpCellStateForBackup` would produce, so
/// the Flowsta Vault recognises it as canonical and:
///   - persists a per-entry-type summary alongside the backup metadata
///     (rendered as "12 polls, 38 votes" on the Your Data and overview UIs),
///   - inlines the `human_readable` view of each record into the user's CAL
///     §4.2.1 data export (full or per-app), and
///   - leaves the signed `raw_record` inside the encrypted backup.
///
/// Architecture note: this captures the user's ENTIRE source chain via the
/// admin `dump_full_state`, not just Poll/Vote app entries — so the CAL export
/// is the user's complete, signed chain. Each `raw_record` is a verbatim
/// `SourceChainDumpRecord` (a fully portable signed record).
///
/// On top of the raw records we layer the human-readable view + per-type
/// counts for the app entries we can decode (Poll, Vote) — this is what Vault
/// renders on the Your Data page and inlines into the CAL §4.2.1 export.
/// Infrastructure records (Dna, AgentValidationPkg, CapGrant, …) carry their
/// raw_record but no human_readable (they're chain plumbing, not user data).
///
/// The three lair files are appended at the top level so the backup is also
/// CAL-complete: data PLUS the cryptographic keys needed to operate it
/// independently. See `read_lair_backup_fields`.
#[tauri::command]
pub async fn build_canonical_backup(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    use holochain_client::{AdminWebsocket, CellInfo};

    let my_key = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // 1. Connect to the admin websocket (same pattern as try_reenable_app).
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", crate::conductor::ADMIN_WS_PORT),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WS for backup: {}", e))?;

    // 2. Find the active (v1.3) provisioned cell.
    let apps = admin_ws
        .list_apps(None)
        .await
        .map_err(|e| format!("list_apps: {}", e))?;
    let cell_id = apps
        .iter()
        .find(|a| a.installed_app_id == crate::dna::ACTIVE_APP_ID)
        .and_then(|app| {
            app.cell_info
                .values()
                .flat_map(|cells| cells.iter())
                .find_map(|c| match c {
                    CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                    _ => None,
                })
        })
        .ok_or("Active app cell not found for backup")?;

    // 3. Dump the full source chain (every action authored by this agent).
    let dump = admin_ws
        .dump_full_state(cell_id, None)
        .await
        .map_err(|e| format!("dump_full_state: {}", e))?;

    // 4. Build canonical records + per-type counts.
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    for rec in &dump.source_chain_dump.records {
        let action_hash = rec.action_address.to_string();
        let created_ms = rec.action.timestamp().as_millis();

        let (entry_type_name, human_readable) = classify_dump_record(rec);

        // Count only the decodable user-data entry types (those with a
        // non-null human_readable view). Infrastructure records are kept in
        // `records` for graft but don't inflate the user-facing summary.
        if !human_readable.is_null() {
            *counts.entry(entry_type_name.clone()).or_insert(0) += 1;
        }

        let raw_record = serde_json::to_value(rec)
            .map_err(|e| format!("serialize SourceChainDumpRecord: {}", e))?;

        records.push(serde_json::json!({
            "entryType": entry_type_name,
            "actionHash": action_hash,
            "createdAtMs": created_ms,
            "human_readable": human_readable,
            "raw_record": raw_record,
        }));
    }

    let total_records: usize = counts.values().sum();
    let counts_json: serde_json::Map<String, serde_json::Value> = counts
        .into_iter()
        .map(|(k, v)| (k, serde_json::json!(v)))
        .collect();

    // 5. Assemble the canonical payload.
    let mut payload = serde_json::json!({
        "version": 1,
        "_readme": "Your ProofPoll data, backed up automatically by Flowsta Vault. Encrypted with your device key — only you can read it. Each record below carries a plain-English view of what you authored AND a signed Holochain record for restore. The lair_* fields are the cryptographic keys that let you recover this identity on a fresh install.",
        "license": "Cryptographic Autonomy License v1.0 (CAL-1.0)",
        "app": {
            "name": "ProofPoll",
        },
        "agent_pub_key": my_key,
        "exported_at_iso": format!("unix:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ),
        "_summary": {
            "countsByEntryType": counts_json,
            "totalRecords": total_records,
        },
        "cells": [
            {
                "role_name": "polls",
                "_readme": "Each record below is one action on your source chain. `human_readable` is the plain-English view (app entries only). `raw_record` is the signed record used to restore your chain onto a fresh install.",
                "records": records,
            }
        ],
    });

    // 6. Append the lair recovery fields (data + keys = CAL-complete). If any
    //    of the three files is missing we omit all three — a partial set is
    //    useless for recovery and we'd rather ship a data-only backup than a
    //    misleading one.
    if let Some((passphrase, config_yaml, store_b64)) =
        read_lair_backup_fields(&state.data_dir)
    {
        payload["lair_passphrase"] = serde_json::json!(passphrase);
        payload["lair_keystore_config"] = serde_json::json!(config_yaml);
        payload["lair_keystore_data"] = serde_json::json!(store_b64);
    } else {
        log::warn!("build_canonical_backup: lair files incomplete; shipping data-only backup (no recovery fields)");
    }

    Ok(payload)
}

/// Classify a dumped source-chain record into a (entryType, human_readable)
/// pair for the canonical backup. App entries we can decode get a type name +
/// plain-JSON view; everything else gets an action-type label and a null
/// human_readable (kept in the backup for graft, not shown as user data).
///
/// For forking developers: the `(zome_index, entry_index)` table below mirrors
/// your DNA manifest's integrity-zome order and each integrity zome's
/// `EntryTypes` enum order. Update it alongside your entry types — it is the
/// one place that maps on-chain entry indices back to readable names.
///
/// ProofPoll v1.3 (`dna/v1.3/workdir/dna.yaml`):
///   integrity zomes: [0] agent_linking_integrity, [1] polls_integrity
///   polls_integrity EntryTypes: [0] Poll [1] Vote [2] Flag [3] MigratedPoll [4] EncryptedEntry
fn classify_dump_record(
    rec: &holochain_state_types::SourceChainDumpRecord,
) -> (String, serde_json::Value) {
    use holochain_integrity_types::EntryType;

    let entry_type = match rec.action.entry_type() {
        Some(et) => et,
        // No entry reference — a system action (Dna, AgentValidationPkg,
        // InitZomesComplete, OpenChain, CloseChain, …). Label by the action
        // variant for diagnostics; not user data.
        None => return (action_variant_label(&rec.action), serde_json::Value::Null),
    };

    let app_def = match entry_type {
        EntryType::App(def) => def,
        EntryType::AgentPubKey => return ("AgentPubKey".to_string(), serde_json::Value::Null),
        EntryType::CapClaim => return ("CapClaim".to_string(), serde_json::Value::Null),
        EntryType::CapGrant => return ("CapGrant".to_string(), serde_json::Value::Null),
    };

    let zome = app_def.zome_index.0;
    let entry_idx = app_def.entry_index.0;

    // Pull the raw msgpack entry bytes (if the entry is present in the dump).
    let entry_bytes: Option<&[u8]> = rec
        .entry
        .as_ref()
        .and_then(|e| e.as_app_entry())
        .map(|app_bytes| app_bytes.as_ref().bytes().as_slice());

    match (zome, entry_idx) {
        // polls_integrity (zome index 1)
        (1, 0) => decode_named::<Poll>("Poll", entry_bytes),
        (1, 1) => decode_named::<Vote>("Vote", entry_bytes),
        // Flag / MigratedPoll: kept in raw_record for graft, but no
        // human_readable view (commands.rs has no plain mirror struct for
        // them). Not counted in the user-facing summary.
        (1, 2) => ("Flag".to_string(), serde_json::Value::Null),
        (1, 3) => ("MigratedPoll".to_string(), serde_json::Value::Null),
        // EncryptedEntry holds ciphertext — no plaintext human_readable view.
        (1, 4) => ("EncryptedEntry".to_string(), serde_json::Value::Null),
        // agent_linking_integrity (zome index 0)
        (0, 0) => ("IsSamePerson".to_string(), serde_json::Value::Null),
        _ => (
            format!("AppEntry(zome={},entry={})", zome, entry_idx),
            serde_json::Value::Null,
        ),
    }
}

/// Decode entry bytes into a named type for the human_readable view. Returns
/// (name, json) on success, or (name, null) if bytes are absent or undecodable
/// — the record is still kept (with its raw_record) for graft.
fn decode_named<T: serde::de::DeserializeOwned + serde::Serialize>(
    name: &str,
    entry_bytes: Option<&[u8]>,
) -> (String, serde_json::Value) {
    let human = entry_bytes
        .and_then(|b| rmp_serde::from_slice::<T>(b).ok())
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(serde_json::Value::Null);
    (name.to_string(), human)
}

/// Best-effort label for a system (entry-less) action, used only for
/// diagnostics in the backup — never shown as user data.
fn action_variant_label(action: &holochain_integrity_types::Action) -> String {
    use holochain_integrity_types::Action;
    match action {
        Action::Dna(_) => "Dna",
        Action::AgentValidationPkg(_) => "AgentValidationPkg",
        Action::InitZomesComplete(_) => "InitZomesComplete",
        Action::CreateLink(_) => "CreateLink",
        Action::DeleteLink(_) => "DeleteLink",
        Action::OpenChain(_) => "OpenChain",
        Action::CloseChain(_) => "CloseChain",
        Action::Create(_) => "Create",
        Action::Update(_) => "Update",
        Action::Delete(_) => "Delete",
    }
    .to_string()
}

/// Read the three lair files and return them base64-encoded / stringified for
/// inclusion in the CAL §4.2.1 backup (the cryptographic keys the user needs to
/// operate their data independently). Returns `None` if any is missing — we
/// include all three or none.
fn read_lair_backup_fields(data_dir: &Path) -> Option<(String, String, String)> {
    use base64::Engine;
    let passphrase = std::fs::read_to_string(data_dir.join("lair-passphrase")).ok()?;
    let config_yaml =
        std::fs::read_to_string(data_dir.join("lair").join("lair-keystore-config.yaml")).ok()?;
    let store_bytes = std::fs::read(data_dir.join("lair").join("store_file")).ok()?;
    let store_b64 = base64::engine::general_purpose::STANDARD.encode(&store_bytes);
    Some((passphrase, config_yaml, store_b64))
}
