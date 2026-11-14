//! Tauri commands for ProofPoll.
//!
//! This is the bridge between the Qwik frontend and the Holochain conductor.
//! All zome calls go through the Rust backend via AppWebsocket — the frontend
//! uses lightweight Tauri `invoke()` calls and never touches @holochain/client.
//!
//! ## For forking developers
//!
//! This file is plumbing any Holochain desktop app keeps: `AppState`,
//! `call_zome`, `friendly_error`, `decode_entry`, the status command,
//! Flowsta identity linking, the Vault backup and export, the restore
//! screen's two buttons. What is about POLLS (the entry types and every
//! command the page calls for polls, votes, flags, drafts and vote
//! reasons) is in the `polls/` folder: that is the part to replace.
//!
//! Two functions here still read poll records and change with your data
//! model: `classify_app_entry` (the backup names each record) and
//! `get_export_data` (the export decodes them).
//!
//! After changing commands, register them in `lib.rs` → `invoke_handler`.

use crate::conductor::ConductorStatus;
use holochain_types::prelude::{ActionHash, AgentPubKey};
use lair_keystore_api::prelude::LairClient;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[allow(unused_imports)]
use crate::polls::*;

// --- App state ---

pub struct AppState {
    /// This profile's root: key store, conductor, recovery file, link files.
    pub data_dir: PathBuf,
    /// The app data dir: holds profiles.json and the profiles/ folder.
    pub device_root: PathBuf,
    /// This launch came from "Open as this identity" (the page starts the
    /// sign-in for a profile that is not linked yet; the window refocuses).
    pub relaunched_into_identity: std::sync::atomic::AtomicBool,
    pub conductor_status: Mutex<ConductorStatus>,
    pub agent_pub_key: Mutex<Option<String>>,
    /// The key the 0.7 line (v1.4) runs as: the same as `agent_pub_key`
    /// unless the 0.6 agent was conductor-born, before the app-held seed.
    pub agent_pub_key_07: Mutex<Option<String>>,
    /// Active (v1.5, Holochain 0.7.1) app client for all reads and writes.
    pub app_client: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    /// v1.4 app client (the same conductor): the move's source, and reads of
    /// what is not carried yet.
    pub app_client_v1_4: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    /// v1.3 app client (Holochain 0.6.1): the migration source, and reads and
    /// votes for polls not yet migrated.
    pub app_client_v1_3: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    /// v1.2 app client for legacy reads.
    pub app_client_v1_2: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    /// v1.1 app client for legacy reads.
    pub app_client_v1_1: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    /// v1.0 app client for legacy reads.
    pub app_client_v1_0: tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    pub passphrase: Mutex<String>,
    pub lair_client: tokio::sync::Mutex<Option<LairClient>>,
    /// Current migration state (persisted to disk).
    pub migration_state: tokio::sync::Mutex<crate::migration::MigrationState>,
    /// What the last Vault backup attempt did, for the Identity page and
    /// proofpoll.log. The SDK and the write trigger only ever said it in the
    /// webview console, so a stale backup date had no explanation (P6).
    pub last_backup_outcome: Mutex<Option<BackupOutcome>>,
    /// A poll on the active network (its hash) -> the older hashes it came
    /// from (its first record's, and each later copy's), from the active
    /// line's mappings; `None` until first read. Votes and flags still on an
    /// older line under one of those hashes (not carried yet) are read from
    /// there as well.
    pub migrated_from: Mutex<Option<std::collections::HashMap<String, Vec<String>>>>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let device_root = data_dir.clone();
        Self::new_with_device_root(data_dir, device_root)
    }

    pub fn new_with_device_root(data_dir: PathBuf, device_root: PathBuf) -> Self {
        let passphrase_path = data_dir.join("lair-passphrase");
        let lair_config_path = data_dir.join("lair").join("lair-keystore-config.yaml");
        let lair_store_path = data_dir.join("lair").join("store_file");

        // These three files are encryption-paired: lair-passphrase derives the
        // crypto key, the config holds matching salts, store_file is encrypted
        // under them. Either ALL three are present (a working install) or ALL
        // three are absent (a true fresh install). Any other combination means
        // an uninstall+reinstall removed some files but not others, leaving
        // orphaned state encrypted under a passphrase or salts that no longer
        // exist — every attempt to start lair from that state crashes with
        // `sqlcipher_page_cipher: hmac check failed`. Detect that mismatch
        // here and wipe everything for a clean restart.
        //
        // The wiped state is already unusable (lair cannot open it), so
        // nothing recoverable is lost HERE - but be precise about what a
        // fresh keystore means: the new install authors under a NEW agent
        // key. Polls/votes stay visible on the shared network under the old
        // key; there is no flow that re-adopts their authorship.
        let pp = passphrase_path.exists();
        let cfg = lair_config_path.exists();
        let store = lair_store_path.exists();
        let all_present = pp && cfg && store;
        let all_absent = !pp && !cfg && !store;
        if !all_present && !all_absent {
            log::warn!(
                "Inconsistent lair state on startup (lair-passphrase={}, config={}, store_file={}). Wiping lair + conductor data dirs to recover.",
                pp, cfg, store,
            );
            let _ = std::fs::remove_file(&passphrase_path);
            for dir in crate::conductor::LINE_DIRS {
                let _ = std::fs::remove_dir_all(data_dir.join(dir));
            }
        }

        let passphrase = if passphrase_path.exists() {
            std::fs::read_to_string(&passphrase_path).unwrap_or_else(|_| generate_passphrase())
        } else {
            let p = generate_passphrase();
            if let Err(e) = std::fs::write(&passphrase_path, &p) {
                log::error!("Failed to persist lair passphrase to {:?}: {} — next launch will regenerate and wipe state", passphrase_path, e);
            } else {
                log::info!("Generated new lair passphrase at {:?}", passphrase_path);
            }
            p
        };

        let migration_state = crate::migration::MigrationState::load(&data_dir);

        Self {
            data_dir,
            device_root,
            relaunched_into_identity: std::sync::atomic::AtomicBool::new(false),
            conductor_status: Mutex::new(ConductorStatus::Stopped),
            agent_pub_key: Mutex::new(None),
            agent_pub_key_07: Mutex::new(None),
            app_client: tokio::sync::Mutex::new(None),
            app_client_v1_4: tokio::sync::Mutex::new(None),
            app_client_v1_3: tokio::sync::Mutex::new(None),
            app_client_v1_2: tokio::sync::Mutex::new(None),
            app_client_v1_1: tokio::sync::Mutex::new(None),
            app_client_v1_0: tokio::sync::Mutex::new(None),
            passphrase: Mutex::new(passphrase),
            lair_client: tokio::sync::Mutex::new(None),
            migration_state: tokio::sync::Mutex::new(migration_state),
            last_backup_outcome: Mutex::new(None),
            migrated_from: Mutex::new(None),
        }
    }
}

/// Wipe every encryption-paired file under data_dir, generate a fresh
/// random passphrase, persist it, and update the AppState mutex so any
/// subsequent code path sees the new value. Returns the new passphrase
/// for the caller to use immediately.
///
/// Called from `conductor::start_holochain` when the first attempt fails
/// with a SQLCipher hmac mismatch — the only recoverable response to
/// that error is to drop the orphaned encrypted state and start fresh.
/// The dropped state is already unopenable, so nothing recoverable is lost
/// here - but the reset is not free either: the fresh keystore means a NEW
/// agent key. Polls/votes stay visible on the shared network under the old
/// key; no flow re-adopts their authorship.
pub fn nuke_state_and_regenerate_passphrase(data_dir: &Path, app_state: &AppState) -> String {
    log::warn!(
        "Performing full state reset under {:?} (lair-passphrase + both lines' lair and conductor dirs)",
        data_dir,
    );
    let passphrase_path = data_dir.join("lair-passphrase");
    if let Err(e) = std::fs::remove_file(&passphrase_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("remove lair-passphrase: {}", e);
        }
    }
    for dir in crate::conductor::LINE_DIRS {
        if let Err(e) = std::fs::remove_dir_all(data_dir.join(dir)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("remove_dir_all {}: {}", dir, e);
            }
        }
    }

    let new_passphrase = generate_passphrase();
    if let Err(e) = std::fs::write(&passphrase_path, &new_passphrase) {
        log::error!(
            "Failed to write regenerated lair-passphrase to {:?}: {}",
            passphrase_path, e,
        );
    }

    *app_state.passphrase.lock().unwrap() = new_passphrase.clone();
    log::info!("State reset complete; new passphrase persisted and in-memory");
    new_passphrase
}

pub(crate) fn generate_passphrase() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

// ── Helpers (reusable infrastructure) ──────────────────────────────────
//
// These constants and functions work for any Holochain app.
// Change ROLE_NAME to match your happ.yaml role id.
// Change POLLS_ZOME to match your coordinator zome name.

/// User-facing app name - shown in Vault dialogs and carried in backup
/// payloads and data exports. Forks: change once here; the frontend has its
/// own copies (see the README's rename checklist).
pub(crate) const APP_NAME: &str = crate::names::APP_NAME;
/// Must match the role `id` in your happ.yaml.
pub(crate) const ROLE_NAME: &str = crate::names::ROLE_NAME;
/// Your app's coordinator zome name (from dna.yaml).
pub(crate) const POLLS_ZOME: &str = "polls";
/// Flowsta agent-linking zome — keep as-is for identity integration.
pub(crate) const AGENT_LINKING_ZOME: &str = "agent_linking";

pub(crate) fn decode_entry<T: serde::de::DeserializeOwned>(record: &crate::hc::RecordView) -> Result<T, String> {
    record.entry_as()
}

