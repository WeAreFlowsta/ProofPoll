//! Tauri commands for ProofPoll.
//!
//! All zome calls go through the Rust backend via AppWebsocket.
//! The frontend uses lightweight Tauri invoke() calls — no @holochain/client needed.

use crate::conductor::{ConductorHandle, ConductorStatus};
use holochain_client::AppWebsocket;
use holochain_types::prelude::{
    ActionHash, AgentPubKey, ExternIO, FunctionName, Record, ZomeName,
};
use std::path::PathBuf;
use std::sync::Mutex;

// --- Entry types matching the zome definitions ---

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
}

// --- Frontend response types (all hashes as strings) ---

#[derive(serde::Serialize, Clone)]
pub struct PollListItem {
    pub hash: String,
    pub poll: Poll,
    pub author: String,
}

#[derive(serde::Serialize)]
pub struct PollDetail {
    pub poll: Poll,
    pub author: String,
}

#[derive(serde::Serialize, Clone)]
pub struct VoteData {
    pub vote: VoteResponse,
    pub author: String,
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
    pub app_client: tokio::sync::Mutex<Option<AppWebsocket>>,
    pub passphrase: Mutex<String>,
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

        Self {
            data_dir,
            conductor_handle: Mutex::new(None),
            conductor_status: Mutex::new(ConductorStatus::Stopped),
            agent_pub_key: Mutex::new(None),
            app_client: tokio::sync::Mutex::new(None),
            passphrase: Mutex::new(passphrase),
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

// --- Helpers ---

const ROLE_NAME: &str = "proofpoll";
const POLLS_ZOME: &str = "polls";
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

async fn call_zome(
    client: &AppWebsocket,
    zome: &str,
    fn_name: &str,
    payload: ExternIO,
) -> Result<ExternIO, String> {
    use holochain_client::ZomeCallTarget;
    use holochain_types::prelude::RoleName;

    client
        .call_zome(
            ZomeCallTarget::RoleName(RoleName::from(ROLE_NAME)),
            ZomeName::from(zome),
            FunctionName::from(fn_name),
            payload,
        )
        .await
        .map_err(|e| format!("Zome call failed: {}", e))
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

// --- Status command ---

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

// --- Poll commands ---

#[tauri::command]
pub async fn create_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
) -> Result<String, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let input = CreatePollInput {
        title,
        description,
        options,
        closes_at,
    };
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "create_poll", payload).await?;

    let record: Record = result.decode().map_err(|e| e.to_string())?;
    Ok(record.action_address().to_string())
}

#[tauri::command]
pub async fn get_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    action_hash: String,
) -> Result<Option<PollDetail>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash =
        ActionHash::try_from(action_hash).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;

    let record: Option<Record> = result.decode().map_err(|e| e.to_string())?;
    match record {
        None => Ok(None),
        Some(record) => {
            let poll: Poll = decode_entry(&record)?;
            let author = record.action().author().to_string();
            Ok(Some(PollDetail { poll, author }))
        }
    }
}

#[tauri::command]
pub async fn get_all_polls(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<PollListItem>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let payload = ExternIO::encode(()).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;

    let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;
    let mut polls = Vec::new();
    for record in &records {
        let poll: Poll = decode_entry(record)?;
        polls.push(PollListItem {
            hash: record.action_address().to_string(),
            poll,
            author: record.action().author().to_string(),
        });
    }
    Ok(polls)
}

#[tauri::command]
pub async fn cast_vote(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    option_index: u32,
) -> Result<String, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let input = CastVoteInput {
        poll_action_hash: hash,
        option_index,
    };
    let payload = ExternIO::encode(input).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;

    let record: Record = result.decode().map_err(|e| e.to_string())?;
    Ok(record.action_address().to_string())
}

#[tauri::command]
pub async fn get_poll_votes(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
) -> Result<Vec<VoteData>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = ExternIO::encode(hash).map_err(|e| e.to_string())?;
    let result = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await?;

    let records: Vec<Record> = result.decode().map_err(|e| e.to_string())?;
    let mut votes = Vec::new();
    for record in &records {
        let vote: Vote = decode_entry(record)?;
        votes.push(VoteData {
            vote: VoteResponse {
                poll_action_hash: vote.poll_action_hash.to_string(),
                option_index: vote.option_index,
            },
            author: record.action().author().to_string(),
        });
    }
    Ok(votes)
}

// --- Identity linking commands ---

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
    Ok(action_hash.to_string())
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
