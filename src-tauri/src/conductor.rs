//! Holochain conductor lifecycle management for ProofPoll.
//!
//! Runs TWO Holochain lines as child processes (the transition release:
//! Holochain 0.6.1 for DNA v1.0-v1.3, Holochain 0.7.1 for v1.4 and v1.5),
//! each with a key store of its own version, installs the DNAs, and sets up
//! WebSocket connections for zome calls on each line.
//!
//! The processes themselves (the key stores, the conductors, their configs,
//! their clean stop, which line runs and whether it still does) are
//! `flowsta-hc-lines`: its `Supervisor` owns them, and this file tells it
//! what a ProofPoll line needs (`PpLines`, the library's `LineHooks`).
//!
//! ## For forking developers
//!
//! This file is reusable infrastructure. The only things you might change:
//!   - `admin_port()` (4466, the 0.6 line) and `admin_port_07()` (4467) — change
//!     if running alongside other Holochain apps
//!   - Bootstrap / signal / relay URLs and the optional auth material — set
//!     via compile-time env vars (see "Bootstrap configuration" below).
//!     Defaults connect to the Holochain ecosystem's public dev bootstrap,
//!     so a fresh fork builds and runs without any Flowsta dependency.
//!   - The startup sequence calls `install_dnas()` from `dna.rs` — that's
//!     where your app-specific hApp bundle names are configured

use crate::hc::Line;
use crate::lair;
use crate::sidecar::sidecar_path;
use flowsta_hc_lines::conductor::NetworkTuning;
use flowsta_hc_lines::keystore::SocketPlan;
use flowsta_hc_lines::rendezvous::Rendezvous;
use flowsta_hc_lines::supervisor::{self, LineHooks, LineProcess, LineStart, Role, Setup, StartStep, Supervisor, Timeouts};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use tauri::Emitter;

/// Admin WebSocket port for the local Holochain conductor.
/// Different from Flowsta Vault's 4455 so both can run simultaneously.
/// Change this if running alongside other Holochain apps.
const ADMIN_WS_PORT_BASE: u16 = 4466;
/// Admin websocket of the Holochain 0.7.1 conductor (DNA v1.4+).
const ADMIN_WS_PORT_07_BASE: u16 = 4467;

/// The 0.6.1 conductor's admin port.
pub fn admin_port() -> u16 {
    ADMIN_WS_PORT_BASE + test_port_offset()
}

/// The 0.7.1 conductor's admin port.
pub fn admin_port_07() -> u16 {
    ADMIN_WS_PORT_07_BASE + test_port_offset()
}

/// A debug build adds `PROOFPOLL_TEST_PORT_OFFSET` to both admin ports, so
/// test instances run beside each other and beside an installed ProofPoll.
/// A release build never reads it.
fn test_port_offset() -> u16 {
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("PROOFPOLL_TEST_PORT_OFFSET").ok().and_then(|v| v.trim().parse::<u16>().ok()) {
        return n;
    }
    0
}
/// Data directory name of the 0.7.1 conductor (the 0.6.1 one keeps `conductor`).
pub const CONDUCTOR_DIR_07: &str = "conductor-07";

/// Key store directory of the 0.7.1 line (the 0.6.1 one keeps `lair`).
pub const LAIR_DIR_07: &str = "lair-07";
/// Every directory that belongs to a line, both lines. A wipe, move or
/// set-aside names all four: a reset that left `lair-07/` behind kept it
/// encrypted under the old passphrase, and the 0.7 line failed to start.
pub const LINE_DIRS: &[&str] = &["conductor", "lair", CONDUCTOR_DIR_07, LAIR_DIR_07];

// ── Bootstrap configuration ────────────────────────────────────────
//
// Read at *compile time* from env vars (set in CI for the official
// release; unset for fork developers, who get the public Holochain dev
// bootstrap defaults). To override at build time:
//
//   PROOFPOLL_BOOTSTRAP_URL=https://your-bootstrap.example.com   \
//   PROOFPOLL_SIGNAL_URL=wss://your-bootstrap.example.com        \
//   PROOFPOLL_RELAY_URL=https://your-bootstrap.example.com./     \
//   PROOFPOLL_AUTH_MATERIAL=<standard base64 of opaque auth bytes>     \
//     cargo tauri build
//
// `PROOFPOLL_AUTH_MATERIAL` is optional and only set when targeting a
// bootstrap that requires authentication (e.g. bootstrap.flowsta.com
// when running with `--authentication-hook-server`). The same value
// is written into both `base64_auth_material_bootstrap` and
// `base64_auth_material_relay` in the conductor config — bootstrap
// and relay are independent auth flows in kitsune2 even when one URL
// terminates both. It is sent verbatim by kitsune2_bootstrap_client
// to `/authenticate`; the returned token is then used on subsequent
// connections automatically.
//
// IMPORTANT: encoding is `base64::engine::general_purpose::STANDARD`
// (standard alphabet `+/`, padding REQUIRED with `=`). NOT
// `URL_SAFE_NO_PAD` (the Holochain docstring claims that but the
// decoder is wrong about itself — see BOOTSTRAP_AUTH_PLAN.md).
// Encode with `base64 -w0` in shell, or `STANDARD.encode(bytes)` in
// Rust, or `Buffer.from(s).toString('base64')` in Node.

/// Default bootstrap URL — the Holochain ecosystem's public dev server.
/// Override with `PROOFPOLL_BOOTSTRAP_URL` for production.
const DEFAULT_BOOTSTRAP_URL: &str = "https://dev-test-bootstrap2.holochain.org";

