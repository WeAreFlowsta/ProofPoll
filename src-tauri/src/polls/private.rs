//! Private (encrypted to self) entries: drafts and vote reasons.

#![allow(unused_imports)]
use crate::commands::*;
use crate::polls::*;
use holochain_types::prelude::{ActionHash, AgentPubKey};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::Mutex;

// ── Encrypted entry commands (v1.3) ──────────────────────────────────

/// Extract the raw 32-byte Ed25519 public key from the agent key string.
/// AgentPubKey is 39 bytes: 3-byte prefix + 32-byte key + 4-byte DHT location.
pub(crate) fn get_agent_ed25519_bytes(state: &AppState) -> Result<[u8; 32], String> {
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
pub(crate) struct EncryptedEntryData {
    pub(crate) cipher: Vec<u8>,
    pub(crate) nonce: Vec<u8>,
    pub(crate) entry_type_hint: String,
    pub(crate) related_hash: Option<ActionHash>,
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
    // Encrypted, but its LINK to the vote is public - "this agent
    // rationalized this vote" is identity-bearing. Same gate.
    require_identity_link(&state).await?;
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
    let payload = crate::hc::encode(&input)?;
    let result = call_zome(client, POLLS_ZOME, "create_encrypted_entry", payload).await?;
    let hash: ActionHash = crate::hc::decode(&result)?;
    Ok(hash.to_string())
}

/// Get and decrypt a vote rationale.
#[tauri::command]
pub async fn get_vote_rationale(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    vote_action_hash: String,
) -> Result<Option<String>, String> {
    let vote_hash = parse_action_hash(&vote_action_hash)?;
    let payload = crate::hc::encode(&vote_hash)?;

    let record = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let result = call_zome(client, POLLS_ZOME, "get_vote_rationale", payload.clone()).await?;
        crate::hc::record_opt(client.line(), &result)?
    };
    // A vote still on an older network (not carried yet) keeps its reason
    // there.
    let mut record = record;
    for slot in [&state.app_client_v1_4, &state.app_client_v1_3] {
        if record.is_some() {
            break;
        }
        let client = slot.lock().await;
        if let Some(c) = client.as_ref() {
            if let Ok(result) = call_zome(c, POLLS_ZOME, "get_vote_rationale", payload.clone()).await {
                record = crate::hc::record_opt(c.line(), &result).unwrap_or(None);
            }
        }
    }

    let record = match record {
        Some(r) => r,
        None => return Ok(None),
    };

    // Only decrypt if this install wrote it (under either of its keys: the
    // 0.7 line's differs when the 0.6 one was conductor-born; the ciphertext
    // is to the 0.6 key either way).
    let mine = [state.agent_pub_key.lock().unwrap().clone(), state.agent_pub_key_07.lock().unwrap().clone()];
    if !mine.iter().any(|k| k.as_deref() == Some(record.author.as_str())) {
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
    // Encrypted-to-self, but still authored + timestamped on the shared
    // DHT under this agent key - same gate as every other write.
    require_identity_link(&state).await?;
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
    let payload = crate::hc::encode(&input)?;
    let result = call_zome(client, POLLS_ZOME, "create_encrypted_entry", payload).await?;
    let hash: ActionHash = crate::hc::decode(&result)?;
    Ok(hash.to_string())
}

/// Get all draft polls (decrypted).
#[tauri::command]
pub async fn get_my_drafts(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<DraftPollItem>, String> {
    log::info!("get_my_drafts: fetching from DHT...");
    let records: Vec<crate::hc::RecordView> = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = crate::hc::encode(&())?;
        let result = call_zome(client, POLLS_ZOME, "get_my_drafts", payload).await?;
        crate::hc::records(client.line(), &result)?
    }; // app_client lock released here

    log::info!("get_my_drafts: {} records found, decrypting...", records.len());
    let agent_bytes = get_agent_ed25519_bytes(&state)?;
    let lair = state.lair_client.lock().await;
    let lair = lair.as_ref().ok_or("Lair not connected")?;

    let mut drafts = Vec::new();
    for record in &records {
        let hash = record.action_hash.clone();
        let created_at = record.timestamp_us / 1_000_000;

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
    // Publishing reaches create_poll on the shared DHT - same gate as the
    // direct path (this command shipped ungated once; never again).
    require_identity_link(&state).await?;
    let draft_hash = parse_action_hash(&draft_action_hash)?;
    let agent_bytes = get_agent_ed25519_bytes(&state)?;

    // Fetch and decrypt the draft
    let (draft_json, _ee) = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = crate::hc::encode(&draft_hash.clone())?;
        let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
        let record = crate::hc::record_opt(client.line(), &result)?;
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
    let payload = crate::hc::encode(&input)?;
    let result = call_zome(client, POLLS_ZOME, "create_poll", payload).await?;
    let poll_hash: ActionHash = crate::hc::decode(&result)?;

    // Delete the draft
    let delete_payload = crate::hc::encode(&draft_hash)?;
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
    require_identity_link(&state).await?;
    let hash = parse_action_hash(&draft_action_hash)?;
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    let payload = crate::hc::encode(&hash)?;
    let result = call_zome(client, POLLS_ZOME, "delete_encrypted_entry", payload).await?;
    let deleted: ActionHash = crate::hc::decode(&result)?;
    Ok(deleted.to_string())
}

/// Parse an action hash string into an ActionHash.
pub(crate) fn parse_action_hash(s: &str) -> Result<ActionHash, String> {
    ActionHash::try_from(s.to_string()).map_err(|e| format!("Invalid action hash: {:?}", e))
}