/// Call a zome function on the active DNA version.
///
/// This is the core helper that all Tauri commands use. It handles:
///   - Routing to the correct zome via ROLE_NAME
///   - Auto-recovery from CellDisabled errors (re-enables the app and retries)
///   - User-friendly error messages via `friendly_error()`
///
/// [`call_zome`], waiting at most `secs` for the answer. A read that asks the
/// network can wait minutes on authorities that have gone away (a conductor
/// that has just reconnected); a list shows what it has instead.
pub(crate) async fn call_zome_within(
    client: &crate::hc::AppClient,
    zome: &str,
    fn_name: &str,
    payload: Vec<u8>,
    secs: u64,
) -> Result<Vec<u8>, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(secs), call_zome(client, zome, fn_name, payload)).await {
        Ok(r) => r,
        Err(_) => Err(format!("{}.{} did not answer within {} s", zome, fn_name, secs)),
    }
}

/// How long a list waits on the network before it shows what this device holds.
pub(crate) const LIST_WAIT_SECS: u64 = 15;

/// A read of `fn_name` on the active line that waits at most
/// [`LIST_WAIT_SECS`] on the network, then shows what this device holds
/// (the coordinator's `<fn_name>_local`).
pub(crate) async fn read_or_local(client: &crate::hc::AppClient, fn_name: &str, payload: Vec<u8>) -> Result<Vec<u8>, String> {
    match call_zome_within(client, POLLS_ZOME, fn_name, payload.clone(), LIST_WAIT_SECS).await {
        Ok(r) => Ok(r),
        Err(e) => {
            log::warn!("[polls] {} did not come from the network ({}): showing what this device holds", fn_name, e);
            call_zome(client, POLLS_ZOME, &format!("{}_local", fn_name), payload).await
        }
    }
}

/// For forking: you don't need to change this function — just call it
/// with your zome name and function name from your Tauri commands.
/// Call a zome function on either line. `payload` and the result are the
/// msgpack bytes (`hc::encode` / `hc::decode`, `hc::records`).
/// When the app last asked a conductor to write (unix seconds), and how many
/// calls are waiting for their answer now.
pub(crate) static LAST_WRITE_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static CALLS_IN_FLIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A call that only reads: the coordinators name every read `get_...`.
/// The pages read by themselves (the poll list every 30 s, the sign-in
/// state every 15 s), so a read is no sign that anyone is at work.
pub(crate) fn only_reads(fn_name: &str) -> bool {
    fn_name.starts_with("get_")
}

/// Whether this is a quiet moment: no call waiting for its answer, and the
/// last write at least a minute ago.
pub(crate) fn quiet_at(now_secs: u64, last_write_secs: u64, in_flight: u64) -> bool {
    in_flight == 0 && now_secs.saturating_sub(last_write_secs) >= 60
}

