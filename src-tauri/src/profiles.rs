//! Identity profiles: one ProofPoll agent, key store, conductor and set of
//! link files per Flowsta identity that has connected to this install.
//!
//! Layout (Phase 2 of the identity switcher, build-docs
//! current/VAULT_1_4_0_PHASE2_BUILD.md step 8):
//!
//!   <app data dir>/profiles.json                 device-level index
//!   <app data dir>/profiles/<folder>/             one profile:
//!       lair-passphrase, lair/, conductor/, proofpoll-recovery.json (+ .replaced-*),
//!       identity-link.json, profile-cache.json, migration-*-state.json,
//!       adopt-relink-pending
//!
//! `<folder>` is the partition key of the Flowsta identity the profile is
//! bound to (first 16 hex chars of sha256 over the 39-byte agent key - the
//! same key the Vault uses for its own partitions), or `local` for an install
//! that has not linked to any identity yet; `local` binds to an identity the
//! first time it links and keeps its folder name.
//!
//! Installs from before this module keep everything at the app data dir
//! root. The first start with this code moves that layout into a profile
//! folder, all-or-nothing, by same-filesystem renames. The key store moves
//! WITH its files and its config's absolute paths are rewritten - unlike the
//! Vault it is never set aside, because `AppState::new` treats a passphrase
//! without a store as corruption and wipes the conductor.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PROFILES_FILE: &str = "profiles.json";
pub const PROFILES_DIR: &str = "profiles";
pub const LOCAL_PROFILE: &str = "local";
pub const PARTITION_KEY_LEN: usize = 16;

/// Single files that belong to a profile (directories: `lair`, `conductor`).
pub const PROFILE_FILES: &[&str] = &[
    "lair-passphrase",
    crate::device_seed::RECOVERY_FILE,
    "identity-link.json",
    "profile-cache.json",
    "adopt-relink-pending",
];

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ProfileInfo {
    /// Flowsta agent key this profile is bound to; None until it links.
    pub identity: Option<String>,
    pub created_at: i64,
    pub last_used: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Profiles {
    pub version: u32,
    /// Folder name of the profile in use.
    pub active: Option<String>,
    pub profiles: BTreeMap<String, ProfileInfo>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn profiles_path(device_root: &Path) -> PathBuf { device_root.join(PROFILES_FILE) }
pub fn profiles_dir(device_root: &Path) -> PathBuf { device_root.join(PROFILES_DIR) }
pub fn profile_root(device_root: &Path, folder: &str) -> PathBuf { profiles_dir(device_root).join(folder) }

/// First 16 hex chars of sha256 over the decoded 39-byte agent key.
/// Identical to the Vault's partition key. None if the string is not a key.
pub fn partition_key(agent_pub_key: &str) -> Option<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let b64 = agent_pub_key.trim().strip_prefix('u')?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64).ok()?;
    if raw.len() != 39 { return None; }
    Some(hex::encode(Sha256::digest(&raw))[..PARTITION_KEY_LEN].to_string())
}

impl Profiles {
    pub fn load(device_root: &Path) -> Profiles {
        let p = profiles_path(device_root);
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("profiles.json unreadable ({}), starting a fresh index", e);
                Profiles { version: 1, ..Default::default() }
            }),
            Err(_) => Profiles { version: 1, ..Default::default() },
        }
    }

    pub fn save(&self, device_root: &Path) -> Result<(), String> {
        let p = profiles_path(device_root);
        let tmp = p.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    pub fn folder_for_identity(&self, agent_pub_key: &str) -> Option<String> {
        self.profiles.iter().find(|(_, i)| i.identity.as_deref() == Some(agent_pub_key)).map(|(f, _)| f.clone())
    }

    pub fn unbound_folder(&self) -> Option<String> {
        self.profiles.iter().find(|(_, i)| i.identity.is_none()).map(|(f, _)| f.clone())
    }

    fn touch(&mut self, folder: &str, identity: Option<&str>) {
        let e = self.profiles.entry(folder.to_string()).or_insert_with(|| ProfileInfo { identity: None, created_at: now(), last_used: now() });
        if let Some(id) = identity { e.identity = Some(id.to_string()); }
        e.last_used = now();
        self.active = Some(folder.to_string());
    }
}

