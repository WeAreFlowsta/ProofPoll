//! Disconnect: the sign-in ends on this device first, then on the networks.
//!
//! Until 0.6.0 Disconnect revoked the identity link by the hash saved when
//! the link was made, and stopped at the first error, before anything on
//! the device was cleared. A link made on v1.3 or v1.4 reaches v1.5 as a
//! carried copy with another hash, so on an install that signed in before
//! 0.6.0 the revoke answered "Entry not found" and the person stayed signed
//! in with an error on the page.
//!
//! Now:
//!   1. the link and the cached profile are removed from this device at
//!      once (that is what ends the sign-in here), and what is still to be
//!      removed on the networks is written down;
//!   2. the link is removed wherever this install can reach it: the
//!      original on the network it was made on (v1.5, v1.4 or v1.3, by its
//!      saved hash), and every carried copy on v1.5 between this install's
//!      key and the identity (`remove_carried_link`). The original goes
//!      first: a carried copy removed while its original still stands would
//!      be carried again;
//!   3. a step a network refuses is logged and tried again at the next
//!      starts, until none is left.

use crate::commands::{call_zome, parse_agent_pub_key_string, AppState, AGENT_LINKING_ZOME, POLLS_ZOME};
use holochain_types::prelude::ActionHash;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const OWED_FILE: &str = "disconnect-owed.json";

/// A link this device no longer uses, still to be removed on the networks.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Owed {
    /// The identity the install was signed in as.
    pub vault_agent_pub_key: String,
    /// The link's hash on the network it was made on.
    pub entry_action_hash: String,
}

fn owed_path(data_dir: &Path) -> PathBuf {
    data_dir.join(OWED_FILE)
}

pub(crate) fn load_owed(data_dir: &Path) -> Vec<Owed> {
    std::fs::read(owed_path(data_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_owed(data_dir: &Path, owed: &[Owed]) {
    if owed.is_empty() {
        let _ = std::fs::remove_file(owed_path(data_dir));
    } else if let Ok(json) = serde_json::to_vec_pretty(owed) {
        let _ = std::fs::write(owed_path(data_dir), json);
    }
}

/// Note a link to remove (once, whatever the number of presses).
pub(crate) fn owe(data_dir: &Path, link: Owed) {
    let mut owed = load_owed(data_dir);
    if !owed.contains(&link) {
        owed.push(link);
        write_owed(data_dir, &owed);
    }
}

/// A network's answer that the record is not there: nothing to remove on
/// that network, which is not a failure (the link was made on another one,
/// or it is gone already).
pub(crate) fn not_there(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("entry not found") || e.contains("record not found") || e.contains("link not found")
}

/// Remove one link wherever this install can reach it. Returns what could
/// not be done, by network; empty when nothing is left.
async fn remove_on_networks(state: &AppState, owed: &Owed) -> Vec<String> {
    let mut left = Vec::new();
    let hash = match ActionHash::try_from(owed.entry_action_hash.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            log::warn!("[disconnect] the saved link hash cannot be read ({:?}); only carried copies are removed", e);
            None
        }
    };
    // The original, on whichever network it was made: the older ones first.
    if let Some(hash) = &hash {
        let payload = match crate::hc::encode(hash) {
            Ok(p) => p,
            Err(e) => return vec![format!("the link's hash could not be encoded: {e}")],
        };
        for (name, client) in [("v1.3", &state.app_client_v1_3), ("v1.4", &state.app_client_v1_4), ("v1.5", &state.app_client)] {
            let client = client.lock().await;
            // A network this install does not run holds no link of its own.
            let Some(client) = client.as_ref() else { continue };
            match call_zome(client, AGENT_LINKING_ZOME, "revoke_link", payload.clone()).await {
                Ok(_) => log::info!("[disconnect] the link is revoked on {name}"),
                Err(e) if not_there(&e) => {}
                Err(e) => left.push(format!("{name}: {e}")),
            }
        }
    }
    // Every carried copy on v1.5 between this install's key and the identity.
    match parse_agent_pub_key_string(&owed.vault_agent_pub_key) {
        Ok(identity) => {
            let client = state.app_client.lock().await;
            match client.as_ref() {
                None => left.push("v1.5: not running".to_string()),
                Some(client) => match crate::hc::encode(&identity) {
                    Err(e) => left.push(format!("v1.5 carried copies: {e}")),
                    Ok(payload) => match call_zome(client, POLLS_ZOME, "remove_carried_link", payload).await {
                        Ok(bytes) => {
                            let removed: u32 = crate::hc::decode(&bytes).unwrap_or(0);
                            if removed > 0 {
                                log::info!("[disconnect] {removed} carried link(s) removed on v1.5");
                            }
                        }
                        Err(e) => left.push(format!("v1.5 carried copies: {e}")),
                    },
                },
            }
        }
        Err(e) => log::warn!("[disconnect] the identity's key cannot be read ({e}); no carried copy is looked for"),
    }
    left
}

/// Try every owed removal; keep what is still left for the next start.
pub(crate) async fn settle(state: &AppState) {
    let loaded = load_owed(&state.data_dir);
    if loaded.is_empty() {
        return;
    }
    let mut still = Vec::new();
    for link in &loaded {
        let left = remove_on_networks(state, link).await;
        if left.is_empty() {
            log::info!("[disconnect] the link to the identity is removed on every network this install runs");
        } else {
            log::warn!("[disconnect] not removed yet, tried again at the next start: {}", left.join("; "));
            still.push(link.clone());
        }
    }
    // A Disconnect pressed while this ran added its own line: keep it.
    still.extend(load_owed(&state.data_dir).into_iter().filter(|l| !loaded.contains(l)));
    write_owed(&state.data_dir, &still);
}

/// At a start: finish any Disconnect a network refused last time, once the
/// conductors answer.
pub(crate) fn spawn(state: Arc<AppState>) {
    if load_owed(&state.data_dir).is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        for _ in 0..60 {
            if state.app_client.lock().await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        settle(&state).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_that_does_not_hold_the_link_is_not_a_failure() {
        // The 0.5.0 report, word for word.
        assert!(not_there("Something went wrong: External API wire error: InternalError(\"Wasm runtime error while working with Ribosome: RuntimeError: flowsta_agent_linking_coordinator:202: Guest(\\\"Entry not found\\\")\")"));
        assert!(not_there("Record not found"));
        assert!(!not_there("Conductor not ready"));
        assert!(!not_there("timed out after 60 s"));
        assert!(!not_there("Only the link's own agents can revoke it"));
    }

    #[test]
    fn a_disconnect_is_owed_once_and_the_file_goes_when_nothing_is_owed() {
        let dir = tempfile::tempdir().unwrap();
        let link = Owed { vault_agent_pub_key: "uhCAk-identity".into(), entry_action_hash: "uhCkk-link".into() };
        assert!(load_owed(dir.path()).is_empty());
        owe(dir.path(), link.clone());
        owe(dir.path(), link.clone());
        assert_eq!(load_owed(dir.path()), vec![link]);
        write_owed(dir.path(), &[]);
        assert!(!owed_path(dir.path()).exists());
    }
}
