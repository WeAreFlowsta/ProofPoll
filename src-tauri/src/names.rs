//! Every name a fork changes, in one place.
//!
//! `scripts/rename-app.mjs` rewrites this file together with the three
//! files that cannot read it (`tauri.conf.json`, `Cargo.toml`,
//! `package.json`) and the scripts that name the sidecars. Nothing else in
//! `src-tauri/src` spells the app's name, identifier, sidecars or ports.
//!
//! What is NOT here, because it belongs to the app's DNA rather than to its
//! name: the hApp bundle files and the installed app ids in `dna.rs`
//! (`proofpoll_v1_*`), which a fork replaces with its own DNA's.

/// What people see: window title, dialogs, the name the Vault shows for
/// this app, the name inside backups and exports.
pub const APP_NAME: &str = "ProofPoll";

/// The bundle identifier (`tauri.conf.json` has the same value). It names
/// the data folder on every system, so changing it on a released app moves
/// people's data out from under them.
pub const APP_IDENTIFIER: &str = "com.proofpoll.app";

/// The log file's name and, on macOS, its folder under `~/Library/Logs`.
pub const LOG_FILE_NAME: &str = "proofpoll";
pub const LOG_FOLDER_NAME: &str = "ProofPoll";

/// The role id in the hApp manifest (`happ.yaml`): what every zome call names.
pub const ROLE_NAME: &str = "proofpoll";

/// What every app this program installs on its conductors starts with. The
/// lines library refuses to uninstall anything else.
pub const INSTALLED_APP_PREFIX: &str = "proofpoll_v1_";

/// The sidecars, as `tauri.conf.json` `externalBin` names them. They carry
/// the app's own prefix so they never collide with another Holochain app's
/// conductor on the same computer, and so the startup sweep can tell its
/// own orphans from anyone else's processes.
pub const LAIR_06: &str = "proofpoll-lair-keystore";
pub const CONDUCTOR_06: &str = "proofpoll-holochain-06";
pub const LAIR_07: &str = "proofpoll-lair-keystore-07";
pub const CONDUCTOR_07: &str = "proofpoll-holochain";
/// All four, for the orphan sweep. With only two listed, an orphaned 0.6
/// conductor kept its port and the next launch refused to start.
pub const SIDECARS: &[&str] = &[CONDUCTOR_07, CONDUCTOR_06, LAIR_06, LAIR_07];

/// The conductors' admin ports: 0.6 first, then 0.7. Different from Flowsta
/// Vault's (4455) and Your Own AI's (4477) so all run on one computer;
/// change them if another Holochain app of yours runs beside this one.
pub const ADMIN_PORT_06: u16 = 4466;
pub const ADMIN_PORT_07: u16 = 4467;

/// The prefix of the random origin the conductors' interfaces accept.
pub const ORIGIN_PREFIX: &str = "pp";

/// The key store tag the device's seed is kept under, and the file the
/// seed's recovery copy is written to.
pub const SEED_TAG: &str = "proofpoll-device-seed";
pub const RECOVERY_FILE: &str = "proofpoll-recovery.json";

#[cfg(test)]
mod tests {
    use super::*;

    /// `tauri.conf.json` cannot read this file: the two must say the same,
    /// or the app looks for sidecars the installer never shipped and for a
    /// data folder under another identifier.
    #[test]
    fn the_app_config_agrees_with_the_names() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["productName"], APP_NAME);
        assert_eq!(conf["identifier"], APP_IDENTIFIER);
        let mut bundled: Vec<String> = conf["bundle"]["externalBin"].as_array().unwrap().iter().map(|b| b.as_str().unwrap().trim_start_matches("binaries/").to_string()).collect();
        bundled.sort();
        let mut ours: Vec<String> = SIDECARS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(bundled, ours, "the sidecars the installer ships are the ones the app starts and sweeps");
    }

    #[test]
    fn the_two_lines_have_their_own_ports_and_the_sidecars_one_prefix() {
        assert_ne!(ADMIN_PORT_06, ADMIN_PORT_07);
        let prefix = LAIR_06.trim_end_matches("-lair-keystore");
        assert!(SIDECARS.iter().all(|s| s.starts_with(&format!("{prefix}-"))), "one prefix, so the startup sweep and the rename script find all four");
        assert!(!APP_IDENTIFIER.is_empty() && APP_IDENTIFIER.split('.').count() >= 3);
    }
}
