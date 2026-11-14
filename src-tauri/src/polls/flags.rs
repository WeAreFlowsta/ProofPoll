//! Community flagging of polls.

#![allow(unused_imports)]
use crate::commands::*;
use crate::polls::*;
use holochain_types::prelude::{ActionHash, AgentPubKey};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::Mutex;

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
pub(crate) struct FlagEntry {
    pub(crate) poll_action_hash: ActionHash,
    pub(crate) reason: FlagReason,
    pub(crate) created_at: i64,
    /// v1.5: where a carried flag came from.
    #[serde(default)]
    pub(crate) carried_from: Option<Origin>,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) enum FlagReason {
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
pub(crate) struct FlagPollInput {
    pub(crate) poll_action_hash: ActionHash,
    pub(crate) reason: String,
}

#[tauri::command]
pub async fn flag_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    reason: String,
    dna_version: Option<String>,
) -> Result<String, String> {
    // Tier-1 gate: signed-in install, no unlock required - but never while
    // a different identity's Vault is open. See require_identity_link.
    require_identity_link(&state).await?;

    // On the poll's own network: its coordinator refuses a poll it cannot
    // find there ("Poll not found" for every v1.3 poll before).
    let client = client_for(&state, dna_version.as_deref().unwrap_or_default()).lock().await;
    let client = client.as_ref().ok_or("This poll's network is not available")?;

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

    let payload = crate::hc::encode(&zome_input)?;
    let result = call_zome(client, POLLS_ZOME, "flag_poll", payload).await?;
    if matches!(dna_version.as_deref(), Some("1.3" | "1.4")) {
        // The move carries it once the poll can come.
        crate::carry_move::reopen(&state.data_dir);
    }

    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

#[tauri::command]
pub async fn get_poll_flags(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    // The poll's dna_version: its flags live on its own network. None =
    // the active one.
    dna_version: Option<String>,
) -> Result<Vec<FlagData>, String> {
    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let version = dna_version.unwrap_or_default();
    let active = !matches!(version.as_str(), "1.0" | "1.1" | "1.2" | "1.3" | "1.4");
    let mut records = flag_records(client_for(&state, &version), &hash, active).await?;
    // A poll on the active network: add the flags still on an older line
    // under one of its older hashes, by anyone with no flag here.
    if active {
        let olds = olds_of(&state, &hash.to_string()).await;
        if !olds.is_empty() {
            let mut here: std::collections::HashSet<String> = records
                .iter()
                .map(|r| counted_author(r, &decode_entry::<FlagEntry>(r).ok().and_then(|f| f.carried_from)))
                .collect();
            for rec in on_older_lines(&state, &olds, "get_poll_flags").await {
                if here.insert(rec.author.clone()) {
                    records.push(rec);
                }
            }
        }
    }
    // Once per person (the hide threshold counts people).
    let records = once_per_person(state.inner(), records, |r| counted_author(r, &decode_entry::<FlagEntry>(r).ok().and_then(|f| f.carried_from))).await;
    let mut flags = Vec::new();
    for record in &records {
        let flag: FlagEntry = decode_entry(record)?;
        flags.push(FlagData {
            hash: record.action_hash.clone(),
            flag: FlagResponse {
                poll_action_hash: flag.poll_action_hash.to_string(),
                reason: format!("{}", flag.reason),
                created_at: flag.created_at,
            },
            author: counted_author(record, &flag.carried_from),
        });
    }
    Ok(flags)
}

#[tauri::command]
pub async fn remove_flag(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    flag_action_hash: String,
    // The flagged poll's dna_version (the flag is on that network).
    dna_version: Option<String>,
) -> Result<String, String> {
    require_identity_link(&state).await?;
    let client = client_for(&state, dna_version.as_deref().unwrap_or_default()).lock().await;
    let client = client.as_ref().ok_or("This poll's network is not available")?;

    let hash = ActionHash::try_from(flag_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = crate::hc::encode(&hash)?;
    let result = call_zome(client, POLLS_ZOME, "remove_flag", payload).await?;

    let delete_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(delete_hash.to_string())
}

#[tauri::command]
pub async fn get_flag_threshold(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u32, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let payload = crate::hc::encode(&())?;
    let result = call_zome(client, POLLS_ZOME, "get_flag_threshold", payload).await?;

    let threshold: u32 = crate::hc::decode(&result)?;
    Ok(threshold)
}