/// One call waiting for its answer, until dropped.
struct InFlight;
impl InFlight {
    fn new() -> Self {
        CALLS_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        InFlight
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        CALLS_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A quiet moment: nothing written for a minute and no call waiting. A
/// restart of the lines for their memory waits for one
/// (`conductor::spawn_watch`). The pages' own reads do not count: with a
/// window open they never leave a minute free (11-14, the matrix's watch
/// leg: ten minutes over the size and no restart).
pub(crate) fn quiet_now(_state: &AppState) -> bool {
    use std::sync::atomic::Ordering::SeqCst;
    quiet_at(unix_secs(), LAST_WRITE_SECS.load(SeqCst), CALLS_IN_FLIGHT.load(SeqCst))
}

pub(crate) async fn call_zome(
    client: &crate::hc::AppClient,
    zome: &str,
    fn_name: &str,
    payload: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let _waiting = InFlight::new();
    if !only_reads(fn_name) {
        LAST_WRITE_SECS.store(unix_secs(), std::sync::atomic::Ordering::SeqCst);
    }
    let result = client.call_zome(ROLE_NAME, zome, fn_name, payload.clone()).await;
    match result {
        Ok(r) => Ok(r),
        Err(err_str) => {
            // 0.6 says CellDisabled, 0.7 says CellNotRunning: both mean "not yet".
            if err_str.contains("CellDisabled") || err_str.contains("CellNotRunning") {
                log::warn!("Cell not running, attempting auto-recovery...");
                if let Err(re) = try_reenable_app(client.line()).await {
                    log::error!("Auto-recovery failed: {}", re);
                    return Err(friendly_error(&err_str));
                }
                log::info!("Retrying zome call after re-enabling app...");
                client
                    .call_zome(ROLE_NAME, zome, fn_name, payload)
                    .await
                    .map_err(|raw| {
                        log::warn!("[zome] {}.{} failed after re-enable: {}", zome, fn_name, raw);
                        friendly_error(&raw)
                    })
            } else {
                // The raw error lands in proofpoll.log even when the UI
                // shows the friendly version.
                log::warn!("[zome] {}.{} failed: {}", zome, fn_name, err_str);
                Err(friendly_error(&err_str))
            }
        }
    }
}

/// Attempt to re-enable the app via admin websocket.
pub(crate) async fn try_reenable_app(line: crate::hc::Line) -> Result<(), String> {
    match line {
        crate::hc::Line::V06 => {
            use holochain_client::AdminWebsocket;
            let admin_ws = AdminWebsocket::connect(
                format!("localhost:{}", crate::conductor::admin_port()),
                Some(crate::conductor::node_origin()),
            )
            .await
            .map_err(|e| format!("Failed to connect to admin WS for recovery: {}", e))?;
            admin_ws
                .enable_app(crate::dna::ACTIVE_APP_ID_06.to_string())
                .await
                .map_err(|e| format!("Failed to re-enable app: {}", e))?;
        }
        crate::hc::Line::V07 => {
            use holochain_client_07::AdminWebsocket;
            let admin_ws = AdminWebsocket::connect(
                format!("localhost:{}", crate::conductor::admin_port_07()),
                Some(crate::conductor::node_origin()),
            )
            .await
            .map_err(|e| format!("Failed to connect to the 0.7 admin WS for recovery: {}", e))?;
            admin_ws
                .enable_app(crate::dna::ACTIVE_APP_ID.to_string())
                .await
                .map_err(|e| format!("Failed to re-enable app: {}", e))?;
        }
    }
    log::info!("App re-enabled successfully");
    Ok(())
}

/// Translate raw Holochain errors into user-friendly messages.
pub(crate) fn friendly_error(raw: &str) -> String {
    if raw.contains("CellDisabled") {
        "Your data cell was temporarily disabled and could not be recovered automatically. Please restart the app.".into()
    } else if raw.contains("WebsocketError") || raw.contains("ConnectionReset") || raw.contains("Io(") {
        "Lost connection to the Holochain conductor. It may have stopped unexpectedly.".into()
    } else if raw.contains("timeout") || raw.contains("Timeout") {
        "The request timed out. The network may be slow or unreachable.".into()
    } else if raw.contains("CellNotRunning") {
        "Your data cell is not running yet. Give it a moment and try again.".into()
    } else if raw.contains("Conductor returned an error") {
        // Strip the nested conductor error wrapper for clarity
        if let Some(inner) = raw.split("InternalError(\"").nth(1) {
            let inner = inner.trim_end_matches("\")");
            format!("Conductor error: {}", inner)
        } else {
            format!("Conductor error: {}", raw)
        }
    } else {
        format!("Something went wrong: {}", raw)
    }
}

/// Parse a uhCAk... agent key string into an AgentPubKey.
/// Decodes the base64url body and preserves the exact 39 bytes (including
/// DHT location) so the key matches what the external signer used.
pub(crate) fn parse_agent_pub_key_string(s: &str) -> Result<AgentPubKey, String> {
    Ok(AgentPubKey::from_raw_39(flowsta_vault_client::identity::parse_agent_key(s)?))
}

// ── Status command (infrastructure — keep as-is) ──────────────────────

#[derive(serde::Serialize)]
pub struct AppStatus {
    pub ready: bool,
    pub agent_pub_key: Option<String>,
    /// The key v1.4 runs as (see `AppState::agent_pub_key_07`).
    pub agent_pub_key_07: Option<String>,
    pub conductor_status: ConductorStatus,
    /// This launch was asked for by "Open as this identity".
    pub relaunched_into_identity: bool,
    /// This start's agent links are on v1.5 (`carry_move::links_carried`).
    pub links_carried: bool,
}

#[tauri::command]
pub fn get_app_status(state: tauri::State<'_, std::sync::Arc<AppState>>) -> AppStatus {
    let status = state.conductor_status.lock().unwrap().clone();
    let agent_key = state.agent_pub_key.lock().unwrap().clone();
    let ready = matches!(status, ConductorStatus::Ready { .. });

    // Debug: the layout asks every 30 s; at info this line was most of the log.
    log::debug!("get_app_status called: ready={}", ready);

    AppStatus {
        ready,
        agent_pub_key: agent_key,
        agent_pub_key_07: state.agent_pub_key_07.lock().unwrap().clone(),
        conductor_status: status,
        relaunched_into_identity: state
            .relaunched_into_identity
            .load(std::sync::atomic::Ordering::Relaxed),
        links_carried: crate::carry_move::links_carried(),
    }
}

/// Best-effort: open the Flowsta Vault desktop app if it's installed but not
/// running, so the user doesn't have to go find it during Flowsta sign-in.
/// Tries the usual install locations per OS; returns an error string if it
/// can't find/launch it (the UI then just asks the user to open it manually).
#[tauri::command]
pub fn launch_vault() -> Result<(), String> {
    // The Vault decides its own window's drawing: it never inherits what
    // this app's webview fix set.
    flowsta_vault_client::vault::launch_vault(crate::webview_fix::set_here())
}

// ── Identity link persistence (Flowsta infrastructure — keep as-is) ───
//
// The identity link is persisted to a JSON file so the app can detect
// a previously-linked Vault user across restarts and DNA migrations.
// This data is separate from the DHT — it's local to this device.
//
// Two files work together:
//   - identity-link.json — stores vault_agent_pub_key + entry_action_hash
//     (proves this user linked with Flowsta; survives DNA migrations)
//   - profile-cache.json — stores display_name + profile_picture
//     (allows the app to show the user's identity without Vault running)

// The files and their formats are `flowsta_vault_client::identity`.
pub use flowsta_vault_client::identity::{CachedProfile, IdentityLinkData};
pub(crate) use flowsta_vault_client::identity::{delete_identity_link, load_cached_profile, load_identity_link, save_cached_profile, save_identity_link};

// ── Profile persistence (survives Vault being closed/locked) ──────────
//
// The Flowsta Vault only needs to be running for the initial identity
// linking ceremony. After that, the display name and profile picture
// are cached locally in profile-cache.json. On subsequent startups,
// the app loads from cache immediately — no Vault dependency.
//
// The cache is refreshed whenever the Vault is available (layout.tsx
// polls /status every 3s). If the user changes their name or picture
// in the Vault, the cache updates automatically on next poll.

// ── Write authorization (the ONE chokepoint, two tiers) ───────────────
//
// Every command that writes to the shared DHT authorizes through one of
// these two functions. Forks: route new write commands through them too -
// per-command checks are how `publish_draft` once shipped with no gate.
//
// Tier 1 - `require_identity_link` (basic use: polls, votes, flags,
// drafts, deletes, backups). The user shouldn't have to unlock their
// Vault for everyday actions on a machine they already signed in on:
//   - no identity link on this device        → refuse (sign in first)
//   - Vault unlocked under a DIFFERENT key   → refuse (someone else's
//     Vault is demonstrably open on this machine - nothing may be
//     authored under this install's agent while they're present)
//   - Vault unreachable or locked            → ALLOW as the linked
//     identity (a locked Vault on the user's own machine is the normal
//     resting state, not an anomaly)
//
// Tier 2 - `require_identity_match` (identity-bearing operations that
// COPY FROM or REBIND to the live Vault: profile-cache refresh, seed
// adopt/re-key; the link ceremony has its own equivalent checks). These
// stay fail-closed on presence: unreachable or locked also refuses,
// because what they write comes from whoever is actually there.
//
// Reads keep working with no link at all - reading polls needs no proof
// of anything.

/// Human-readable refusals; the frontend shows these verbatim.
pub(crate) const ERR_NOT_LINKED: &str = "Sign in with Flowsta first";
pub(crate) const ERR_VAULT_UNCONFIRMED: &str =
    "Flowsta Vault isn't running - open it so ProofPoll can confirm it's you, then try again";
pub(crate) const ERR_VAULT_LOCKED: &str =
    "Your Flowsta Vault is locked - unlock it, then try again";
pub(crate) const ERR_IDENTITY_MISMATCH: &str =
    "Your Flowsta Vault is signed in as a different identity than the one connected here - \
     unlock the matching Vault, or disconnect and reconnect on the Identity page";

/// Live `(unlocked, agent_pub_key)` from this user's Vault, or None when
/// none answers. All three ports at once, another OS user's Vault ignored,
/// the unlocked one preferred (`vault_probe`); a slow Vault reads as
/// unconfirmable, which refuses (never allows).
pub(crate) async fn vault_live_identity() -> Option<(bool, Option<String>)> {
    crate::vault_probe::client().live_identity().await
}

/// A refusal in this app's words (the decisions are
/// `flowsta_vault_client::identity`).
fn refusal(why: flowsta_vault_client::identity::Refusal) -> String {
    use flowsta_vault_client::identity::Refusal;
    match why {
        Refusal::NotLinked => ERR_NOT_LINKED,
        Refusal::VaultNotRunning => ERR_VAULT_UNCONFIRMED,
        Refusal::VaultLocked => ERR_VAULT_LOCKED,
        Refusal::OtherIdentity => ERR_IDENTITY_MISMATCH,
    }
    .to_string()
}

pub(crate) async fn require_identity_match(
    state: &std::sync::Arc<AppState>,
) -> Result<IdentityLinkData, String> {
    flowsta_vault_client::identity::require_identity_match(crate::vault_probe::client(), &state.data_dir).await.map_err(refusal)
}

/// Tier-1 gate: the install must be signed in (identity link on disk), but
/// the Vault does not have to be running or unlocked. The one hard refusal
/// is a DIFFERENT identity demonstrably present - that is never ambiguous
/// and never allowed.
pub(crate) async fn require_identity_link(
    state: &std::sync::Arc<AppState>,
) -> Result<IdentityLinkData, String> {
    flowsta_vault_client::identity::require_identity_link(crate::vault_probe::client(), &state.data_dir).await.map_err(refusal)
}

#[tauri::command]
pub fn get_cached_profile(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Option<CachedProfile> {
    load_cached_profile(&state.data_dir)
}

#[tauri::command]
pub async fn save_profile_cache(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    display_name: Option<String>,
    profile_picture: Option<String>,
) -> Result<(), String> {
    // Rust-side gate, not frontend discipline: this cache is what public
    // votes publish to the shared DHT (irreversibly), and the frontend
    // refreshes it from /status in three places. A different identity's
    // name/photo must never land here.
    require_identity_match(&state).await?;
    save_cached_profile(
        &state.data_dir,
        &CachedProfile {
            display_name,
            profile_picture,
        },
    );
    Ok(())
}

// ── Identity linking commands (Flowsta infrastructure — keep as-is) ───
//
// These commands handle the Flowsta Vault ↔ app identity linking ceremony.
// The Vault signs an attestation that this app's agent key belongs to the
// same person as the Vault's agent key. This attestation is stored on the
// DHT via the agent_linking zome so anyone can verify it.

#[tauri::command]
pub async fn commit_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    vault_agent_pub_key: String,
    vault_signature: String,
) -> Result<String, String> {
    // Never silently REBIND this install to a different identity than the
    // one already linked - the launch-time auto-relink would otherwise
    // adopt whichever identity happens to be unlocked, and the next public
    // vote would publish their name under this agent key. Disconnecting
    // first is the explicit path.
    let previous = load_identity_link(&state.data_dir);
    if let Some(ref existing) = previous {
        if existing.vault_agent_pub_key != vault_agent_pub_key {
            return Err(ERR_IDENTITY_MISMATCH.into());
        }
    }
    // And the identity being bound must actually be the one present.
    let (unlocked, live) = vault_live_identity()
        .await
        .ok_or(ERR_VAULT_UNCONFIRMED)?;
    if !unlocked {
        return Err(ERR_VAULT_LOCKED.into());
    }
    if live.as_deref() != Some(vault_agent_pub_key.as_str()) {
        return Err(ERR_IDENTITY_MISMATCH.into());
    }

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let external_agent = parse_agent_pub_key_string(&vault_agent_pub_key)?;

    // Decode base64 signature to bytes.
    use base64::Engine;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&vault_signature)
        .map_err(|e| format!("Invalid signature: {}", e))?;

    #[derive(serde::Serialize, Debug)]
    struct CreateExternalLinkInput {
        external_agent: AgentPubKey,
        external_signature: Vec<u8>,
    }

    let input = CreateExternalLinkInput {
        external_agent,
        external_signature: sig_bytes,
    };
    let payload = crate::hc::encode(&input)?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "create_external_link", payload).await?;

    let action_hash: ActionHash = crate::hc::decode(&result)?;
    let action_hash_str = action_hash.to_string();

    // A re-link supersedes the previous entry - revoke it so repeated
    // links can't accumulate orphaned IsSamePerson entries on the DHT
    // (the file only ever remembers the newest hash). Best-effort: the
    // new link is already committed either way.
    if let Some(existing) = previous {
        if existing.entry_action_hash != action_hash_str {
            match ActionHash::try_from(existing.entry_action_hash.clone()) {
                Ok(old_hash) => {
                    if let Ok(payload) = crate::hc::encode(&old_hash) {
                        if let Err(e) =
                            call_zome(client, AGENT_LINKING_ZOME, "revoke_link", payload).await
                        {
                            // A re-link on a newer network version: the
                            // previous link is on the older one and stays
                            // there with it.
                            if e.contains("Entry not found") {
                                log::info!("the superseded identity link is on an older network version; left there");
                            } else {
                                log::warn!("could not revoke the superseded identity link: {}", e);
                            }
                        }
                    }
                }
                Err(e) => log::warn!("superseded identity link hash unparseable: {:?}", e),
            }
        }
    }

    // Persist the link data for later revocation
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    save_identity_link(
        &state.data_dir,
        &IdentityLinkData {
            vault_agent_pub_key: vault_agent_pub_key.clone(),
            entry_action_hash: action_hash_str.clone(),
            linked_at: now,
        },
    );
    // The profile this install runs in now belongs to that identity.
    crate::profiles::bind_profile(&state.device_root, &state.data_dir, &vault_agent_pub_key);

    // A successful sign-in completes step 2 of an adopt/re-key (if one was
    // pending) - retire the marker so the restore prompt stands down.
    crate::seed_adopt::clear_relink_marker(&state.data_dir);

    Ok(action_hash_str)
}

#[tauri::command]
pub fn get_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Option<IdentityLinkData> {
    load_identity_link(&state.data_dir)
}

