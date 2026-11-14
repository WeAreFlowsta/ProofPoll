//! Identity profiles: one ProofPoll agent, key store, conductor and set of
//! link files per Flowsta identity that has connected to this install.
//!
//!   <app data dir>/profiles.json                 device-level index
//!   <app data dir>/profiles/<folder>/             one profile:
//!       lair-passphrase, lair/, conductor/, lair-07/, conductor-07/,
//!       proofpoll-recovery.json (+ .replaced-*),
//!       identity-link.json, profile-cache.json, migration-*-state.json,
//!       adopt-relink-pending
//!
//! The index, the folder naming, the move of an install from before
//! profiles and the setting aside of an older copy's data are
//! `flowsta_vault_client::profiles`; here is what ProofPoll keeps in a
//! profile. The key store moves WITH its files and its config's absolute
//! paths are rewritten - it is never set aside, because `AppState::new`
//! treats a passphrase without a store as corruption and wipes the conductor.
use flowsta_vault_client::profiles as sdk;
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub use flowsta_vault_client::profiles::{bind_profile, lair_socket_path_fits, partition_key, profile_root, Profiles, LOCAL_PROFILE};
#[cfg(not(windows))]
#[allow(unused_imports)]
pub use flowsta_vault_client::profiles::{in_root_socket_fits, short_socket_path};

fn is_migration_state(name: &str) -> bool {
    name.starts_with("migration-") && name.ends_with("-state.json")
}

/// Lair's config pins absolute paths (connectionUrl, pidFile, storeFile).
/// After the directory moved, point them at the new location. The socket
/// address is a percent-encoded URL, so this goes through the URL-aware
/// repoint, never a plain text replace.
fn repoint(new_lair_dir: &Path) -> Result<(), String> {
    crate::lair::repoint_config(new_lair_dir).map(|_| ())
}

/// What a ProofPoll profile holds.
const LAYOUT: sdk::Layout<'static> = sdk::Layout {
    recovery_file: crate::device_seed::RECOVERY_FILE,
    files: &["lair-passphrase"],
    also: &is_migration_state,
    dirs: crate::conductor::LINE_DIRS,
    key_stores: &["lair", crate::conductor::LAIR_DIR_07],
    conductor: "conductor",
    repoint_key_store: &repoint,
};

