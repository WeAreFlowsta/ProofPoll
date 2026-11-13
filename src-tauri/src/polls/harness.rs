//! Test harness only: the shapes ProofPoll 0.4.x left on v1.3.

#![allow(unused_imports)]
use crate::commands::*;
use crate::polls::*;
use holochain_types::prelude::{ActionHash, AgentPubKey};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::Mutex;

/// Test harness only (feature `harness`): write the shapes ProofPoll 0.4.x
/// left on v1.3 - a poll, an encrypted draft or vote reason - so the
/// migration's tests start from real data. Nothing else writes to v1.3.
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_poll(
    state: &AppState,
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    poll_type: Option<String>,
) -> Result<String, String> {
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let input = CreatePollInput { title, description, options, closes_at, poll_type: Some(poll_type.unwrap_or_else(|| "Anonymous".to_string())) };
    let result = call_zome(client, POLLS_ZOME, "create_poll", crate::hc::encode(&input)?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

/// See `harness_v13_poll`: a vote on a v1.3 poll, cast there with no
/// identity gate (the carrier's test fixtures need one).
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_vote(state: &AppState, poll: String, option_index: u32, display_name: Option<String>) -> Result<String, String> {
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let poll_action_hash = ActionHash::try_from(poll).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let input = CastVoteInput { poll_action_hash, option_index, display_name, profile_picture: None };
    let result = call_zome(client, POLLS_ZOME, "cast_vote", crate::hc::encode(&input)?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

/// See `harness_v13_poll`: a sign-in as a 0.4.x install made it, the
/// identity link written on v1.3 and saved here, so the move's tests start
/// from an install whose only link is on an older version.
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_link(state: &AppState, vault_agent_pub_key: String, vault_signature: String) -> Result<String, String> {
    #[derive(serde::Serialize, Debug)]
    struct CreateExternalLinkInput {
        external_agent: AgentPubKey,
        external_signature: Vec<u8>,
    }
    use base64::Engine;
    let external_signature = base64::engine::general_purpose::STANDARD
        .decode(&vault_signature)
        .map_err(|e| format!("Invalid signature: {}", e))?;
    let input = CreateExternalLinkInput { external_agent: parse_agent_pub_key_string(&vault_agent_pub_key)?, external_signature };
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "create_external_link", crate::hc::encode(&input)?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    let linked_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    save_identity_link(
        &state.data_dir,
        &IdentityLinkData { vault_agent_pub_key: vault_agent_pub_key.clone(), entry_action_hash: action_hash.to_string(), linked_at },
    );
    crate::profiles::bind_profile(&state.device_root, &state.data_dir, &vault_agent_pub_key);
    Ok(action_hash.to_string())
}

/// See `harness_v13_poll`: a flag on a v1.3 poll, raised there with no
/// identity gate.
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_flag(state: &AppState, poll: String, reason: String) -> Result<String, String> {
    #[derive(serde::Serialize, Debug)]
    struct Input {
        poll_action_hash: ActionHash,
        reason: String,
    }
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let poll_action_hash = ActionHash::try_from(poll).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let result = call_zome(client, POLLS_ZOME, "flag_poll", crate::hc::encode(&Input { poll_action_hash, reason })?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

/// See `harness_v13_poll`: `f` ("delete_poll" or "remove_flag") on a v1.3
/// record, as its 0.4.x author does.
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_remove(state: &AppState, f: &str, hash: String) -> Result<String, String> {
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let hash = ActionHash::try_from(hash).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let result = call_zome(client, POLLS_ZOME, f, crate::hc::encode(&hash)?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

/// See `harness_v13_poll`: an entry encrypted to this agent, linked as
/// `link_as` ("draft_poll" or "vote_rationale"), on v1.3.
#[cfg(feature = "harness")]
pub(crate) async fn harness_v13_encrypted(
    state: &AppState,
    link_as: &str,
    related_hash: Option<String>,
    plaintext: Vec<u8>,
) -> Result<String, String> {
    let agent_bytes = get_agent_ed25519_bytes(state)?;
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
    let related_hash = match related_hash {
        Some(h) => Some(parse_action_hash(&h)?),
        None => None,
    };
    let input = Input { cipher, nonce: nonce.to_vec(), link_as: link_as.to_string(), related_hash };
    let client = state.app_client_v1_3.lock().await;
    let client = client.as_ref().ok_or("v1.3 is not running")?;
    let result = call_zome(client, POLLS_ZOME, "create_encrypted_entry", crate::hc::encode(&input)?).await?;
    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}