/// True when this install still keeps its identity files at the root.
pub fn legacy_layout_present(device_root: &Path) -> bool {
    device_root.join("lair").is_dir()
        || device_root.join("conductor").is_dir()
        || device_root.join(crate::device_seed::RECOVERY_FILE).exists()
        || device_root.join("identity-link.json").exists()
}

/// Whether the key store socket under this root fits the platform's Unix
/// socket path limit (Linux 108, macOS 104, minus a margin; Windows named
/// pipes always fit).
pub fn lair_socket_path_fits(root: &Path) -> bool {
    #[cfg(windows)]
    { let _ = root; true }
    #[cfg(not(windows))]
    {
        let limit: usize = if cfg!(target_os = "macos") { 104 } else { 108 };
        root.join("lair").join("socket").as_os_str().len() + 1 <= limit.saturating_sub(4)
    }
}

fn profile_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for name in PROFILE_FILES {
        let p = root.join(name);
        if p.exists() { out.push(p); }
    }
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let is_migration_state = name.starts_with("migration-") && name.ends_with("-state.json");
            let is_replaced_recovery = name.starts_with("proofpoll-recovery.replaced-");
            if (is_migration_state || is_replaced_recovery) && e.path().is_file() { out.push(e.path()); }
        }
    }
    for dir in ["conductor", "lair"] {
        let p = root.join(dir);
        if p.is_dir() { out.push(p); }
    }
    out
}

/// Lair's config pins absolute paths (connectionUrl, pidFile, storeFile).
/// After the directory moved, point them at the new location. The socket
/// address is a percent-encoded URL, so this goes through the URL-aware
/// repoint, never a plain text replace.
fn rewrite_lair_paths(new_lair_dir: &Path, _old_lair_dir: &Path) -> Result<(), String> {
    crate::lair::repoint_config(new_lair_dir).map(|_| ())
}