/// Default signal URL — same host as the dev bootstrap.
const DEFAULT_SIGNAL_URL: &str = "wss://dev-test-bootstrap2.holochain.org";

/// Default Iroh relay URL — the public Iroh-canary relay (matches
/// Holochain's own NetworkConfig default). Override with
/// `PROOFPOLL_RELAY_URL` for production.
const DEFAULT_RELAY_URL: &str = "https://use1-1.relay.n0.iroh-canary.iroh.link./";

/// Treat empty-string env vars as unset — covers the common case of a
/// fork's CI referencing `${{ secrets.PROOFPOLL_BOOTSTRAP_URL }}` when
/// the secret isn't configured (GitHub substitutes the empty string),
/// which would otherwise clobber the default with empty.
macro_rules! env_or {
    ($var:literal, $default:expr) => {
        match option_env!($var) {
            Some(s) if !s.is_empty() => s,
            _ => $default,
        }
    };
}

fn bootstrap_url() -> &'static str {
    env_or!("PROOFPOLL_BOOTSTRAP_URL", DEFAULT_BOOTSTRAP_URL)
}

fn signal_url() -> &'static str {
    env_or!("PROOFPOLL_SIGNAL_URL", DEFAULT_SIGNAL_URL)
}

fn relay_url() -> &'static str {
    env_or!("PROOFPOLL_RELAY_URL", DEFAULT_RELAY_URL)
}

/// Bootstrap of the 0.7 line. A 0.7.1 conductor speaks to a kitsune2 0.5.x
/// server only, which is a different host from the 0.6 line's (Flowsta:
/// `bootstrap-7.flowsta.com`). Default: the ecosystem's dev bootstrap, which
/// serves both lines.
fn bootstrap_url_07() -> &'static str {
    env_or!("PROOFPOLL_BOOTSTRAP_URL_07", DEFAULT_BOOTSTRAP_URL)
}

/// Relay of the 0.7 line: the bootstrap host serves the iroh relay in 0.7.
fn relay_url_07() -> &'static str {
    env_or!("PROOFPOLL_RELAY_URL_07", DEFAULT_BOOTSTRAP_URL)
}

fn auth_material() -> Option<&'static str> {
    match option_env!("PROOFPOLL_AUTH_MATERIAL") {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// The library's supervisor: it owns both lines' key stores and conductors.
static SUPERVISOR: LazyLock<Arc<Supervisor>> = LazyLock::new(|| Arc::new(Supervisor::new("pp")));

pub fn supervisor() -> &'static Arc<Supervisor> {
    &SUPERVISOR
}

/// Each line's conductor pid and app port, while it runs.
pub fn line_pid(line: Line) -> Option<u32> {
    supervisor().conductor_pid(line)
}

pub fn line_ports(line: Line) -> Option<(u16, u16)> {
    supervisor().ports(line)
}

/// Stop both lines, each conductor asked to shut down before its key store
/// (blocking: the app's exit, an update's install, an adopt).
pub fn stop_lines() {
    log::info!("Shutting down conductors and key stores...");
    CLOSING.store(true, Ordering::SeqCst);
    if supervisor().stop() {
        log::info!("Conductors and key stores stopped");
    }
    CLOSING.store(false, Ordering::SeqCst);
}

/// Set while the app stops its lines on purpose, so the watch does not read
/// the stop as a conductor that died.
static CLOSING: AtomicBool = AtomicBool::new(false);

/// Conductor status reported to the frontend.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "status")]
pub enum ConductorStatus {
    #[serde(rename = "stopped")]
    Stopped,
    #[serde(rename = "starting")]
    Starting { message: String },
    /// An adopted key's v1.4 chain is being rebuilt from the network
    /// (`restore_from_dht`); the start waits for it, with no time limit.
    #[serde(rename = "restoring")]
    Restoring { waited_secs: u64 },
    #[serde(rename = "ready")]
    Ready { admin_port: u16, app_port: u16 },
    #[serde(rename = "error")]
    Error { message: String },
}

/// The one origin the conductors' admin and app interfaces accept: a random
/// value made once per app run (the library's supervisor keeps it). It is
/// written into each conductor's config file, which only this OS account can
/// read, and presented by every connection this app opens. A web page always
/// presents its own address as its origin, and another account on the device
/// cannot read the file, so neither can open these interfaces.
pub fn node_origin() -> String {
    supervisor().origin_or_fresh()
}

/// Where a key store's socket goes when its own folder's path is over the
/// Unix-socket limit: the folder and naming ProofPoll has always used
/// (shared with the Vault's by design), so an existing key store keeps its
/// socket.
pub fn socket_plan() -> SocketPlan {
    SocketPlan::new("fv", "lair")
}

/// A line's key store and conductor folders in the profile.
pub fn line_dirs(data_dir: &Path, line: Line) -> (PathBuf, PathBuf) {
    match line {
        Line::V06 => (data_dir.join("lair"), data_dir.join("conductor")),
        Line::V07 => (data_dir.join(LAIR_DIR_07), data_dir.join(CONDUCTOR_DIR_07)),
    }
}

/// A line's sidecars: its key store, then its conductor.
fn sidecars(line: Line) -> (&'static str, &'static str) {
    match line {
        Line::V06 => ("proofpoll-lair-keystore", "proofpoll-holochain-06"),
        Line::V07 => ("proofpoll-lair-keystore-07", "proofpoll-holochain"),
    }
}

fn line_admin_port(line: Line) -> u16 {
    match line {
        Line::V06 => admin_port(),
        Line::V07 => admin_port_07(),
    }
}

