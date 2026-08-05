//! Device agent seed - generated app-side, escrowed in backups.
//!
//! The agent seed is created HERE (not inside lair) so the user's canonical
//! backup can carry it: the export then holds the means of authorship, and a
//! restore imports the same seed - the same agent - on a new machine. Lair
//! still holds and uses the seed for signing; this module is its origin and
//! its recovery copy. Same custody model as the on-disk lair store itself.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const RECOVERY_FILE: &str = "proofpoll-recovery.json";
pub const SEED_TAG: &str = "proofpoll-device-seed";

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DeviceSeed {
    /// Hex-encoded 32-byte agent seed. Treat like a key: escrow it, export
    /// it to the user, never log it.
    pub device_seed_hex: String,
    /// Format version of this file itself.
    pub version: u32,
}

impl DeviceSeed {
    pub fn seed_bytes(&self) -> Result<[u8; 32], String> {
        let v = hex::decode(&self.device_seed_hex)
            .map_err(|e| format!("recovery file: bad seed hex: {}", e))?;
        v.try_into()
            .map_err(|_| "recovery file: seed is not 32 bytes".to_string())
    }
}

pub fn recovery_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RECOVERY_FILE)
}

/// Load the device seed, or None when this install predates the scheme
/// (or is brand new). Corrupt files are an ERROR, never silently regenerated
/// - regenerating would orphan the agent the corrupt file described.
pub fn load(data_dir: &Path) -> Result<Option<DeviceSeed>, String> {
    let p = recovery_path(data_dir);
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&p)
        .map_err(|e| format!("recovery file unreadable: {}", e))?;
    let seed: DeviceSeed =
        serde_json::from_str(&raw).map_err(|e| format!("recovery file corrupt: {}", e))?;
    seed.seed_bytes()?; // validate now, loudly
    Ok(Some(seed))
}

/// Generate a fresh random seed WITHOUT persisting it. The re-key flow
/// commits the seed only after the old key-derived state is verifiably
/// wiped - generation and persistence are deliberately separate steps.
pub fn generate() -> Result<DeviceSeed, String> {
    let mut seed = [0u8; 32];
    lair_keystore_api::dependencies::sodoken::random::randombytes_buf(&mut seed)
        .map_err(|e| format!("seed generation failed: {}", e))?;
    Ok(DeviceSeed { device_seed_hex: hex::encode(seed), version: 1 })
}

/// Generate a fresh random seed and persist it. Refuses to overwrite an
/// existing file - replacing a seed is an explicit adopt/re-key operation,
/// never a side effect.
pub fn generate_and_store(data_dir: &Path) -> Result<DeviceSeed, String> {
    let p = recovery_path(data_dir);
    if p.exists() {
        return Err("recovery file already exists".into());
    }
    let ds = generate()?;
    store(data_dir, &ds)?;
    Ok(ds)
}

/// Replace the recovery file with `seed`, preserving whatever is there as a
/// timestamped sibling first. A key must never be silently destroyed - old
/// records encrypted to the old agent are unrecoverable without it, so the
/// adopt/re-key paths keep the outgoing material on disk (same rule as Your
/// Own AI's `replace_recovery_material`).
pub fn replace(data_dir: &Path, seed: &DeviceSeed) -> Result<(), String> {
    let p = recovery_path(data_dir);
    if p.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = data_dir.join(format!("proofpoll-recovery.replaced-{}.json", ts));
        std::fs::rename(&p, &backup)
            .map_err(|e| format!("could not preserve the old recovery file: {}", e))?;
    }
    store(data_dir, seed)
}

/// The `app_keys` block both export emitters carry (canonical backup and the
/// data export): the means of authorship travels WITH the data (CAL).
/// Installs whose agent predates the app-held seed have nothing to escrow
/// yet and say so. A corrupt recovery file is an error - the caller decides
/// whether that blocks (backups do; see the escrow gate) - never silently
/// exported as "no seed".
pub fn app_keys_json(data_dir: &Path) -> Result<serde_json::Value, String> {
    match load(data_dir)? {
        Some(seed) => Ok(serde_json::json!({
            "_readme": "The agent seed this device signs with. Import it on a new machine to keep authoring as the same agent. Keep any export of this file private.",
            "device_seed_hex": seed.device_seed_hex,
            "version": seed.version,
        })),
        None => Ok(serde_json::json!({
            "_readme": "This install's agent key predates escrowable seeds, so no seed can be included. Re-key from the Identity page to make future exports carry your authorship.",
            "device_seed_hex": serde_json::Value::Null,
        })),
    }
}

