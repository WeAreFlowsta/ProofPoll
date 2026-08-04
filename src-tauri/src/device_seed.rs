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

/// Generate a fresh random seed and persist it. Refuses to overwrite an
/// existing file - replacing a seed is an explicit adopt/re-key operation,
/// never a side effect.
pub fn generate_and_store(data_dir: &Path) -> Result<DeviceSeed, String> {
    let p = recovery_path(data_dir);
    if p.exists() {
        return Err("recovery file already exists".into());
    }
    let mut seed = [0u8; 32];
    lair_keystore_api::dependencies::sodoken::random::randombytes_buf(&mut seed)
        .map_err(|e| format!("seed generation failed: {}", e))?;
    let ds = DeviceSeed { device_seed_hex: hex::encode(seed), version: 1 };
    store(data_dir, &ds)?;
    Ok(ds)
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
    fn agent_key_is_stable_and_shaped() {
        let seed = DeviceSeed { device_seed_hex: "11".repeat(32), version: 1 };
        let k1 = agent_pub_key(&seed).unwrap();
        let k2 = agent_pub_key(&seed).unwrap();
        assert_eq!(k1, k2);
        assert_eq!(&k1.get_raw_39()[0..3], &[0x84, 0x20, 0x24]);
    }
}