/// Decide which profile this launch runs, moving a legacy layout first if
/// there is one. `live_identity` = the Vault's unlocked agent key when the
/// Vault is reachable and unlocked at launch, else None.
pub fn select_profile_root(device_root: &Path, live_identity: Option<&str>) -> PathBuf {
    sdk::select_profile_root(device_root, live_identity, &LAYOUT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowsta_vault_client::profiles::legacy_layout_present as legacy_present;

    fn legacy_layout_present(root: &Path) -> bool {
        legacy_present(root, &LAYOUT)
    }

    const KEY_A: &str = "uhCAk75qJ5oobyfm3Lh-akZIQSe2zpSTtG1Pcxs23qTFoQwY_GDWY";
    const KEY_B: &str = "uhCAk0O4EJ97RZ7eX2wf9x08PWjNj3Avt2K1SdU8tgPzWoQBwWk0s";

    fn legacy_install(root: &Path, linked_to: Option<&str>) {
        std::fs::write(root.join("lair-passphrase"), b"pp").unwrap();
        std::fs::create_dir_all(root.join("lair")).unwrap();
        let lair = root.join("lair");
        std::fs::write(lair.join("lair-keystore-config.yaml"), format!("connectionUrl: unix://{}/socket?k=abc\npidFile: {}/pid_file\nstoreFile: {}/store_file\n", lair.display(), lair.display(), lair.display())).unwrap();
        std::fs::write(lair.join("store_file"), b"s").unwrap();
        std::fs::create_dir_all(root.join("conductor/databases")).unwrap();
        std::fs::write(root.join("conductor/databases/x"), b"d").unwrap();
        std::fs::write(root.join(crate::device_seed::RECOVERY_FILE), b"{}").unwrap();
        std::fs::write(root.join("migration-proofpoll_v1_3-state.json"), b"{}").unwrap();
        if let Some(k) = linked_to {
            std::fs::write(root.join("identity-link.json"), serde_json::json!({"vault_agent_pub_key": k, "entry_action_hash": "h", "linked_at": 1}).to_string()).unwrap();
        }
    }

    #[test]
    fn the_07_line_moves_with_the_legacy_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        legacy_install(root, Some(KEY_A));
        let lair07 = root.join("lair-07");
        std::fs::create_dir_all(&lair07).unwrap();
        std::fs::write(lair07.join("lair-keystore-config.yaml"), format!("connectionUrl: unix://{}/socket?k=abc\npidFile: {}/pid_file\nstoreFile: {}/store_file\n", lair07.display(), lair07.display(), lair07.display())).unwrap();
        std::fs::create_dir_all(root.join("conductor-07/databases")).unwrap();
        std::fs::write(root.join("conductor-07/databases/x"), b"d").unwrap();
        let chosen = select_profile_root(root, None);
        assert!(chosen.join("conductor-07/databases/x").exists(), "0.7 conductor data in the profile");
        assert!(!root.join("lair-07").exists() && !root.join("conductor-07").exists(), "nothing of the 0.7 line left at the root");
        let yaml = std::fs::read_to_string(chosen.join("lair-07/lair-keystore-config.yaml")).unwrap();
        assert!(yaml.contains(&format!("storeFile: {}", chosen.join("lair-07").join("store_file").display())), "0.7 key store paths rewritten: {}", yaml);
    }

    #[test]
    fn linked_legacy_install_moves_into_its_identity_profile_and_rewrites_lair_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        legacy_install(root, Some(KEY_A));
        let chosen = select_profile_root(root, None);
        let pk = partition_key(KEY_A).unwrap();
        assert_eq!(chosen, profile_root(root, &pk));
        for rel in ["lair-passphrase", "lair/store_file", "lair/lair-keystore-config.yaml", "conductor/databases/x", crate::device_seed::RECOVERY_FILE, "identity-link.json", "migration-proofpoll_v1_3-state.json"] {
            assert!(chosen.join(rel).exists(), "{} in profile", rel);
            assert!(!root.join(rel).exists(), "{} left the root", rel);
        }
        let yaml = std::fs::read_to_string(chosen.join("lair/lair-keystore-config.yaml")).unwrap();
        assert!(yaml.contains(&chosen.join("lair").display().to_string()), "paths rewritten: {}", yaml);
        assert!(!yaml.contains(&root.join("lair").display().to_string()));
        let profiles = Profiles::load(root);
        assert_eq!(profiles.active.as_deref(), Some(pk.as_str()));
        assert_eq!(profiles.profiles[&pk].identity.as_deref(), Some(KEY_A));
        // second launch, Vault closed: same profile, no relocation
        assert_eq!(select_profile_root(root, None), chosen);
        assert!(!legacy_layout_present(root));
    }

    #[test]
    fn a_failed_move_rolls_back_and_stays_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        legacy_install(root, Some(KEY_A));
        let pk = partition_key(KEY_A).unwrap();
        // a stray conductor dir in the target makes that rename refuse
        std::fs::create_dir_all(profile_root(root, &pk).join("conductor")).unwrap();
        let chosen = select_profile_root(root, None);
        assert_eq!(chosen, root);
        for rel in ["lair-passphrase", "lair/store_file", "conductor/databases/x", "identity-link.json"] {
            assert!(root.join(rel).exists(), "{} back at the root", rel);
        }
        let yaml = std::fs::read_to_string(root.join("lair/lair-keystore-config.yaml")).unwrap();
        assert!(yaml.contains(&root.join("lair").display().to_string()), "yaml untouched on rollback");
    }
}