/// A line's conductor settings, as ProofPoll has always written them: 0.6
/// with `db_sync_strategy: Resilient`; 0.7 with `request_timeout_s: 60`,
/// `db_sync_level: Normal` and NO gossip cap. Upstream 0.7.1 needed a cap
/// (its presence check fails above about 10.9k offered ids,
/// holochain/holochain#6062), and under it a large batch can strand. The
/// conductor ProofPoll ships checks in chunks (WeAreFlowsta/holochain-build),
/// so Kitsune2's own limit applies; a capped conductor elsewhere is never
/// sent more than its own limit.
fn tuning(line: Line) -> NetworkTuning {
    let mut t = NetworkTuning::for_line(line);
    match line {
        Line::V06 => t.db_sync_strategy = Some("Resilient".into()),
        Line::V07 => {
            t.request_timeout_s = Some(60);
            t.db_sync_level = Some("Normal".into());
            t.gossip_max_op_bytes = None;
        }
    }
    t
}

/// A line's rendezvous as this start uses it: with its auth when it answers
/// (or there is none to carry), without it when nothing answers.
async fn rendezvous_for(line: Line) -> Rendezvous {
    let (bootstrap, relay) = match line {
        Line::V06 => (bootstrap_url(), relay_url()),
        Line::V07 => (bootstrap_url_07(), relay_url_07()),
    };
    let with_auth = rendezvous_answers(bootstrap, line).await;
    supervisor().set_rendezvous(line, Some(bootstrap));
    Rendezvous {
        signal_url: (line == Line::V06).then(|| signal_url().to_string()),
        auth_material: auth_material().filter(|_| with_auth).map(str::to_string),
        relay_url: Some(relay.to_string()),
        ..Rendezvous::new(bootstrap)
    }
}

fn step_message(step: StartStep) -> &'static str {
    match step {
        StartStep::KeyStore => "Starting lair-keystore...",
        StartStep::Connect | StartStep::Agents => "Connecting to lair-keystore...",
        StartStep::Conductor => "Starting Holochain conductor...",
        _ => "Waiting for conductor...",
    }
}

/// Full startup sequence: lair → conductor → install DNA → attach app interface.
///
/// ProofPoll uses lair's auto-generated key (no deterministic seed import).
/// Result of the startup sequence, including migration status.
pub struct StartupResult {
    /// The 0.7 line's admin and app ports (the line the page talks to).
    pub admin_port_07: u16,
    pub app_port_07: u16,
    pub agent_key: String,
    /// The key v1.4 runs as (differs from `agent_key` for a conductor-born 0.6 agent).
    pub agent_key_07: String,
    /// The active client: v1.5 on the 0.7.1 conductor.
    pub app_client: crate::hc::AppClient,
    /// v1.4 on the same conductor: the move's source, and reads.
    pub app_client_v1_4: Option<crate::hc::AppClient>,
    /// v1.3 client on the 0.6.1 conductor: the migration source, and reads
    /// and votes for polls not yet migrated.
    pub app_client_v1_3: Option<crate::hc::AppClient>,
    /// v1.2 client for legacy reads.
    pub app_client_v1_2: Option<crate::hc::AppClient>,
    /// v1.1 client for legacy reads.
    pub app_client_v1_1: Option<crate::hc::AppClient>,
    /// v1.0 client for legacy reads.
    pub app_client_v1_0: Option<crate::hc::AppClient>,
    pub lair_client: lair_keystore_api::prelude::LairClient,
}

/// What a start's lines leave for the app once both have joined.
#[derive(Default)]
struct Joined {
    install_06: Option<crate::dna::InstallResult>,
    clients_06: Option<(Option<crate::hc::AppClient>, Option<crate::hc::AppClient>, Option<crate::hc::AppClient>, Option<crate::hc::AppClient>)>,
    lair_client: Option<lair_keystore_api::prelude::LairClient>,
    agent_07: Option<holochain_types::prelude::AgentPubKey>,
    clients_07: Option<(crate::hc::AppClient, Option<crate::hc::AppClient>)>,
    /// Why the 0.6 line did not start, when it did not.
    failed_06: Option<String>,
    joined_06: bool,
}

/// What ProofPoll's lines need from the app (flowsta-hc-lines `LineHooks`).
struct PpLines {
    app: tauri::AppHandle,
    data_dir: PathBuf,
    resource_dir: PathBuf,
    passphrase: String,
    out: tokio::sync::Mutex<Joined>,
    /// Signalled when the 0.6 line has joined or failed.
    old_line: tokio::sync::Notify,
}

impl PpLines {
    fn say(&self, message: &str) {
        let _ = self.app.emit("conductor-status", ConductorStatus::Starting { message: message.into() });
    }

    fn set_status(&self, status: ConductorStatus) {
        use tauri::Manager;
        if let Some(state) = self.app.try_state::<Arc<crate::commands::AppState>>() {
            *state.conductor_status.lock().unwrap() = status.clone();
        }
        let _ = self.app.emit("conductor-status", status);
    }