/// Persist a seed (adopt/re-key path). Writes via a temp file + rename so a
/// crash can never leave a half-written recovery file.
pub fn store(data_dir: &Path, seed: &DeviceSeed) -> Result<(), String> {
    seed.seed_bytes()?; // never persist something unloadable
    let p = recovery_path(data_dir);
    let tmp = p.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(seed).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("recovery file write failed: {}", e))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("recovery file rename failed: {}", e))?;
    Ok(())
}

/// Derive the Holochain agent key this seed produces (holochain_types
/// computes the canonical DHT location bytes).
pub fn agent_pub_key(seed: &DeviceSeed) -> Result<holochain_types::prelude::AgentPubKey, String> {
    let seed_bytes = seed.seed_bytes()?;
    let mut pk = [0u8; 32];
    let mut sk = lair_keystore_api::dependencies::sodoken::SizedLockedArray::<64>::new()
        .map_err(|e| e.to_string())?;
    lair_keystore_api::dependencies::sodoken::sign::seed_keypair(
        &mut pk,
        &mut *sk.lock(),
        &seed_bytes,
    )
    .map_err(|e| format!("keypair derivation failed: {}", e))?;
    Ok(holochain_types::prelude::AgentPubKey::from_raw_32(pk.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_load_roundtrip_and_no_overwrite() {
        let dir = std::env::temp_dir().join(format!("pp-seed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = generate_and_store(&dir).unwrap();
        let b = load(&dir).unwrap().expect("seed present");
        // No Debug on key material - compare without assert_eq.
        assert!(a == b, "roundtrip changed the seed");
        assert!(generate_and_store(&dir).is_err(), "must refuse overwrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_errors_never_regenerates() {
        let dir = std::env::temp_dir().join(format!("pp-seed-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(recovery_path(&dir), "{not json").unwrap();
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_preserves_the_old_file_with_a_timestamp() {
        let dir = std::env::temp_dir().join(format!("pp-seed-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = generate_and_store(&dir).unwrap();
        let new = generate().unwrap();
        replace(&dir, &new).unwrap();
        let loaded = load(&dir).unwrap().expect("seed present");
        assert!(loaded == new, "replace must commit the incoming seed");
        let preserved = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("proofpoll-recovery.replaced-")
            })
            .count();
        assert_eq!(preserved, 1, "old seed must be preserved, never destroyed");
        let raw = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("proofpoll-recovery.replaced-")
            })
            .unwrap();
        let kept: DeviceSeed =
            serde_json::from_str(&std::fs::read_to_string(raw.path()).unwrap()).unwrap();
        assert!(kept == old, "preserved file must hold the outgoing seed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The v6 payload-shape contract: what both export emitters embed.
    #[test]
    fn app_keys_block_carries_the_seed_when_present() {
        let dir = std::env::temp_dir().join(format!("pp-seed-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seed = generate_and_store(&dir).unwrap();
        let keys = app_keys_json(&dir).unwrap();
        assert_eq!(
            keys["device_seed_hex"].as_str(),
            Some(seed.device_seed_hex.as_str())
        );
        assert_eq!(keys["version"].as_u64(), Some(1));
        assert!(keys["_readme"].as_str().unwrap().contains("same agent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_keys_block_is_an_honest_null_for_legacy_installs() {
        let dir = std::env::temp_dir().join(format!("pp-seed-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = app_keys_json(&dir).unwrap();
        assert!(keys["device_seed_hex"].is_null());
        assert!(keys["_readme"].as_str().unwrap().contains("Re-key"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_keys_block_errors_on_a_corrupt_file_never_null() {
        let dir = std::env::temp_dir().join(format!("pp-seed-keyscorrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(recovery_path(&dir), "{not json").unwrap();
        assert!(app_keys_json(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_key_is_stable_and_shaped() {
        let seed = DeviceSeed { device_seed_hex: "11".repeat(32), version: 1 };
        let k1 = agent_pub_key(&seed).unwrap();
        let k2 = agent_pub_key(&seed).unwrap();
        assert_eq!(k1, k2);
        assert_eq!(&k1.get_raw_39()[0..3], &[0x84, 0x20, 0x24]);
    }
}

/// Load-or-create the device seed, import it into lair, and return the
/// agent key - the fresh-install path. The key is built from LAIR's derived
/// public key and cross-checked against the app-side derivation, so the
/// conductor can always sign for the agent we install with.
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