#[tauri::command]
pub async fn revoke_identity_link(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let Some(link_data) = load_identity_link(&state.data_dir) else {
        // Already unlinked locally — nothing to do.
        return Ok(());
    };

    // Call revoke_link on the agent_linking zome
    let action_hash = ActionHash::try_from(link_data.entry_action_hash.clone())
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let payload = crate::hc::encode(&action_hash)?;
    call_zome(client, AGENT_LINKING_ZOME, "revoke_link", payload).await?;

    // Delete local persistence - INCLUDING the profile cache, or the next
    // link under a different identity could publish the previous person's
    // name/photo on their first public vote.
    delete_identity_link(&state.data_dir);
    flowsta_vault_client::identity::delete_cached_profile(&state.data_dir);

    // The Vault-side notification happens from the WEBVIEW (identity page),
    // not here: the Vault authorizes /revoke-identity by the caller's
    // Origin header, which a Rust reqwest doesn't carry - the old
    // Rust-side POST was silently 403'd on every unlink ever made.

    Ok(())
}

/// **DEPRECATED (v0.2.0+).** Replaced by the SDK's `dumpCellStateForBackup`
/// (which goes through `decode_record_for_export` below). This function is
/// kept only so the existing `startAutoBackup({ getData })` wiring in
/// `routes/layout.tsx` keeps working while we transition; once the new
/// `startAutoBackup({ adminWebsocket, cellIds, decodeRecordForExport })`
/// signature ships in production this function should be removed.
///
/// Export this user's ProofPoll data for Vault auto-backup.
/// Only includes the user's own data (CAL compliance):
///   - Polls they created (with all votes for context)
///   - Their votes on other people's polls
///   - Cryptographic keys to recreate their identity
#[tauri::command]
pub async fn get_export_data(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let my_key = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // Fetch all polls and votes from the active DHT
    let mut my_polls = Vec::new();
    let mut my_votes = Vec::new();
    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;

        let payload = crate::hc::encode(&())?;
        let result = call_zome(client, POLLS_ZOME, "get_all_polls", payload).await?;
        let records = crate::hc::records(client.line(), &result)?;

        for record in &records {
        let poll: Poll = decode_entry(record)?;
        let hash = record.action_hash.clone();
        let author = record.author.clone();
        let is_my_poll = author == my_key;

        // Fetch votes for this poll
        let vote_payload = crate::hc::encode(&crate::hc::action_hash(&record.action_hash)?)?;
        let vote_result = call_zome(client, POLLS_ZOME, "get_poll_votes", vote_payload).await;

        let all_votes: Vec<(u32, String)> = match vote_result {
            Ok(vr) => {
                let vote_records = crate::hc::records(client.line(), &vr).unwrap_or_default();
                vote_records
                    .iter()
                    .filter_map(|vr| {
                        let vote: Vote = decode_entry(vr).ok()?;
                        Some((vote.option_index, vr.author.clone()))
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        // If I created this poll, include it with all its votes
        if is_my_poll {
            let votes_json: Vec<serde_json::Value> = all_votes
                .iter()
                .map(|(idx, voter)| serde_json::json!({
                    "option_index": idx,
                    "voter": voter,
                }))
                .collect();

            my_polls.push(serde_json::json!({
                "hash": hash,
                "title": poll.title,
                "description": poll.description,
                "options": poll.options,
                "created_at": poll.created_at,
                "closes_at": poll.closes_at,
                "total_votes": votes_json.len(),
                "votes": votes_json,
            }));
        }

        // If I voted on this poll (whether mine or someone else's), record it
        for (option_index, voter) in &all_votes {
            if voter == &my_key {
                my_votes.push(serde_json::json!({
                    "poll_hash": hash,
                    "poll_title": poll.title,
                    "option_index": option_index,
                    "option_text": poll.options.get(*option_index as usize),
                }));
            }
        }
    }
    } // app_client lock released

    // Fetch encrypted private data (vote rationales + drafts)

    let agent_bytes = get_agent_ed25519_bytes(&state)?;
    let mut my_rationales = Vec::new();
    let mut my_drafts = Vec::new();

    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let lair = state.lair_client.lock().await;
        let lair = lair.as_ref().ok_or("Lair not connected")?;

        // Fetch vote rationales for each of my votes
        for vote_json in &my_votes {
            if let Some(poll_hash) = vote_json.get("poll_hash").and_then(|h| h.as_str()) {
                // Get all votes for this poll to find mine
                if let Ok(hash) = ActionHash::try_from(poll_hash.to_string()) {
                    let payload = crate::hc::encode(&hash)?;
                    if let Ok(result) = call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await {
                        let vote_records = crate::hc::records(client.line(), &result).unwrap_or_default();
                        for vr in &vote_records {
                            if vr.author.clone() == my_key {
                                let vote_hash = crate::hc::action_hash(&vr.action_hash)?;
                                let rat_payload = crate::hc::encode(&vote_hash)?;
                                if let Ok(rat_result) = call_zome(client, POLLS_ZOME, "get_vote_rationale", rat_payload).await {
                                    let rat_record = crate::hc::record_opt(client.line(), &rat_result).unwrap_or(None);
                                    if let Some(rec) = rat_record {
                                        if let Ok(ee) = decode_entry::<EncryptedEntryData>(&rec) {
                                            let mut nonce = [0u8; 24];
                                            if ee.nonce.len() == 24 {
                                                nonce.copy_from_slice(&ee.nonce);
                                                if let Ok(plaintext) = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await {
                                                    if let Ok(text) = String::from_utf8(plaintext) {
                                                        my_rationales.push(serde_json::json!({
                                                            "poll_title": vote_json.get("poll_title"),
                                                            "voted_for": vote_json.get("option_text"),
                                                            "rationale": text,
                                                        }));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fetch and decrypt draft polls
        let drafts_payload = crate::hc::encode(&())?;
        if let Ok(drafts_result) = call_zome(client, POLLS_ZOME, "get_my_drafts", drafts_payload).await {
            let draft_records = crate::hc::records(client.line(), &drafts_result).unwrap_or_default();
            for record in &draft_records {
                if let Ok(ee) = decode_entry::<EncryptedEntryData>(record) {
                    let mut nonce = [0u8; 24];
                    if ee.nonce.len() == 24 {
                        nonce.copy_from_slice(&ee.nonce);
                        if let Ok(plaintext) = crate::crypto::decrypt_from_self(lair, agent_bytes, nonce, &ee.cipher).await {
                            if let Ok(draft) = serde_json::from_slice::<serde_json::Value>(&plaintext) {
                                my_drafts.push(draft);
                            }
                        }
                    }
                }
            }
        }
    }

    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut export = serde_json::json!({
        "_readme": format!(
            "Your complete {} data export. Includes polls you created, \
             votes you cast, private vote rationales (decrypted), and draft polls (decrypted).",
            APP_NAME
        ),

        "format": {
            "version": 6,
            "exported_at": exported_at,
        },

        "you": {
            "agent_pub_key": my_key,
        },

        "polls_created": {
            "count": my_polls.len(),
            "polls": my_polls,
        },

        "votes_cast": {
            "count": my_votes.len(),
            "votes": my_votes,
        },

        "private_data": {
            "_readme": concat!(
                "Your private data, decrypted from encrypted entries on the DHT. ",
                "On the network, this data is stored as opaque ciphertext that only you can read. ",
                "This export includes the decrypted plaintext for your records.",
            ),

            "vote_rationales": {
                "count": my_rationales.len(),
                "rationales": my_rationales,
            },

            "draft_polls": {
                "count": my_drafts.len(),
                "drafts": my_drafts,
            },
        },
    });

    // Format v6: the export carries the means of authorship (CAL) - the
    // same app_keys block as the canonical backup. Both emitters carry it,
    // so either file restores the same authorship.
    match crate::device_seed::app_keys_json(&state.data_dir) {
        Ok(keys) => export["app_keys"] = keys,
        Err(e) => log::warn!("[export] could not read the device seed: {}", e),
    }

    Ok(export)
}

/// Core lookup: the agents the agent-linking zome reports as directly linked
/// to `agent` (an already-parsed key). Returns `AgentPubKey`s, NOT strings, so
/// callers can chain hops without round-tripping through the string form —
/// which matters because the Flowsta Vault's agent key fails holo_hash's strict
/// string parser (BadChecksum), even though its raw bytes are a valid key. The
/// zome hands keys back as valid bytes, so chaining from those always works.
/// Locks the app client itself; callers must NOT already hold that lock.
pub(crate) async fn linked_agent_keys(
    state: &std::sync::Arc<AppState>,
    agent: &AgentPubKey,
) -> Result<Vec<AgentPubKey>, String> {
    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;
    linked_agent_keys_with(client, agent).await
}

pub(crate) async fn linked_agent_keys_with(
    client: &crate::hc::AppClient,
    agent: &AgentPubKey,
) -> Result<Vec<AgentPubKey>, String> {
    let payload = crate::hc::encode(&agent.clone())?;
    let result = call_zome(client, AGENT_LINKING_ZOME, "get_linked_agents", payload).await?;
    crate::hc::decode(&result)
}

/// The networks an identity's links can be on. Links are per network: an
/// install that linked on v1.3 or v1.4 and never upgraded exists only there,
/// so while those lines run the identity's agent set is the UNION of every
/// answer. Without it, every vote cast by an older install of the same
/// person would stop reading as theirs the moment they upgraded.
pub(crate) const LINK_NETWORKS: [&str; 3] = ["active", "v1.4", "v1.3"];

/// The agents linked to `agent` on one network (on the active one, its own
/// links and those carried there); `None` when that line is not running.
pub(crate) async fn linked_on(
    state: &std::sync::Arc<AppState>,
    network: &str,
    agent: &AgentPubKey,
) -> Result<Option<Vec<AgentPubKey>>, String> {
    let slot = match network {
        "v1.4" => &state.app_client_v1_4,
        "v1.3" => &state.app_client_v1_3,
        _ => &state.app_client,
    };
    let client = slot.lock().await;
    let Some(c) = client.as_ref() else { return Ok(None) };
    let mut keys = linked_agent_keys_with(c, agent).await?;
    if network == "active" {
        let carried: Vec<AgentPubKey> = crate::hc::decode(&call_zome(c, POLLS_ZOME, "get_carried_links", crate::hc::encode(agent)?).await?)?;
        for k in carried {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    Ok(Some(keys))
}

/// The hubs (Vault identities) this agent is linked to on one network, or
/// nothing when that network is not running or not ready yet (logged, not
/// raised: the caller re-runs on its interval).
pub(crate) async fn hubs_on(
    state: &std::sync::Arc<AppState>,
    local: &AgentPubKey,
    network: &str,
) -> Vec<AgentPubKey> {
    match linked_on(state, network, local).await {
        Ok(h) => h.unwrap_or_default(),
        Err(e) => {
            log::warn!("[identity] get_my_agent_set ({}): local lookup failed: {}", network, e);
            Vec::new()
        }
    }
}

/// Every agent linked to `hub` on one network, folded into `set`. A hub is
/// a Vault identity key, the same on every network: a new install links on
/// the active network only, so its hub must be expanded on the older
/// networks too, or the identity's older installs (linked there before the
/// upgrade) never read as the same person on the new device (seen on a Mac
/// on 0.5.0-beta.3, 2026-10-25).
pub(crate) async fn fold_siblings_on(
    state: &std::sync::Arc<AppState>,
    hub: &AgentPubKey,
    network: &str,
    set: &mut Vec<AgentPubKey>,
) {
    match linked_on(state, network, hub).await {
        Ok(siblings) => {
            for s in siblings.unwrap_or_default() {
                if !set.contains(&s) {
                    set.push(s);
                }
            }
        }
        Err(e) => log::warn!("[identity] get_my_agent_set ({}): hub lookup failed: {}", network, e),
    }
}

/// Whether the active network (v1.5) links `agent_pub_key` to the identity
/// this install is signed in with: by a link made there, or by one carried
/// there. A carried link counts only for a key its original names (v1.5
/// files it under those two keys alone and validates that), so a link made
/// for the install's other key never stands in for this one. Without a
/// saved link to name the identity, any link of this key there counts. Err
/// while that network does not answer: the caller asks again later rather
/// than deciding from nothing. Older networks are not read: a link there
/// does not let v1.5 peers see who wrote what.
#[tauri::command]
pub async fn identity_linked_on_active(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    agent_pub_key: String,
) -> Result<bool, String> {
    let agent = AgentPubKey::try_from(agent_pub_key).map_err(|e| format!("Invalid agent key: {:?}", e))?;
    identity_linked_on_active_inner(state.inner(), &agent).await
}

pub(crate) async fn identity_linked_on_active_inner(
    state: &std::sync::Arc<AppState>,
    agent: &AgentPubKey,
) -> Result<bool, String> {
    let linked = linked_on(state, "active", agent).await?.ok_or("The active network is not running yet")?;
    let identity = load_identity_link(&state.data_dir).and_then(|l| parse_agent_pub_key_string(&l.vault_agent_pub_key).ok());
    Ok(links_this_identity(&linked, identity.as_ref()))
}

/// By the key itself: the saved identity key comes from the Vault as text,
/// and its location bytes need not match the network's copy.
pub(crate) fn links_this_identity(linked: &[AgentPubKey], identity: Option<&AgentPubKey>) -> bool {
    match identity {
        Some(identity) => linked.iter().any(|k| k.get_raw_32() == identity.get_raw_32()),
        None => !linked.is_empty(),
    }
}

#[tauri::command]
pub async fn get_linked_agents(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    agent_pub_key: String,
) -> Result<Vec<String>, String> {
    let agent = AgentPubKey::try_from(agent_pub_key.clone())
        .map_err(|e| format!("Invalid agent key: {:?}", e))?;
    let mut keys = linked_agent_keys(state.inner(), &agent).await?;
    for network in &LINK_NETWORKS[1..] {
        if let Ok(Some(older)) = linked_on(state.inner(), network, &agent).await {
            for k in older {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
        }
    }
    let out: Vec<String> = keys.iter().map(|a| a.to_string()).collect();
    // Debug, not info: the layout polls this every ~15s, so info would spam
    // the log. The user-facing identity set is logged (on change) by
    // get_my_agent_set instead.
    log::debug!(
        "[identity] get_linked_agents({}) -> {} agent(s): {:?}",
        agent_pub_key, out.len(), out,
    );
    Ok(out)
}

/// Last set logged by `get_my_agent_set`, so we log only when it changes
/// (the command runs ~every 30s; logging every call would spam the log).
pub(crate) static LAST_AGENT_SET: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// Every Holochain agent key that belongs to THIS user: the local conductor
/// agent plus every other ProofPoll agent linked to the same Flowsta Vault
/// identity (i.e. the user's other installs/devices). Used for RECOGNITION
/// only — "is this poll/vote/flag mine?" — never for mutation (Holochain only
/// lets the original author edit/delete, so those gates stay bound to the
/// current local agent).
///
/// This lives in Rust rather than being orchestrated from the webview so the
/// lookup is (a) a single robust round-trip instead of a fragile multi-call
/// frontend sequence that a poll-fetch failure could skip, and (b) fully
/// observable in proofpoll.log (webview console logs aren't readable on disk).
///
/// Resolved as a 2-hop walk through the link graph: local agent → the Vault
/// identity hub(s) it's linked to → every agent linked to that hub. We get the
/// hub key from the zome (valid bytes) rather than parsing the Vault key string
/// from identity-link.json, because that string fails holo_hash's checksum
/// parser. Sibling agents arrive via DHT gossip, so the set grows over the
/// first few minutes after launch — callers should re-run it.
#[tauri::command]
pub async fn get_my_agent_set(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    local_agent: Option<String>,
) -> Result<Vec<String>, String> {
    let local = match local_agent.as_ref().and_then(|s| AgentPubKey::try_from(s.clone()).ok()) {
        Some(k) => k,
        None => {
            log::warn!("[identity] get_my_agent_set: no valid local agent: {:?}", local_agent);
            return Ok(local_agent.into_iter().collect());
        }
    };

    let mut set: Vec<AgentPubKey> = vec![local.clone()];
    // The key the 0.7 line runs as, when the 0.6 one was conductor-born.
    let key_07 = state.agent_pub_key_07.lock().unwrap().clone();
    if let Some(k) = key_07.and_then(|k| AgentPubKey::try_from(k).ok()) {
        if !set.contains(&k) {
            set.push(k);
        }
    }

    // Hop 1: which identity hub(s) is this local agent linked to (the Vault).
    // Hop 2: every agent linked to that hub = all of the user's installs.
    // A network that is not ready contributes nothing this tick; the caller
    // re-runs on its interval, so the set self-heals as the networks come up.
    // The hubs are collected from BOTH networks and each is expanded on
    // BOTH: the hub key is the identity's, identical on every network, while
    // the links to it are per network.
    let mut hubs: Vec<AgentPubKey> = Vec::new();
    for own in set.clone() {
        for network in LINK_NETWORKS {
            for h in hubs_on(state.inner(), &own, network).await {
                if !hubs.contains(&h) {
                    hubs.push(h);
                }
            }
        }
    }
    for hub in &hubs {
        for network in LINK_NETWORKS {
            fold_siblings_on(state.inner(), hub, network, &mut set).await;
        }
    }

    let mut out: Vec<String> = set.iter().map(|a| a.to_string()).collect();
    out.sort(); // stable order so change-detection doesn't fire on reordering

    // Log only when the set CHANGES (first resolve, or a new linked agent
    // gossips in). Keeps the observability that diagnosed the BadChecksum bug
    // without logging every ~30s tick.
    {
        let mut last = LAST_AGENT_SET.lock().unwrap();
        if last.as_deref() != Some(out.as_slice()) {
            log::info!(
                "[identity] get_my_agent_set: local={:?} -> {} agent(s): {:?}",
                local_agent, out.len(), out,
            );
            *last = Some(out.clone());
        }
    }
    Ok(out)
}

// ── Migration status commands (infrastructure — keep as-is) ───────────
//
// These let the frontend show migration progress to the user.
// The actual migration logic is in migration.rs.

#[tauri::command]
pub async fn get_migration_status(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<crate::migration::MigrationState, String> {
    let state = state.migration_state.lock().await;
    Ok(state.clone())
}

// ── CAL-compliant backup helpers (v0.2.0+) ────────────────────────
//
// `decode_record_for_export` decodes an entry by type into plain JSON so the
// Flowsta Vault can embed a human-readable view of each record in the user's
// data export — what the Cryptographic Autonomy License (§4.2.1) obliges
// every Holochain app to provide.
//
// Adding a new entry type to ProofPoll means adding ONE `match` arm in
// `decode_record_for_export` and one in `build_canonical_backup`'s record loop,
// mirroring the entry type's existing structs. There is no restore command —
// see the note below on why recovery is recognition, not replay.

use base64::Engine as _;

/// Decode an entry's MessagePack bytes into a human-readable JSON view, used
/// when the SDK builds a backup for Vault. Each arm uses the existing local
/// struct (Poll / Vote) so all fields stay in sync with the DNA via serde.
#[tauri::command]
pub async fn decode_record_for_export(
    entry_type: String,
    entry_bytes_b64: String,
) -> Result<serde_json::Value, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&entry_bytes_b64)
        .map_err(|e| format!("base64 decode: {}", e))?;

    match entry_type.as_str() {
        "Poll" => {
            let poll: Poll = rmp_serde::from_slice(&bytes)
                .map_err(|e| format!("Poll decode: {}", e))?;
            serde_json::to_value(poll).map_err(|e| e.to_string())
        }
        "Vote" => {
            let vote: Vote = rmp_serde::from_slice(&bytes)
                .map_err(|e| format!("Vote decode: {}", e))?;
            serde_json::to_value(vote).map_err(|e| e.to_string())
        }
        other => Ok(serde_json::json!({
            "_warning": format!("Unknown entry type: {}", other),
            "raw_bytes_hex": hex::encode(&bytes),
        })),
    }
}

// Note: there is no restore command. The old `restore_record` dispatcher — and a
// later source-chain graft attempt — were both removed. Recovery is now
// identity-aware recognition: a fresh install reads the user's existing DHT data
// via their linked agent set (`get_my_agent_set`, a 2-hop walk of the identity
// link graph), so nothing is re-created or duplicated. The backup feeds the CAL
// §4.2.1 data export only.

/// Build the user's canonical-shape backup payload for Vault.
///
/// Replaces `get_export_data` for the Vault auto-backup path. Produces the
/// same payload shape the SDK's `dumpCellStateForBackup` would produce, so
/// the Flowsta Vault recognises it as canonical and:
///   - persists a per-entry-type summary alongside the backup metadata
///     (rendered as "12 polls, 38 votes" on the Your Data and overview UIs),
///   - inlines the `human_readable` view of each record into the user's CAL
///     §4.2.1 data export (full or per-app), and
///   - leaves the signed `raw_record` inside the encrypted backup.
///
/// Architecture note: this captures the user's ENTIRE source chain via the
/// admin `dump_full_state`, not just Poll/Vote app entries — so the CAL export
/// is the user's complete, signed chain. Each `raw_record` is a verbatim
/// `SourceChainDumpRecord` (a fully portable signed record).
///
/// On top of the raw records we layer the human-readable view + per-type
/// counts for the app entries we can decode (Poll, Vote) — this is what Vault
/// renders on the Your Data page and inlines into the CAL §4.2.1 export.
/// Infrastructure records (Dna, AgentValidationPkg, CapGrant, …) carry their
/// raw_record but no human_readable (they're chain plumbing, not user data).
///
/// The backup carries data only - never the lair keystore or passphrase
/// (see the note at the end of this function).
/// One Vault backup attempt, as the Identity page and proofpoll.log see it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct BackupOutcome {
    pub ok: bool,
    /// Plain words: "saved" or why it was skipped (the SDK's own reason).
    pub message: String,
    pub bytes: Option<u64>,
    /// Unix seconds.
    pub at: u64,
}

/// The webview reports every backup attempt here (the hourly one and the
/// write-triggered one), so the reason a backup did not happen reaches
/// proofpoll.log and the Identity page instead of only the devtools console.
#[tauri::command]
pub fn report_backup_outcome(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    ok: bool,
    message: String,
    bytes: Option<u64>,
) -> BackupOutcome {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if ok {
        log::info!("[backup] saved to the Vault: {} bytes", bytes.unwrap_or(0));
    } else {
        log::warn!("[backup] not saved: {}", message);
    }
    let outcome = BackupOutcome { ok, message, bytes, at };
    *state.last_backup_outcome.lock().unwrap() = Some(outcome.clone());
    outcome
}

#[tauri::command]
pub fn get_last_backup_outcome(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Option<BackupOutcome> {
    state.last_backup_outcome.lock().unwrap().clone()
}

#[tauri::command]
pub async fn build_canonical_backup(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    client_id: Option<String>,
) -> Result<serde_json::Value, String> {
    use holochain_client_07::{AdminWebsocket, CellInfo};

    // This payload is written into the linked Vault's backup slot. Same
    // gate as the DHT writes: if the Vault present right now belongs to a
    // different identity, this data must not land in their slot.
    require_identity_link(&state).await?;

    // Escrow gate (fail closed): refuse to build a payload that would
    // replace a Vault backup escrowing a DIFFERENT seed than this device's -
    // on a fresh install after machine death, that overwrite would destroy
    // the very seed the user needs to adopt. The frontend passes the app's
    // client_id (it lives in the webview env); without it the escrow cannot
    // be checked, so no payload is built.
    let client_id = client_id.ok_or(
        "backup held: no client_id supplied, so the Vault's escrowed key cannot be checked \
         first - pass clientId when invoking build_canonical_backup",
    )?;
    let probe = crate::seed_adopt::escrow_probe(client_id);
    crate::seed_adopt::backup_escrow_gate(&probe, crate::device_seed::load(&state.data_dir))
        .await?;

    let my_key = {
        let key = state.agent_pub_key.lock().unwrap();
        key.clone().ok_or("Agent key not available")?
    };

    // 1. Connect to the admin websocket (same pattern as try_reenable_app).
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", crate::conductor::admin_port_07()),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WS for backup: {}", e))?;

    // 2. Find the active (v1.4) provisioned cell, and v1.5's when installed.
    let apps = admin_ws
        .list_apps(None)
        .await
        .map_err(|e| format!("list_apps: {}", e))?;
    let cell_of = |app_id: &str| {
        apps.iter().find(|a| a.installed_app_id == app_id).and_then(|app| {
            app.cell_info
                .values()
                .flat_map(|cells| cells.iter())
                .find_map(|c| match c {
                    CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                    _ => None,
                })
        })
    };
    let cell_id = cell_of(crate::dna::APP_ID_V1_4).ok_or("The v1.4 cell was not found for the backup")?;
    let cell_id_15 = cell_of(crate::dna::APP_ID_V1_5);

    // 3. Dump the full source chain (every action authored by this agent).
    let dump = admin_ws
        .dump_full_state(cell_id, None, None)
        .await
        .map_err(|e| format!("dump_full_state: {}", e))?;

    // 4. Build canonical records + per-type counts.
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    // Drafts and reasons on v1.4, by ciphertext: the move to v1.5 writes
    // them again there, and each counts once.
    let mut private_14: std::collections::HashSet<(Vec<u8>, Vec<u8>)> = std::collections::HashSet::new();
    for rec in &dump.source_chain_dump.records {
        let action_hash = rec.action_address.to_string();
        let created_ms = rec.action.timestamp().as_millis();

        let (entry_type_name, human_readable) = classify_dump_record(rec);
        if let Some(p) = private_of(rec) {
            private_14.insert(p);
        }

        // Count only the decodable user-data entry types (those with a
        // non-null human_readable view). Infrastructure records keep their
        // signed `raw_record` so the export is a complete copy of the chain,
        // but they don't inflate the user-facing summary.
        if !human_readable.is_null() {
            *counts.entry(entry_type_name.clone()).or_insert(0) += 1;
        }

        let raw_record = serde_json::to_value(rec)
            .map_err(|e| format!("serialize SourceChainDumpRecord: {}", e))?;

        records.push(serde_json::json!({
            "entryType": entry_type_name,
            "actionHash": action_hash,
            "createdAtMs": created_ms,
            "human_readable": human_readable,
            "raw_record": raw_record,
        }));
    }

    // 4b. The v1.3 chain on the 0.6 line, while that line runs. It holds
    // what was authored there and never copied to v1.4 (a vote on a poll
    // still on the 0.6 network, a closed poll that stayed there); without
    // it the Vault's counts and the data export left all of that out (a
    // Mac, 0.6.0-beta.1). Every record goes in the export; the summary
    // counts only those the migration did not copy, so none counts twice.
    let cell_06 = match dump_chain_06(&state).await {
        Ok(Some(chain)) => {
            let copied = Copied06::from(&*state.migration_state.lock().await);
            let mut records_06: Vec<serde_json::Value> = Vec::new();
            let mut copied_hashes: Vec<String> = Vec::new();
            for rec in &chain.records {
                let action_hash = rec.action_address.to_string();
                let (entry_type_name, human_readable) = classify_dump_record_06(rec);
                let vote_poll = (entry_type_name == "Vote")
                    .then(|| app_entry_bytes_06(rec).and_then(|b| rmp_serde::from_slice::<Vote>(b).ok()))
                    .flatten()
                    .map(|v| v.poll_action_hash.to_string());
                if copied.holds(&entry_type_name, &action_hash, vote_poll.as_deref()) {
                    copied_hashes.push(action_hash.clone());
                } else if !human_readable.is_null() {
                    *counts.entry(entry_type_name.clone()).or_insert(0) += 1;
                }
                let raw_record = serde_json::to_value(rec)
                    .map_err(|e| format!("serialize SourceChainDumpRecord (0.6): {}", e))?;
                records_06.push(serde_json::json!({
                    "entryType": entry_type_name,
                    "actionHash": action_hash,
                    "createdAtMs": rec.action.timestamp().as_millis(),
                    "human_readable": human_readable,
                    "raw_record": raw_record,
                }));
            }
            Some(serde_json::json!({
                "role_name": "polls",
                "line": "holochain-0.6",
                "dna_version": "1.3",
                "_readme": "The same, from the previous Holochain line (0.6): what you authored on ProofPoll before it moved to Holochain 0.7, and since then on polls still on the 0.6 network. The records listed in `copied_to_07` were copied to the 0.7 chain when this install moved, so they appear in both chains but are counted once.",
                "copied_to_07": copied_hashes,
                "records": records_06,
            }))
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("[backup] the 0.6 chain was not read, so this backup carries the 0.7 chain only: {}", e);
            None
        }
    };

    // 4c. The v1.5 chain, once installed. It must be in the backup: a key
    // whose v1.5 chain exists (from its genesis on) restores it on a new
    // device instead of starting a second one (seed_adopt::backup_chain), so
    // a backup that cannot list it is not made. Its carried copies are not
    // counted (their originals are, above), nor drafts and reasons written
    // again from v1.4; what was written there first is.
    let cell_15 = match cell_id_15 {
        Some(cell_id_15) => {
            let dump_15 = admin_ws
                .dump_full_state(cell_id_15, None, None)
                .await
                .map_err(|e| format!("dump_full_state (v1.5): {}", e))?;
            let mut records_15: Vec<serde_json::Value> = Vec::new();
            for rec in &dump_15.source_chain_dump.records {
                let (entry_type_name, human_readable) = classify_dump_record(rec);
                // Only polls, votes, flags, drafts and reasons count (a
                // mapping or a carried link is the move's own bookkeeping).
                let kind = match rec.action.entry_type() {
                    Some(holochain_integrity_types_07::action::EntryType::App(def)) => Some((def.zome_index.0, def.entry_index.0)),
                    _ => None,
                };
                let again = match kind {
                    Some((1, 4)) => private_of(rec).is_some_and(|p| private_14.contains(&p)),
                    Some((1, 0 | 1 | 2)) => is_carried(rec),
                    _ => true,
                };
                if !human_readable.is_null() && !again {
                    *counts.entry(entry_type_name).or_insert(0) += 1;
                }
                let raw_record = serde_json::to_value(rec)
                    .map_err(|e| format!("serialize SourceChainDumpRecord (v1.5): {}", e))?;
                records_15.push(serde_json::json!({
                    "actionHash": rec.action_address.to_string(),
                    "createdAtMs": rec.action.timestamp().as_millis(),
                    "raw_record": raw_record,
                }));
            }
            Some(serde_json::json!({
                "role_name": "polls",
                "line": "holochain-0.7",
                "dna_version": "1.5",
                "_readme": "Your chain on ProofPoll's newer network on the same Holochain line: what you wrote there, and copies of your polls and votes that carry the original signed record (those are counted once, in the chain above).",
                "records": records_15,
            }))
        }
        None => None,
    };

    let total_records: usize = counts.values().sum();
    let counts_json: serde_json::Map<String, serde_json::Value> = counts
        .into_iter()
        .map(|(k, v)| (k, serde_json::json!(v)))
        .collect();

    let mut cells = vec![serde_json::json!({
        "role_name": "polls",
        // Which chain this is: a restore onto another device rebuilds
        // a 0.7 chain from the network (seed_adopt::backup_chain).
        "line": "holochain-0.7",
        "dna_version": "1.4",
        "_readme": "Each record below is one action on your source chain. `human_readable` is the plain-English view (app entries only). `raw_record` is the signed record used to restore your chain onto a fresh install.",
        "records": records,
    })];
    cells.extend(cell_15);
    cells.extend(cell_06);

    // 5. Assemble the canonical payload.
    let payload = serde_json::json!({
        "version": 1,
        "_readme": format!(
            "Your {} data, backed up automatically by Flowsta Vault. Encrypted with your device key — only you can read it. Each record below carries a plain-English view of what you authored AND a signed Holochain record. The app_keys block holds your agent seed, so a restore continues as the SAME author.",
            APP_NAME
        ),
        "license": "Cryptographic Autonomy License v1.0 (CAL-1.0)",
        "app": {
            "name": APP_NAME,
        },
        "agent_pub_key": my_key,
        "exported_at_iso": format!("unix:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ),
        "_summary": {
            "countsByEntryType": counts_json,
            "totalRecords": total_records,
        },
        "cells": cells,
    });
    let mut payload = payload;
    // The means of authorship travels WITH the data (CAL): the agent seed
    // this device authors under. Encrypted at rest in the Vault, readable in
    // the user's own export - that is the point. Installs whose agent
    // predates the app-held seed have nothing to escrow yet and say so.
    // An unreadable recovery file only reaches this point when nothing
    // key-shaped is escrowed yet (the escrow gate above refuses otherwise),
    // so omitting app_keys here can never shadow an escrowed seed.
    match crate::device_seed::app_keys_json(&state.data_dir) {
        Ok(keys) => payload["app_keys"] = keys,
        Err(e) => log::warn!("[backup] could not read the device seed: {}", e),
    }

    // The backup carries the AGENT SEED and nothing else key-shaped: never
    // the lair passphrase or keystore again (0.2.2 shipped both, hourly, for
    // a restore flow that did not exist). The seed is the targeted escrow
    // that makes an export CAL-whole and lets a restore resume the same
    // author, without handing over the whole keystore.

    Ok(payload)
}

/// Classify a dumped source-chain record into a (entryType, human_readable)
/// pair for the canonical backup. App entries we can decode get a type name +
/// plain-JSON view; everything else gets an action-type label and a null
/// human_readable (its signed `raw_record` stays in the backup for a complete,
/// verifiable export, but it is not shown as user data).
///
/// For forking developers: the `(zome_index, entry_index)` table below mirrors
/// your DNA manifest's integrity-zome order and each integrity zome's
/// `EntryTypes` enum order. Update it alongside your entry types — it is the
/// one place that maps on-chain entry indices back to readable names.
///
/// ProofPoll v1.3 (`dna/v1.3/workdir/dna.yaml`):
///   integrity zomes: [0] agent_linking_integrity, [1] polls_integrity
///   polls_integrity EntryTypes: [0] Poll [1] Vote [2] Flag [3] MigratedPoll [4] EncryptedEntry
/// A draft's or reason's (cipher, nonce), for a v1.4 or v1.5 record.
pub(crate) fn private_of(rec: &holochain_state_types_07::SourceChainDumpRecord) -> Option<(Vec<u8>, Vec<u8>)> {
    use holochain_integrity_types_07::action::EntryType;
    #[derive(serde::Deserialize)]
    struct Private {
        cipher: Vec<u8>,
        nonce: Vec<u8>,
    }
    let Some(EntryType::App(def)) = rec.action.entry_type() else { return None };
    if (def.zome_index.0, def.entry_index.0) != (1, 4) {
        return None;
    }
    let bytes = rec.entry.as_ref().and_then(|e| e.as_app_entry())?.as_ref().bytes().clone();
    rmp_serde::from_slice::<Private>(&bytes).ok().map(|p| (p.cipher, p.nonce))
}

/// A v1.5 poll, vote or flag carried from an older line.
pub(crate) fn is_carried(rec: &holochain_state_types_07::SourceChainDumpRecord) -> bool {
    #[derive(serde::Deserialize)]
    struct Carried {
        #[serde(default)]
        carried_from: Option<serde::de::IgnoredAny>,
    }
    rec.entry
        .as_ref()
        .and_then(|e| e.as_app_entry())
        .and_then(|a| rmp_serde::from_slice::<Carried>(a.as_ref().bytes()).ok())
        .is_some_and(|c| c.carried_from.is_some())
}

pub(crate) fn classify_dump_record(
    rec: &holochain_state_types_07::SourceChainDumpRecord,
) -> (String, serde_json::Value) {
    use holochain_integrity_types_07::action::EntryType;

    let entry_type = match rec.action.entry_type() {
        Some(et) => et,
        // No entry reference — a system action (Dna, AgentValidationPkg,
        // InitZomesComplete, OpenChain, CloseChain, …). Label by the action
        // variant for diagnostics; not user data.
        None => return (action_variant_label(&rec.action), serde_json::Value::Null),
    };

    let app_def = match entry_type {
        EntryType::App(def) => def,
        EntryType::AgentPubKey => return ("AgentPubKey".to_string(), serde_json::Value::Null),
        EntryType::CapClaim => return ("CapClaim".to_string(), serde_json::Value::Null),
        EntryType::CapGrant => return ("CapGrant".to_string(), serde_json::Value::Null),
    };

    // Pull the raw msgpack entry bytes (if the entry is present in the dump).
    let entry_bytes: Option<&[u8]> = rec
        .entry
        .as_ref()
        .and_then(|e| e.as_app_entry())
        .map(|app_bytes| app_bytes.as_ref().bytes().as_slice());

    classify_app_entry(app_def.zome_index.0, app_def.entry_index.0, entry_bytes)
}

/// The `(zome_index, entry_index)` table of [`classify_dump_record`], the
/// same on both lines (v1.4 keeps v1.3's zome and entry order).
pub(crate) fn classify_app_entry(zome: u8, entry_idx: u8, entry_bytes: Option<&[u8]>) -> (String, serde_json::Value) {
    match (zome, entry_idx) {
        // polls_integrity (zome index 1)
        (1, 0) => decode_named::<Poll>("Poll", entry_bytes),
        (1, 1) => decode_named::<Vote>("Vote", entry_bytes),
        // Flag / MigratedPoll: kept as signed raw_record for a complete
        // export, but no human_readable view (commands.rs has no plain mirror
        // struct for them). Not counted in the user-facing summary.
        (1, 2) => ("Flag".to_string(), serde_json::Value::Null),
        (1, 3) => ("MigratedPoll".to_string(), serde_json::Value::Null),
        // EncryptedEntry holds ciphertext — no plaintext human_readable view.
        (1, 4) => ("EncryptedEntry".to_string(), serde_json::Value::Null),
        // agent_linking_integrity (zome index 0)
        (0, 0) => ("IsSamePerson".to_string(), serde_json::Value::Null),
        _ => (
            format!("AppEntry(zome={},entry={})", zome, entry_idx),
            serde_json::Value::Null,
        ),
    }
}

/// Decode entry bytes into a named type for the human_readable view. Returns
/// (name, json) on success, or (name, null) if bytes are absent or undecodable
/// — the record is still kept (with its signed raw_record) so the export stays
/// a complete copy of the chain.
pub(crate) fn decode_named<T: serde::de::DeserializeOwned + serde::Serialize>(
    name: &str,
    entry_bytes: Option<&[u8]>,
) -> (String, serde_json::Value) {
    let human = entry_bytes
        .and_then(|b| rmp_serde::from_slice::<T>(b).ok())
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(serde_json::Value::Null);
    (name.to_string(), human)
}

/// Best-effort label for a system (entry-less) action, used only for
/// diagnostics in the backup — never shown as user data.
pub(crate) fn action_variant_label(action: &holochain_integrity_types_07::action::Action) -> String {
    // The 0.7 action model: one Action struct, its kind named by ActionType.
    format!("{:?}", action.action_type())
}

/// The v1.3 source chain on the 0.6 conductor, for the backup. None when the
/// 0.6 line is not running (post-window builds) or v1.3 is not installed.
pub(crate) async fn dump_chain_06(state: &AppState) -> Result<Option<holochain_state_types::SourceChainDump>, String> {
    use holochain_client::{AdminWebsocket, CellInfo};
    if state.app_client_v1_3.lock().await.is_none() {
        return Ok(None);
    }
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", crate::conductor::admin_port()),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("admin WS: {}", e))?;
    let apps = admin_ws.list_apps(None).await.map_err(|e| format!("list_apps: {}", e))?;
    let Some(cell_id) = apps
        .iter()
        .find(|a| a.installed_app_id == crate::dna::ACTIVE_APP_ID_06)
        .and_then(|app| {
            app.cell_info.values().flat_map(|cells| cells.iter()).find_map(|c| match c {
                CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                _ => None,
            })
        })
    else {
        return Ok(None);
    };
    let dump = admin_ws
        .dump_full_state(cell_id, None)
        .await
        .map_err(|e| format!("dump_full_state: {}", e))?;
    Ok(Some(dump.source_chain_dump))
}

/// [`classify_dump_record`] for a v1.3 record (the 0.6 action model).
pub(crate) fn classify_dump_record_06(rec: &holochain_state_types::SourceChainDumpRecord) -> (String, serde_json::Value) {
    use holochain_integrity_types::EntryType;
    let app_def = match rec.action.entry_type() {
        Some(EntryType::App(def)) => def,
        Some(EntryType::AgentPubKey) => return ("AgentPubKey".to_string(), serde_json::Value::Null),
        Some(EntryType::CapClaim) => return ("CapClaim".to_string(), serde_json::Value::Null),
        Some(EntryType::CapGrant) => return ("CapGrant".to_string(), serde_json::Value::Null),
        None => return (format!("{:?}", rec.action.action_type()), serde_json::Value::Null),
    };
    classify_app_entry(app_def.zome_index.0, app_def.entry_index.0, app_entry_bytes_06(rec))
}

pub(crate) fn app_entry_bytes_06(rec: &holochain_state_types::SourceChainDumpRecord) -> Option<&[u8]> {
    rec.entry
        .as_ref()
        .and_then(|e| e.as_app_entry())
        .map(|app_bytes| app_bytes.as_ref().bytes().as_slice())
}

/// What the migration copied from v1.3 to v1.4: the polls (by their v1.3
/// hash), the polls whose votes it cast again on v1.4, and the drafts and
/// vote reasons. Such a record is on both chains and the backup's summary
/// counts it on the 0.7 one.
pub(crate) struct Copied06 {
    pub(crate) polls: std::collections::HashSet<String>,
    pub(crate) voted_polls: std::collections::HashSet<String>,
    pub(crate) private: std::collections::HashSet<String>,
}

impl Copied06 {
    fn from(state: &crate::migration::MigrationState) -> Self {
        Copied06 {
            polls: state.polls_migrated.iter().map(|p| p.old_hash.clone()).collect(),
            voted_polls: state.votes_migrated.iter().map(|v| v.old_poll_hash.clone()).collect(),
            private: state.private_copied.iter().cloned().collect(),
        }
    }

    fn holds(&self, entry_type: &str, action_hash: &str, vote_poll: Option<&str>) -> bool {
        match entry_type {
            "Poll" => self.polls.contains(action_hash),
            "Vote" => vote_poll.is_some_and(|p| self.voted_polls.contains(p)),
            "EncryptedEntry" => self.private.contains(action_hash),
            _ => false,
        }
    }
}

#[cfg(test)]
mod backup_tests {
    use super::Copied06;
    use crate::migration::{MigratedPollRecord, MigratedVoteRecord, MigrationState};

    const OWN_POLL_06: &str = "uhCkkn8fJ8VnqKtA2LtNyMfwW0mGfLtqH6b2yyqFMSZr7aN9Q_Cfa";
    const OWN_POLL_07: &str = "uhCkkpz3Wd4Ko5dTH5Kq4pKm4gM-oq6i0W0b1rQyPBwXc4vO3tZl6";
    const VOTED_POLL_06: &str = "uhCkkxS1Z1kEw2u9fGhB4sQ0mVYdL3tKzHqA8pRc7nWj5yUe6oDiT";
    const OTHER_POLL_06: &str = "uhCkk0b7cR2mQ8nJw4kXz9tLp3sVfE6hYqG1aUd5oNiT2rKyWxMcB";
    const DRAFT_06: &str = "uhCkkT3vJq9Lw0sXn4mB7cYz2kR5pG8hUd1aEoNiW6rKyQxMfVtC";

    #[test]
    fn a_v13_record_the_migration_copied_counts_once() {
        let mut state = MigrationState::default();
        state.polls_migrated.push(MigratedPollRecord { old_hash: OWN_POLL_06.into(), new_hash: OWN_POLL_07.into(), title: "Lunch".into() });
        state.votes_migrated.push(MigratedVoteRecord { old_poll_hash: VOTED_POLL_06.into(), new_poll_hash: OWN_POLL_07.into(), option_index: 1 });
        let copied = Copied06::from(&state);
        // Copied to v1.4: counted on the 0.7 chain only.
        assert!(copied.holds("Poll", OWN_POLL_06, None));
        assert!(copied.holds("Vote", "uhCkkVoteActionHashIsNotTheKeyHereQ8nJw4kXz9tLp3sVfE6", Some(VOTED_POLL_06)));
        // Authored on v1.3 and never copied (a poll created there after the
        // move, a vote on a poll still on the 0.6 network): counted here.
        assert!(!copied.holds("Poll", OTHER_POLL_06, None));
        assert!(!copied.holds("Vote", "uhCkkVoteActionHashIsNotTheKeyHereQ8nJw4kXz9tLp3sVfE6", Some(OTHER_POLL_06)));
        assert!(!copied.holds("Vote", OWN_POLL_06, None));
        // A draft or vote reason: copied once `carry_private` recorded it.
        assert!(!copied.holds("EncryptedEntry", DRAFT_06, None));
        state.private_copied.push(DRAFT_06.into());
        assert!(Copied06::from(&state).holds("EncryptedEntry", DRAFT_06, None));
    }
}

#[cfg(test)]
mod active_link_tests {
    use super::links_this_identity;
    use holochain_types::prelude::AgentPubKey;

    #[test]
    fn the_active_link_counts_for_the_signed_in_identity_only() {
        let identity = AgentPubKey::from_raw_32(vec![7; 32]);
        let other_identity = AgentPubKey::from_raw_32(vec![8; 32]);
        assert!(links_this_identity(&[identity.clone()], Some(&identity)));
        assert!(links_this_identity(&[other_identity.clone(), identity.clone()], Some(&identity)));
        assert!(!links_this_identity(&[other_identity.clone()], Some(&identity)), "a link to an identity this install left does not count");
        assert!(!links_this_identity(&[], Some(&identity)));

        // The saved key arrives as text from the Vault: compared by the key
        // itself, whatever its location bytes say.
        let mut raw = identity.get_raw_39().to_vec();
        raw[35..39].copy_from_slice(&[0, 0, 0, 0]);
        let as_saved = AgentPubKey::from_raw_39(raw);
        assert!(links_this_identity(&[identity.clone()], Some(&as_saved)));

        // No saved link to name the identity: any link of the key counts.
        assert!(links_this_identity(&[other_identity], None));
        assert!(!links_this_identity(&[], None));
    }
}

/// The start screen's "Continue without them": a restore that came back
/// shorter than the backup is enabled as it is, without the old device's
/// last records (`dna::wait_for_restore_07`).
#[tauri::command]
pub fn restore_go_on() {
    log::info!("[restore] the person chose to go on without the records the network does not hold");
    crate::dna::RESTORE_ACCEPT_SHORT.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The start screen's "Try again": ask the network once more for the chain
/// a short restore was set aside for (`conductor.rs`).
#[tauri::command]
pub fn restore_try_again() -> bool {
    let taken = crate::dna::ask_to_try_again();
    log::info!("[restore] the person asked to try again{}", if taken { "" } else { ": not yet (the other device needs a few minutes first)" });
    taken
}

#[cfg(test)]
mod quiet_tests {
    use super::{only_reads, quiet_at};

    /// The memory restart waits for a quiet moment. The pages read on their
    /// own timers, so reads must not hold it off; a write does, for a
    /// minute, and so does any call still waiting for its answer.
    #[test]
    fn a_quiet_moment_ignores_the_pages_own_reads() {
        for read in ["get_all_polls", "get_all_polls_local", "get_linked_agents", "get_poll_flags", "get_my_drafts", "get_all_migration_mappings"] {
            assert!(only_reads(read), "{}", read);
        }
        for write in ["create_poll", "cast_vote", "flag_poll", "remove_flag", "delete_poll", "create_encrypted_entry", "create_external_link", "register_migrated_poll", "revoke_link"] {
            assert!(!only_reads(write), "{}", write);
        }
        let now = 1_794_600_000;
        assert!(quiet_at(now, 0, 0), "nothing ever written");
        assert!(quiet_at(now, now - 60, 0));
        assert!(!quiet_at(now, now - 59, 0), "a write less than a minute ago");
        assert!(!quiet_at(now, now - 3600, 1), "a call is waiting for its answer");
        assert!(quiet_at(now, now + 5, 0) == false, "a clock that stepped back is not quiet");
    }
}