    /// The 0.6 line: DNAs v1.0 to v1.3 under the device's key, restarted
    /// once when this start installed v1.3 (a 0.6.1 conductor keeps what an
    /// install used until it restarts: ~800 MB after a fresh start, ~280 MB
    /// after a restart), then its app interface.
    async fn setup_06(&self, process: &mut LineProcess) -> Result<Setup, String> {
        self.say("Installing DNA...");
        let lair_client = connect_retrying(|| lair::connect_to_lair(&process.lair_url, &self.passphrase)).await?;
        let install = crate::dna::install_dnas(process.admin_port, &self.resource_dir, &lair_client, &self.data_dir)
            .await
            .map_err(|e| format!("DNA installation failed: {}", e))?;
        if install.installed_now {
            log::info!("v1.3 installed this start: the 0.6 line restarts once to give back the memory the install kept");
            return Ok(Setup { app_port: 0, restart: true });
        }
        self.say("Setting up app interface...");
        let (app_port, v1_3, v1_2, v1_1, v1_0) =
            crate::dna::setup_app_interface(process.admin_port, install.v1_0_available, install.v1_1_available, install.v1_2_available)
                .await
                .map_err(|e| format!("App interface setup failed: {}", e))?;
        let mut out = self.out.lock().await;
        out.install_06 = Some(install);
        out.clients_06 = Some((v1_3, v1_2, v1_1, v1_0));
        out.lair_client = Some(lair_client);
        Ok(app_port.into())
    }

    /// The 0.7 line: v1.4 and v1.5 under the same device seed (an adopted
    /// key's chains rebuilt from the network instead of started again),
    /// restarted once when this start installed any (Holochain 0.7.1 keeps
    /// about 2 GB after a first install until its conductor restarts,
    /// holochain#6064), then its app interface.
    async fn setup_07(&self, process: &mut LineProcess) -> Result<Setup, String> {
        self.say("Installing DNA (0.7)...");
        let lair_client_07 = connect_retrying(|| lair::connect_to_lair_07(&process.lair_url, &self.passphrase)).await?;
        let agent_07 = crate::device_seed::ensure_device_agent_07(&lair_client_07, &self.data_dir)
            .await
            .map_err(|e| format!("Device agent on the 0.7 lair failed: {}", e))?;
        // An adopt whose backup showed the key's 0.7 chains left a marker naming
        // that key and their versions: each of those rebuilds its chain from the
        // network instead of starting a second chain under the same key
        // (seed_adopt.rs).
        let restore_marker = crate::seed_adopt::read_restore_marker(&self.data_dir);
        let restore: Vec<String> = match &restore_marker {
            Some(m) if m.agent == agent_07.to_string() => m.dnas.clone(),
            Some(m) => {
                log::warn!("[restore] the restore marker names {} but this start signs as {}: nothing to restore", m.agent, agent_07);
                crate::seed_adopt::clear_restore_marker(&self.data_dir);
                Vec::new()
            }
            None => Vec::new(),
        };
        let installs = crate::dna::install_dnas_07(process.admin_port, &self.resource_dir, agent_07.get_raw_39(), &restore)
            .await
            .map_err(|e| format!("DNA installation (0.7) failed: {}", e))?;
        let restoring = installs.restoring();
        if !restoring.is_empty() {
            for app in &restoring {
                log::info!(
                    "[restore] v{} is rebuilding this key's chain from the network (the backup held the 0.7 chains to seq {})",
                    app.dna_version,
                    restore_marker.as_ref().map(|m| m.backup_head_seq.to_string()).unwrap_or_else(|| "?".into())
                );
            }
            for app in restoring {
                let progress = |waited_secs: u64| self.set_status(ConductorStatus::Restoring { waited_secs });
                crate::dna::wait_for_restore_07(process.admin_port, app, progress).await?;
            }
            crate::seed_adopt::clear_restore_marker(&self.data_dir);
            use tauri::Manager;
            if let Some(state) = self.app.try_state::<Arc<crate::commands::AppState>>() {
                *state.conductor_status.lock().unwrap() = ConductorStatus::Starting { message: "Restored your history. Finishing setup...".into() };
            }
        } else if restore_marker.is_some() {
            // The chains were already there under this key: nothing left to restore.
            crate::seed_adopt::clear_restore_marker(&self.data_dir);
        }
        if installs.any_new() {
            log::info!("installed on the 0.7 line this start: the 0.7 line restarts once to give back the memory the install kept");
            self.say("Restarting Holochain once after setup...");
            return Ok(Setup { app_port: 0, restart: true });
        }
        let (app_port, client, client_v1_4) =
            crate::dna::setup_app_interface_07(process.admin_port).await.map_err(|e| format!("App interface setup (0.7) failed: {}", e))?;
        let mut out = self.out.lock().await;
        out.agent_07 = Some(agent_07);
        out.clients_07 = Some((client, client_v1_4));
        Ok(app_port.into())
    }
}

