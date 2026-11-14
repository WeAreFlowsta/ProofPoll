//! Device agent seed - generated app-side, escrowed in backups.
//!
//! The seed file itself (its format, the load that never regenerates over a
//! corrupt file, the replace that keeps the outgoing seed beside it) is
//! `flowsta_vault_client::device_seed`; here are this app's file name and
//! key store tag, and the import into each line's key store.

use flowsta_vault_client::device_seed as sdk;
use std::path::Path;

pub use flowsta_vault_client::device_seed::DeviceSeed;

pub const RECOVERY_FILE: &str = crate::names::RECOVERY_FILE;
pub const SEED_TAG: &str = crate::names::SEED_TAG;

/// Load the device seed, or None when this install predates the scheme
/// (or is brand new). A corrupt file is an ERROR, never regenerated.
pub fn load(data_dir: &Path) -> Result<Option<DeviceSeed>, String> {
    sdk::load(data_dir, RECOVERY_FILE)
}

/// A fresh random seed, NOT persisted (the re-key commits it only after
/// the old key-derived state is verifiably wiped).
pub fn generate() -> Result<DeviceSeed, String> {
    sdk::generate()
}

/// A fresh random seed, persisted. Refuses to overwrite an existing file.
pub fn generate_and_store(data_dir: &Path) -> Result<DeviceSeed, String> {
    sdk::generate_and_store(data_dir, RECOVERY_FILE)
}

/// Replace the recovery file with `seed`, keeping whatever is there as a
/// timestamped sibling first.
pub fn replace(data_dir: &Path, seed: &DeviceSeed) -> Result<(), String> {
    sdk::replace(data_dir, RECOVERY_FILE, seed)
}

/// The `app_keys` block both export emitters carry (canonical backup and
/// the data export): the means of authorship travels WITH the data.
pub fn app_keys_json(data_dir: &Path) -> Result<serde_json::Value, String> {
    sdk::app_keys_json(
        data_dir,
        RECOVERY_FILE,
        "This install's agent key predates escrowable seeds, so no seed can be included. Re-key from the Identity page to make future exports carry your authorship.",
    )
}

/// Derive the Holochain agent key this seed produces (holochain_types
/// computes the canonical DHT location bytes).
pub fn agent_pub_key(seed: &DeviceSeed) -> Result<holochain_types::prelude::AgentPubKey, String> {
    Ok(holochain_types::prelude::AgentPubKey::from_raw_32(sdk::public_key(seed)?.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key the SDK derives is the one both key stores derive (they are
    /// cross-checked at every start); its Holochain form has the agent prefix.
    #[test]
    fn agent_key_is_stable_and_shaped() {
        let seed = DeviceSeed { device_seed_hex: "11".repeat(32), version: 1 };
        let k1 = agent_pub_key(&seed).unwrap();
        let k2 = agent_pub_key(&seed).unwrap();
        assert_eq!(k1, k2);
        assert_eq!(&k1.get_raw_39()[0..3], &[0x84, 0x20, 0x24]);
    }

    #[test]
    fn the_export_block_keeps_this_app_s_words_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let keys = app_keys_json(dir.path()).unwrap();
        assert!(keys["device_seed_hex"].is_null());
        assert!(keys["_readme"].as_str().unwrap().contains("Re-key from the Identity page"));
        let seed = generate_and_store(dir.path()).unwrap();
        assert!(dir.path().join("proofpoll-recovery.json").exists());
        assert_eq!(app_keys_json(dir.path()).unwrap()["device_seed_hex"].as_str(), Some(seed.device_seed_hex.as_str()));
        replace(dir.path(), &generate().unwrap()).unwrap();
        let kept = std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("proofpoll-recovery.replaced-")).count();
        assert_eq!(kept, 1, "the outgoing seed is kept under the name the profile move looks for");
    }
}

/// Load-or-create the device seed, import it into lair, and return the
/// agent key - the fresh-install path. The key is built from LAIR's derived
/// public key and cross-checked against the app-side derivation, so the
/// conductor can always sign for the agent we install with.
/// The same device agent on the 0.7 lair: the app-held seed imported there
/// too, so both conductors sign as one agent. An install whose agent was
/// conductor-born (before the app-held seed) has no seed file; it gets a new
/// one here, so its 0.7 line is a new agent (its carried records name the
/// old author).
pub async fn ensure_device_agent_07(
    client: &lair_keystore_api_07::prelude::LairClient,
    data_dir: &Path,
) -> Result<holochain_types::prelude::AgentPubKey, String> {
    let seed = match load(data_dir)? {
        Some(s) => s,
        None => {
            log::info!("[seed] generating device agent seed for the 0.7 line (no app-held seed yet)");
            generate_and_store(data_dir)?
        }
    };
    let lair_pk = crate::lair::import_seed_to_lair_07(client, &seed.seed_bytes()?, SEED_TAG).await?;
    let lair_key = holochain_types::prelude::AgentPubKey::from_raw_32(lair_pk.to_vec());
    let derived = agent_pub_key(&seed)?;
    if derived != lair_key {
        return Err("lair (0.7)-derived agent key differs from the recovery file's derivation".into());
    }
    Ok(lair_key)
}

pub async fn ensure_device_agent(
    client: &lair_keystore_api::prelude::LairClient,
    data_dir: &Path,
) -> Result<holochain_types::prelude::AgentPubKey, String> {
    let seed = match load(data_dir)? {
        Some(s) => s,
        None => {
            log::info!("[seed] generating device agent seed (app-held, escrowed in backups)");
            generate_and_store(data_dir)?
        }
    };
    let info = crate::lair::import_seed_to_lair(client, &seed.seed_bytes()?, SEED_TAG)
        .await
        .map_err(|e| format!("seed import into lair failed: {}", e))?;
    let lair_pk: [u8; 32] = *info.ed25519_pub_key.0;
    let lair_key = holochain_types::prelude::AgentPubKey::from_raw_32(lair_pk.to_vec());
    let derived = agent_pub_key(&seed)?;
    if derived != lair_key {
        return Err("lair-derived agent key differs from the recovery file's derivation".into());
    }
    Ok(lair_key)
}