/// Move the legacy root layout into `profiles/<folder>/`, all-or-nothing.
pub fn relocate_legacy(device_root: &Path, folder: &str) -> Result<PathBuf, String> {
    let root = profile_root(device_root, folder);
    if !lair_socket_path_fits(&root) {
        return Err(format!("profile path too long for the key store socket ({} bytes)", root.as_os_str().len()));
    }
    if root.join("lair").exists() || root.join(crate::device_seed::RECOVERY_FILE).exists() {
        return Err(format!("{:?} already holds a profile", root));
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {:?}: {}", root, e))?;
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for from in profile_entries(device_root) {
        let to = root.join(from.file_name().unwrap());
        if to.exists() {
            rollback(&done);
            return Err(format!("{:?} already exists in the profile", to));
        }
        if let Err(e) = std::fs::rename(&from, &to) {
            rollback(&done);
            return Err(format!("could not move {:?}: {}", from, e));
        }
        done.push((from, to));
    }
    if let Err(e) = rewrite_lair_paths(&root.join("lair"), &device_root.join("lair")) {
        rollback(&done);
        return Err(format!("could not rewrite the key store paths: {}", e));
    }
    Ok(root)
}

fn rollback(done: &[(PathBuf, PathBuf)]) {
    for (from, to) in done.iter().rev() {
        if let Err(e) = std::fs::rename(to, from) {
            log::error!("profile relocation rollback: could not restore {:?}: {}", from, e);
        }
    }
}

/// Decide which profile this launch runs, moving a legacy layout first if
/// there is one. `live_identity` = the Vault's unlocked agent key when the
/// Vault is reachable and unlocked at launch, else None.
pub fn select_profile_root(device_root: &Path, live_identity: Option<&str>) -> PathBuf {
    let mut profiles = Profiles::load(device_root);

    if legacy_layout_present(device_root) {
        let link = crate::commands::load_identity_link(device_root);
        let bound = link.as_ref().map(|l| l.vault_agent_pub_key.clone());
        let folder = bound.as_deref().and_then(partition_key).unwrap_or_else(|| LOCAL_PROFILE.to_string());
        match relocate_legacy(device_root, &folder) {
            Ok(root) => {
                profiles.touch(&folder, bound.as_deref());
                if let Err(e) = profiles.save(device_root) { log::warn!("profiles.json not saved: {}", e); }
                log::info!("Identity files moved into profile {:?}", root);
            }
            Err(e) if profile_root(device_root, &folder).join("lair").exists() => {
                // The move already happened; something recreated a legacy
                // name at the device root. The profile is the truth - the
                // legacy root would open as an empty identity.
                log::warn!("Legacy names at the device root beside a finished profile ({}) - using the profile", e);
            }
            Err(e) => {
                log::warn!("Profile relocation skipped: {} - staying on the legacy layout", e);
                return device_root.to_path_buf();
            }
        }
    }

    let folder = match live_identity {
        Some(id) => profiles
            .folder_for_identity(id)
            .or_else(|| profiles.unbound_folder())
            .or_else(|| partition_key(id))
            .unwrap_or_else(|| LOCAL_PROFILE.to_string()),
        None => profiles
            .active
            .clone()
            .or_else(|| profiles.profiles.keys().next().cloned())
            .unwrap_or_else(|| LOCAL_PROFILE.to_string()),
    };
    let root = profile_root(device_root, &folder);
    if !lair_socket_path_fits(&root) {
        log::warn!("Profile path {:?} too long for the key store socket - using the legacy root", root);
        return device_root.to_path_buf();
    }
    if let Err(e) = std::fs::create_dir_all(&root) {
        log::warn!("cannot create profile {:?}: {} - using the legacy root", root, e);
        return device_root.to_path_buf();
    }
    // Only record a binding the profile already has; linking binds later.
    let known = profiles.profiles.get(&folder).and_then(|p| p.identity.clone());
    profiles.touch(&folder, known.as_deref());
    if let Err(e) = profiles.save(device_root) { log::warn!("profiles.json not saved: {}", e); }
    root
}

/// Record that the profile at `profile_root` now belongs to `agent_pub_key`
/// (called when an identity link is saved).
pub fn bind_profile(device_root: &Path, profile_root_path: &Path, agent_pub_key: &str) {
    let Some(folder) = profile_root_path.file_name().map(|f| f.to_string_lossy().to_string()) else { return };
    if profile_root_path.parent() != Some(&profiles_dir(device_root)) { return; } // legacy root: nothing to record
    let mut profiles = Profiles::load(device_root);
    profiles.touch(&folder, Some(agent_pub_key));
    if let Err(e) = profiles.save(device_root) { log::warn!("profiles.json not saved: {}", e); }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn partition_key_matches_the_vaults_shape() {
        let k = partition_key(KEY_A).unwrap();
        assert_eq!(k.len(), PARTITION_KEY_LEN);
        assert_eq!(k, k.to_lowercase());
        assert_ne!(partition_key(KEY_B).unwrap(), k);
        assert_eq!(partition_key("not a key"), None);
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
    fn unlinked_legacy_install_becomes_local_and_binds_on_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        legacy_install(root, None);
        let chosen = select_profile_root(root, None);
        assert_eq!(chosen, profile_root(root, LOCAL_PROFILE));
        assert!(Profiles::load(root).profiles[LOCAL_PROFILE].identity.is_none());
        // the Vault appears with identity A: the unbound local profile is reused, then binds
        assert_eq!(select_profile_root(root, Some(KEY_A)), chosen);
        bind_profile(root, &chosen, KEY_A);
        let p = Profiles::load(root);
        assert_eq!(p.folder_for_identity(KEY_A).as_deref(), Some(LOCAL_PROFILE));
        // a different identity gets its own new profile
        let other = select_profile_root(root, Some(KEY_B));
        assert_eq!(other, profile_root(root, &partition_key(KEY_B).unwrap()));
        assert!(other.is_dir());
        // and A still resolves to local
        assert_eq!(select_profile_root(root, Some(KEY_A)), chosen);
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

    #[test]
    fn fresh_install_starts_in_local_or_the_live_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(select_profile_root(root, None), profile_root(root, LOCAL_PROFILE));
        let dir2 = tempfile::tempdir().unwrap();
        assert_eq!(select_profile_root(dir2.path(), Some(KEY_B)), profile_root(dir2.path(), &partition_key(KEY_B).unwrap()));
    }
}