/// A key store that just came up may take a moment to accept a connection
/// (on Windows the named pipe registers a second or two after the process
/// starts).
async fn connect_retrying<C, F, Fut>(mut connect: F) -> Result<C, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<C, String>>,
{
    let mut last = String::new();
    for _ in 0..40 {
        match connect().await {
            Ok(c) => return Ok(c),
            Err(e) => last = e,
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Err(last)
}

#[async_trait::async_trait]
impl LineHooks for PpLines {
    async fn start(&self, line: Line, role: Role) -> Result<LineProcess, String> {
        // The conductor port must be free. Anything answering on it now is a
        // conductor that is not ours (a second running ProofPoll, or one an
        // earlier launch left behind that the startup sweep could not stop).
        // Attaching to it would run this profile on another profile's data
        // (seen 2026-09-27: after "Open as this identity" the relaunch
        // picked the new profile but reused the old conductor). Refuse
        // instead; the pre-spawn check is the only moment this is certain.
        let port = line_admin_port(line);
        if let Ok(Ok(_)) = tokio::time::timeout(std::time::Duration::from_millis(400), tokio::net::TcpStream::connect(("127.0.0.1", port))).await {
            return Err(format!(
                "Another ProofPoll is already running on this computer (port {} is in use). \
                 Quit it and open ProofPoll again.",
                port
            ));
        }
        let (lair_name, conductor_name) = sidecars(line);
        let (lair_bin, conductor_bin) = (sidecar_path(lair_name), sidecar_path(conductor_name));
        let (lair_dir, conductor_dir) = line_dirs(&self.data_dir, line);
        let rendezvous = rendezvous_for(line).await;
        // One origin per app start, shared by both lines: the primary makes
        // it, a line attached beside it keeps the one in force.
        let origin = if role == Role::Primary || supervisor().origin().is_empty() { supervisor().fresh_origin() } else { supervisor().origin() };
        supervisor::start_line(
            &LineStart {
                line,
                lair_bin: &lair_bin,
                conductor_bin: &conductor_bin,
                lair_dir: &lair_dir,
                conductor_dir: &conductor_dir,
                passphrase: &self.passphrase,
                sockets: &socket_plan(),
                admin_ports: &[port],
                origin: &origin,
                rendezvous: &rendezvous,
                tuning: &tuning(line),
                // The device's key is put into each key store by the setup
                // (`device_seed`): an install from before the app held the
                // seed keeps the key its 0.6 conductor made.
                agents: &[],
                timeouts: Timeouts::default(),
            },
            |step| {
                if role == Role::Primary {
                    self.say(step_message(step));
                }
            },
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn setup(&self, line: Line, _role: Role, process: &mut LineProcess) -> Result<Setup, String> {
        match line {
            Line::V06 => self.setup_06(process).await,
            Line::V07 => self.setup_07(process).await,
        }
    }

    async fn joined(&self, line: Line, _role: Role, _session: u64, admin_port: u16, app_port: u16) {
        log::info!("[{line}] Holochain conductor ready (admin: {}, app: {})", admin_port, app_port);
        if line == Line::V06 {
            self.out.lock().await.joined_06 = true;
            self.old_line.notify_waiters();
        }
    }

    async fn secondary_failed(&self, line: Line, error: &str) {
        log::warn!("[{line}] did not start: {error}");
        if line == Line::V06 {
            self.out.lock().await.failed_06 = Some(error.to_string());
            self.old_line.notify_waiters();
        }
    }
}

/// Public entry point. Runs one full startup; on failure, performs the
/// right recovery for the error class and retries once.
///
/// Two failure modes are handled distinctly:
///
/// 1. SQLCipher hmac mismatch — lair's `store_file` or the conductor's
///    DHT/source-chain DBs are encrypted under a passphrase that no
///    longer pairs with the current one (typical after a partial
///    uninstall left orphaned crypto material behind). The only
///    recoverable response is to wipe both data dirs and the passphrase
///    file, regenerate, and start fresh. Nothing user-recoverable lives
///    in those dirs — agent keys are regenerated every install and
///    user-authored data comes back via recognition on next sign-in:
///    the fresh agent joins the user's identity link graph
///    (get_my_agent_set) and their polls/votes re-sync from the DHT.
///    The Vault backup is the CAL export + last-resort copy, not the
///    recovery path.
///
/// 2. Any other startup failure — usually a transient kitsune/Iroh hiccup
///    or a Windows lock-release race on first install. Stop what is left,
///    wait briefly, retry on fresh processes.
pub async fn start_holochain(
    app_handle: tauri::AppHandle,
    app_state: std::sync::Arc<crate::commands::AppState>,
    data_dir: PathBuf,
    resource_dir: PathBuf,
    passphrase: String,
) -> Result<StartupResult, String> {
    let first_err = match start_holochain_attempt(app_handle.clone(), data_dir.clone(), resource_dir.clone(), passphrase.clone()).await {
        Ok(result) => return Ok(result),
        Err(e) => e,
    };

    let is_hmac_error = first_err.contains("hmac check failed") || first_err.contains("SQL logic error") || first_err.contains("error decrypting page");

    let retry_passphrase = if is_hmac_error {
        log::warn!("[start_holochain] SQLCipher hmac mismatch on first attempt — performing full state reset: {}", first_err);
        let _ = app_handle.emit("conductor-status", ConductorStatus::Starting { message: "Recovering from corrupted state — full reset...".into() });
        crate::commands::nuke_state_and_regenerate_passphrase(&data_dir, &app_state)
    } else {
        log::warn!("[start_holochain] first attempt failed: {} — auto-restarting", first_err);
        let _ = app_handle.emit("conductor-status", ConductorStatus::Starting { message: "First-run setup needs a quick restart...".into() });
        passphrase
    };

    // Brief pause so the admin ports are released and any process tear-down
    // completes before the second attempt binds the same resources.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    start_holochain_attempt(app_handle, data_dir, resource_dir, retry_passphrase).await
}

/// How long a start waits for the 0.6 line after the 0.7 line is up: its
/// first start on a device installs four DNA versions.
const OLD_LINE_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// One full startup attempt: the 0.7 line (the one the page talks to), then
/// the 0.6 line beside it; both must be up. Either failing stops both.
async fn start_holochain_attempt(app_handle: tauri::AppHandle, data_dir: PathBuf, resource_dir: PathBuf, passphrase: String) -> Result<StartupResult, String> {
    STARTED_OFFLINE.store(false, Ordering::SeqCst);
    let hooks = Arc::new(PpLines {
        app: app_handle.clone(),
        data_dir,
        resource_dir,
        passphrase,
        out: tokio::sync::Mutex::new(Joined::default()),
        old_line: tokio::sync::Notify::new(),
    });
    let stop_all = || async {
        let _ = tokio::task::spawn_blocking(|| supervisor().stop()).await;
    };
    if let Err(e) = supervisor().start_lines(Line::V07, Some(Line::V06), hooks.clone()).await {
        stop_all().await;
        return Err(e);
    }
    // The 0.6 line attaches from a task of the library's: wait for it.
    let deadline = tokio::time::Instant::now() + OLD_LINE_WAIT;
    loop {
        let waiting = hooks.old_line.notified();
        {
            let out = hooks.out.lock().await;
            if let Some(e) = out.failed_06.clone() {
                drop(out);
                stop_all().await;
                return Err(e);
            }
            if out.joined_06 {
                break;
            }
        }
        if tokio::time::timeout_at(deadline, waiting).await.is_err() {
            stop_all().await;
            return Err(format!("The 0.6 line did not come up within {} minutes", OLD_LINE_WAIT.as_secs() / 60));
        }
    }

    let mut out = hooks.out.lock().await;
    let install = out.install_06.take().ok_or("the 0.6 line joined without its install result")?;
    let (app_client_v1_3, app_client_v1_2, app_client_v1_1, app_client_v1_0) = out.clients_06.take().ok_or("the 0.6 line joined without its clients")?;
    let lair_client = out.lair_client.take().ok_or("the 0.6 line joined without its key store client")?;
    let agent_07 = out.agent_07.take().ok_or("the 0.7 line joined without its agent")?;
    let (app_client, app_client_v1_4) = out.clients_07.take().ok_or("the 0.7 line joined without its clients")?;
    drop(out);
    if agent_07 != install.agent_pub_key {
        // A conductor-born agent (pre app-held seed) cannot leave the 0.6 lair.
        log::warn!(
            "The 0.7 line signs as a new agent {} (the 0.6 line's {} was conductor-born); carried records name the old author",
            agent_07, install.agent_pub_key
        );
    }
    *LEGACY_AVAILABLE.lock().unwrap() = (install.v1_0_available, install.v1_1_available, install.v1_2_available);
    *HOOKS.lock().unwrap() = Some(hooks.clone());

    let (admin_07, app_port_07) = line_ports(Line::V07).ok_or("the 0.7 line is not running")?;
    let (admin_06, app_port_06) = line_ports(Line::V06).ok_or("the 0.6 line is not running")?;
    let agent_key_str = install.agent_pub_key.to_string();
    let _ = app_handle.emit("conductor-status", ConductorStatus::Ready { admin_port: admin_07, app_port: app_port_07 });
    log::info!(
        "Holochain conductors ready (0.6 admin {} app {}; 0.7 admin {} app {}; agent {}; migration: {})",
        admin_06, app_port_06, admin_07, app_port_07, agent_key_str, install.needs_migration,
    );

    Ok(StartupResult {
        admin_port_07: admin_07,
        app_port_07,
        agent_key: agent_key_str,
        agent_key_07: agent_07.to_string(),
        app_client,
        app_client_v1_4,
        app_client_v1_3,
        app_client_v1_2,
        app_client_v1_1,
        app_client_v1_0,
        lair_client,
    })
}

/// Which older apps the 0.6 conductor holds (its app interface serves them).
static LEGACY_AVAILABLE: std::sync::Mutex<(bool, bool, bool)> = std::sync::Mutex::new((false, false, false));
/// The running session's hooks: what a line attached again is started with.
static HOOKS: std::sync::Mutex<Option<Arc<PpLines>>> = std::sync::Mutex::new(None);

/// Whether this session's conductors started with no rendezvous answering,
/// and so without its auth. Holochain 0.6.1 and 0.7.1 authenticate with the
/// relay while they start, and a conductor that cannot (no network) refuses
/// to start at all ("Authenticate Failed", exit 101). Started without the
/// auth, everything local works; the conductors restart with it once the
/// rendezvous answers again.
static STARTED_OFFLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn started_offline() -> bool {
    STARTED_OFFLINE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Whether a line's config carries the auth on this start: always when there
/// is none to carry, otherwise only when its rendezvous answers.
async fn rendezvous_answers(bootstrap: &str, line: crate::hc::Line) -> bool {
    if auth_material().is_none() || bootstrap_alive(bootstrap).await {
        return true;
    }
    log::warn!(
        "[{:?}] no rendezvous answers ({}): starting without its auth; the conductors restart with it once it answers",
        line, bootstrap
    );
    STARTED_OFFLINE.store(true, std::sync::atomic::Ordering::SeqCst);
    false
}

/// Whether a bootstrap server answers its health check.
async fn bootstrap_alive(url: &str) -> bool {
    #[cfg(debug_assertions)]
    if test_offline_now() {
        return false;
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(2500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let health = format!("{}/health", url.trim_end_matches('/'));
    matches!(client.get(&health).send().await, Ok(r) if r.status().is_success())
}

/// A debug build takes `PROOFPOLL_TEST_OFFLINE_UNTIL=<unix seconds>`: every
/// rendezvous check fails until then ("offline, then back" in a test). A
/// release build never reads it.
#[cfg(debug_assertions)]
fn test_offline_now() -> bool {
    std::env::var("PROOFPOLL_TEST_OFFLINE_UNTIL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|until| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() < until)
                .unwrap_or(false)
        })
}


/// What a start gives the app: the clients, the keys, Ready. From the first
/// start (`lib.rs`) and from every start after it (`restart_session`).
pub async fn apply_started(state: &std::sync::Arc<crate::commands::AppState>, app_handle: &tauri::AppHandle, result: StartupResult) {
    let ready = ConductorStatus::Ready { admin_port: result.admin_port_07, app_port: result.app_port_07 };
    *state.agent_pub_key.lock().unwrap() = Some(result.agent_key);
    *state.agent_pub_key_07.lock().unwrap() = Some(result.agent_key_07);
    *state.app_client.lock().await = Some(result.app_client);
    *state.app_client_v1_4.lock().await = result.app_client_v1_4;
    *state.app_client_v1_3.lock().await = result.app_client_v1_3;
    *state.app_client_v1_2.lock().await = result.app_client_v1_2;
    *state.app_client_v1_1.lock().await = result.app_client_v1_1;
    *state.app_client_v1_0.lock().await = result.app_client_v1_0;
    *state.lair_client.lock().await = Some(result.lair_client);
    *state.conductor_status.lock().unwrap() = ready.clone();
    let _ = app_handle.emit("conductor-status", ready);
}

/// One start at a time after the first: the watch and the reconnect both
/// come here.
static RESTART: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Stop both lines cleanly and start the session again from the app's
/// state: with the network's auth once the rendezvous answers, after a
/// conductor died, or to give back memory.
async fn restart_session(state: &std::sync::Arc<crate::commands::AppState>, app_handle: &tauri::AppHandle, message: &str) -> Result<(), String> {
    let _one = RESTART.lock().await;
    // Not Ready while the lines stop: nothing reads the stop as a failure.
    let starting = ConductorStatus::Starting { message: message.into() };
    *state.conductor_status.lock().unwrap() = starting.clone();
    let _ = app_handle.emit("conductor-status", starting);
    let _ = tokio::task::spawn_blocking(|| supervisor().stop()).await;
    let data_dir = state.data_dir.clone();
    let passphrase = state.passphrase.lock().unwrap().clone();
    let resource_dir = crate::resolve_resource_dir(app_handle, &state.device_root);
    let result = start_holochain(app_handle.clone(), state.clone(), data_dir, resource_dir, passphrase).await?;
    apply_started(state, app_handle, result).await;
    Ok(())
}

const STOPPED_MESSAGE: &str = "The Holochain conductor stopped unexpectedly. Restart the app to reconnect.";

fn say_stopped(state: &std::sync::Arc<crate::commands::AppState>, app_handle: &tauri::AppHandle) {
    let status = ConductorStatus::Error { message: STOPPED_MESSAGE.into() };
    *state.conductor_status.lock().unwrap() = status.clone();
    let _ = app_handle.emit("conductor-status", status);
}

/// A session that started offline: once both rendezvous answer (checked each
/// minute), restart the lines with the auth.
pub fn spawn_reconnect_when_online(state: std::sync::Arc<crate::commands::AppState>, app_handle: tauri::AppHandle) {
    if !started_offline() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if !started_offline() {
                break;
            }
            if !(bootstrap_alive(bootstrap_url()).await && bootstrap_alive(bootstrap_url_07()).await) {
                continue;
            }
            log::info!("[network] the rendezvous answers again: restarting the conductors with its auth");
            match restart_session(&state, &app_handle, "Connecting to the network...").await {
                Ok(()) => log::info!("[network] the conductors are back with the network's auth"),
                Err(e) => {
                    log::error!("[network] the conductors did not start again: {}", e);
                    say_stopped(&state, &app_handle);
                }
            }
            break;
        }
    });
}

// ── The watch ────────────────────────────────────────────────────────────

/// Both lines must run: a conductor that died is started again (the whole
/// session: the page's clients are replaced with it), at most three times
/// in ten minutes; after that the app says so and waits for a restart.
const RECOVERIES_MOST: usize = 3;
const RECOVERIES_WITHIN: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Restart the lines for their memory: Holochain 0.7.1 keeps every reloaded
/// module's machine code until a restart (holochain#6064). Above 1 GiB
/// resident, after 20 minutes running, at most once in 6 hours.
const MEMORY_POLICY: flowsta_hc_lines::watch::MemoryPolicy = flowsta_hc_lines::watch::MemoryPolicy {
    over_bytes: 1 << 30,
    min_uptime: std::time::Duration::from_secs(20 * 60),
    min_gap: std::time::Duration::from_secs(6 * 60 * 60),
};

/// `MEMORY_POLICY`, or in a debug build
/// `PROOFPOLL_TEST_MEMORY_RESTART=<MB>,<uptime secs>,<gap secs>`.
fn memory_policy() -> flowsta_hc_lines::watch::MemoryPolicy {
    #[cfg(debug_assertions)]
    if let Some(p) = test_memory_policy(std::env::var("PROOFPOLL_TEST_MEMORY_RESTART").ok().as_deref()) {
        return p;
    }
    MEMORY_POLICY
}

#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn test_memory_policy(v: Option<&str>) -> Option<flowsta_hc_lines::watch::MemoryPolicy> {
    let mut parts = v?.split(',').map(|x| x.trim().parse::<u64>().ok());
    let (mb, uptime, gap) = (parts.next()??, parts.next()??, parts.next()??);
    Some(flowsta_hc_lines::watch::MemoryPolicy { over_bytes: mb << 20, min_uptime: std::time::Duration::from_secs(uptime), min_gap: std::time::Duration::from_secs(gap) })
}

/// Whether one more recovery fits the limit, counting it when it does.
fn recovery_allowed(recoveries: &mut std::collections::VecDeque<std::time::Instant>, now: std::time::Instant) -> bool {
    while recoveries.front().is_some_and(|t| now.duration_since(*t) >= RECOVERIES_WITHIN) {
        recoveries.pop_front();
    }
    if recoveries.len() >= RECOVERIES_MOST {
        return false;
    }
    recoveries.push_back(now);
    true
}

/// Whether a line this app needs is down: no session, or either conductor
/// not running. (The library lets a dead previous line go; ProofPoll still
/// reads and votes on its 0.6 line, so it wants it back.)
fn a_line_is_down() -> bool {
    use flowsta_hc_lines::watch::{decide, LinesNow, WatchAction, WatchPolicy};
    let lines = LinesNow::of(supervisor());
    decide(&lines, &WatchPolicy { current: Line::V07, limit: None }) != WatchAction::Nothing || lines.v06 != Some(true)
}

static WATCHING: AtomicBool = AtomicBool::new(false);

/// Watch both lines while the app runs (one task a process): recover a
/// conductor that stopped, and restart the lines at most once in six hours
/// when one holds over 1 GiB.
pub fn spawn_watch(state: std::sync::Arc<crate::commands::AppState>, app_handle: tauri::AppHandle) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut recoveries = std::collections::VecDeque::new();
        let mut last_memory_restart: Option<std::time::Instant> = None;
        let mut tick: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            tick += 1;
            // Only a session the page was told is Ready: a start, a restart
            // and an intended stop are not failures.
            let ready = matches!(*state.conductor_status.lock().unwrap(), ConductorStatus::Ready { .. });
            if !ready || CLOSING.load(Ordering::SeqCst) || RESTART.try_lock().is_err() {
                continue;
            }
            if a_line_is_down() {
                if !recovery_allowed(&mut recoveries, std::time::Instant::now()) {
                    log::error!("[watch] a conductor stopped again after {} recoveries in {} min: held", RECOVERIES_MOST, RECOVERIES_WITHIN.as_secs() / 60);
                    say_stopped(&state, &app_handle);
                    continue;
                }
                log::warn!("[watch] a conductor stopped: starting the lines again");
                match restart_session(&state, &app_handle, "Reconnecting to Holochain...").await {
                    Ok(()) => log::info!("[watch] the lines are back"),
                    Err(e) => {
                        log::error!("[watch] the lines did not start again: {}", e);
                        say_stopped(&state, &app_handle);
                    }
                }
                continue;
            }
            // Memory, once a minute.
            if tick % 20 != 0 {
                continue;
            }
            let policy = memory_policy();
            let since = last_memory_restart.map(|t| t.elapsed());
            let heavy = [Line::V07, Line::V06].into_iter().find_map(|line| {
                let m = flowsta_hc_lines::watch::conductor_memory(line_pid(line)?)?;
                flowsta_hc_lines::watch::memory_restart_due(&m, since, crate::commands::quiet_now(&state), &policy).then_some((line, m.resident_bytes))
            });
            if let Some((line, bytes)) = heavy {
                log::info!("[memory] the {} conductor holds {} MB: restarting the lines at a quiet moment to give it back", line, bytes >> 20);
                last_memory_restart = Some(std::time::Instant::now());
                if let Err(e) = restart_session(&state, &app_handle, "Restarting Holochain to free memory...").await {
                    log::error!("[memory] the lines did not start again: {}", e);
                    say_stopped(&state, &app_handle);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No gossip cap on 0.7 (see `tuning`), and the settings ProofPoll's
    /// configs have always carried.
    #[test]
    fn the_lines_keep_their_settings_and_07_carries_no_gossip_cap() {
        let t7 = tuning(Line::V07);
        assert_eq!((t7.gossip_max_op_bytes, t7.request_timeout_s, t7.db_sync_level.as_deref()), (None, Some(60), Some("Normal")));
        let t6 = tuning(Line::V06);
        assert_eq!((t6.db_sync_strategy.as_deref(), t6.request_timeout_s), (Some("Resilient"), None));
    }

    #[test]
    fn each_line_has_its_own_folders_sidecars_and_port() {
        let root = Path::new("/p");
        assert_eq!(line_dirs(root, Line::V06), (PathBuf::from("/p/lair"), PathBuf::from("/p/conductor")));
        assert_eq!(line_dirs(root, Line::V07), (PathBuf::from("/p/lair-07"), PathBuf::from("/p/conductor-07")));
        assert_eq!(sidecars(Line::V06), ("proofpoll-lair-keystore", "proofpoll-holochain-06"));
        assert_eq!(sidecars(Line::V07), ("proofpoll-lair-keystore-07", "proofpoll-holochain"));
        assert_ne!(line_admin_port(Line::V06), line_admin_port(Line::V07));
    }

    #[test]
    fn a_fourth_recovery_in_ten_minutes_is_held_and_an_old_one_stops_counting() {
        let mut r = std::collections::VecDeque::new();
        let t0 = std::time::Instant::now();
        for i in 0..RECOVERIES_MOST {
            assert!(recovery_allowed(&mut r, t0 + std::time::Duration::from_secs(i as u64)));
        }
        assert!(!recovery_allowed(&mut r, t0 + std::time::Duration::from_secs(60)), "the fourth is held");
        assert!(recovery_allowed(&mut r, t0 + RECOVERIES_WITHIN + std::time::Duration::from_secs(1)), "the first has left the window");
    }

    #[test]
    fn the_test_memory_policy_reads_three_numbers_or_nothing() {
        let p = test_memory_policy(Some("300, 60,120")).unwrap();
        assert_eq!((p.over_bytes, p.min_uptime.as_secs(), p.min_gap.as_secs()), (300 << 20, 60, 120));
        assert!(test_memory_policy(Some("300,60")).is_none() && test_memory_policy(None).is_none());
    }
}
