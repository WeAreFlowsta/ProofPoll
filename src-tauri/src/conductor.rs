//! Holochain conductor lifecycle management for ProofPoll.
//!
//! Starts lair-keystore and holochain as child processes, installs the DNA,
//! and sets up WebSocket connections for zome calls.
//!
//! ## For forking developers
//!
//! This file is reusable infrastructure. The only things you might change:
//!   - `ADMIN_WS_PORT` (4466) — change if running alongside other Holochain apps
//!   - Bootstrap / signal / relay URLs and the optional auth material — set
//!     via compile-time env vars (see "Bootstrap configuration" below).
//!     Defaults connect to the Holochain ecosystem's public dev bootstrap,
//!     so a fresh fork builds and runs without any Flowsta dependency.
//!   - The startup sequence calls `install_dnas()` from `dna.rs` — that's
//!     where your app-specific hApp bundle names are configured

use crate::lair;
use crate::process_ext::CommandExt as _;
use crate::sidecar::sidecar_path;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use tauri::Emitter;

/// Admin WebSocket port for the local Holochain conductor.
/// Different from Flowsta Vault's 4455 so both can run simultaneously.
/// Change this if running alongside other Holochain apps.
pub const ADMIN_WS_PORT: u16 = 4466;

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

fn auth_material() -> Option<&'static str> {
    match option_env!("PROOFPOLL_AUTH_MATERIAL") {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Handle to a running conductor + lair-keystore pair.
pub struct ConductorHandle {
    pub lair_child: Child,
    pub conductor_child: Child,
    pub admin_port: u16,
    pub app_port: u16,
    pub conductor_pid: u32,
}

impl ConductorHandle {
    pub fn shutdown(mut self) {
        log::info!("Shutting down conductor...");
        if let Err(e) = self.conductor_child.kill() {
            log::warn!("Failed to kill conductor process: {}", e);
        }
        let _ = self.conductor_child.wait();

        log::info!("Shutting down lair-keystore...");
        if let Err(e) = self.lair_child.kill() {
            log::warn!("Failed to kill lair-keystore process: {}", e);
        }
        let _ = self.lair_child.wait();

        log::info!("Conductor and lair-keystore stopped");
    }
}

/// Conductor status reported to the frontend.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "status")]
pub enum ConductorStatus {
    #[serde(rename = "stopped")]
    Stopped,
    #[serde(rename = "starting")]
    Starting { message: String },
    #[serde(rename = "ready")]
    Ready { admin_port: u16, app_port: u16 },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Generate conductor-config.yaml for ProofPoll.
fn generate_conductor_config(
    conductor_dir: &Path,
    lair_connection_url: &str,
    admin_port: u16,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(conductor_dir)
        .map_err(|e| format!("Failed to create conductor directory: {}", e))?;

    // Conditionally include base64_auth_material_bootstrap AND
    // base64_auth_material_relay. Indented to match the `network:`
    // block; empty string when no auth material is configured (the
    // common case for fork developers). Same value goes into both
    // fields — see the module-level comment for why.
    let auth_line = match auth_material() {
        Some(material) => format!(
            "  base64_auth_material_bootstrap: \"{m}\"\n  base64_auth_material_relay: \"{m}\"\n",
            m = material,
        ),
        None => String::new(),
    };

    // Path values use SINGLE-quoted YAML strings — double-quoted YAML
    // interprets backslash escapes (e.g. "C:\Users\..." reads "\U" as the
    // start of a Unicode escape and bombs out on the first non-hex
    // character). Single-quoted strings pass backslashes through verbatim.
    // The only character that needs escaping inside single quotes is the
    // single quote itself; doubling it is the YAML convention.
    let data_root = conductor_dir.display().to_string().replace('\'', "''");
    let lair_url = lair_connection_url.replace('\'', "''");

    let config = format!(
        r#"data_root_path: '{data_root}'
keystore:
  type: lair_server
  connection_url: '{lair_url}'
admin_interfaces:
- driver:
    type: websocket
    port: {admin_port}
    allowed_origins: '*'
network:
  bootstrap_url: {bootstrap}
  signal_url: {signal}
  relay_url: {relay}
{auth_line}db_sync_strategy: Resilient
"#,
        data_root = data_root,
        admin_port = admin_port,
        lair_url = lair_url,
        bootstrap = bootstrap_url(),
        signal = signal_url(),
        relay = relay_url(),
        auth_line = auth_line,
    );

    let config_path = conductor_dir.join("conductor-config.yaml");
    std::fs::write(&config_path, &config)
        .map_err(|e| format!("Failed to write conductor config: {}", e))?;

    log::info!("Conductor config written to {:?}", config_path);
    Ok(config_path)
}

/// Start the holochain conductor process.
fn start_conductor_process(
    config_path: &Path,
    conductor_dir: &Path,
    passphrase: &str,
) -> Result<Child, String> {
    log::info!("Starting holochain conductor...");

    let stdout_path = conductor_dir.join("holochain-stdout.log");
    let stderr_path = conductor_dir.join("holochain-stderr.log");

    let stdout_file = std::fs::File::create(&stdout_path)
        .map_err(|e| format!("Failed to create conductor stdout log: {}", e))?;
    let stderr_file = std::fs::File::create(&stderr_path)
        .map_err(|e| format!("Failed to create conductor stderr log: {}", e))?;

    let mut child = std::process::Command::new(sidecar_path("proofpoll-holochain"))
        .arg("-c")
        .arg(config_path)
        .arg("--piped")
        .stdin(Stdio::piped())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .tie_to_parent()
        .spawn_hidden()
        .map_err(|e| format!("Failed to spawn holochain conductor: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(format!("{}\n", passphrase).as_bytes())
            .map_err(|e| format!("Failed to write passphrase to conductor: {}", e))?;
    }

    log::info!("Holochain conductor started (pid {})", child.id());

    // Brief check for immediate failure.
    std::thread::sleep(std::time::Duration::from_millis(500));
    match child.try_wait() {
        Ok(Some(status)) => {
            let output = read_conductor_logs(conductor_dir);
            Err(format!(
                "Holochain conductor exited immediately (status {}): {}",
                status, output.trim()
            ))
        }
        Ok(None) => Ok(child),
        Err(e) => Err(format!("Failed to check conductor process status: {}", e)),
    }
}

fn read_conductor_logs(conductor_dir: &Path) -> String {
    let stderr_path = conductor_dir.join("holochain-stderr.log");
    let stdout_path = conductor_dir.join("holochain-stdout.log");

    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();

    let output = if !stderr.is_empty() { stderr } else { stdout };
    if output.len() > 500 {
        format!("{}...", &output[..500])
    } else {
        output
    }
}

/// Wait for the conductor admin WebSocket to be ready.
async fn wait_for_admin_ws(
    port: u16,
    timeout_secs: u64,
    conductor_child: &mut Child,
    conductor_dir: &Path,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut attempt = 0;

    while std::time::Instant::now() < deadline {
        attempt += 1;

        match conductor_child.try_wait() {
            Ok(Some(status)) => {
                let output = read_conductor_logs(conductor_dir);
                return Err(format!(
                    "Conductor exited during startup (status {}): {}",
                    status,
                    output.trim()
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("Failed to check conductor process: {}", e)),
        }

        match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)).await {
            Ok(_) => {
                log::info!(
                    "Conductor admin WS ready on port {} (attempt {})",
                    port,
                    attempt
                );
                return Ok(());
            }
            Err(_) => {
                if attempt <= 3 {
                    log::info!("Waiting for conductor admin WS (attempt {})...", attempt);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }

    let output = read_conductor_logs(conductor_dir);
    if !output.trim().is_empty() {
        Err(format!(
            "Conductor not ready after {}s. Logs: {}",
            timeout_secs, output.trim()
        ))
    } else {
        Err(format!(
            "Conductor admin WS not ready after {}s on port {}",
            timeout_secs, port
        ))
    }
}

/// Full startup sequence: lair → conductor → install DNA → attach app interface.
///
/// ProofPoll uses lair's auto-generated key (no deterministic seed import).
/// Result of the startup sequence, including migration status.
pub struct StartupResult {
    pub handle: ConductorHandle,
    pub agent_key: String,
    pub app_client: holochain_client::AppWebsocket,
    /// v1.2 client for migration reads (v1.2 → v1.3).
    pub app_client_v1_2: Option<holochain_client::AppWebsocket>,
    /// v1.1 client for legacy reads.
    pub app_client_v1_1: Option<holochain_client::AppWebsocket>,
    /// v1.0 client for legacy reads.
    pub app_client_v1_0: Option<holochain_client::AppWebsocket>,
    pub lair_client: lair_keystore_api::prelude::LairClient,
    pub needs_migration: bool,
}

/// Public entry point. Runs the full startup once; if it fails we kill
/// any leftover children, wait briefly for OS resources to free, and try
/// a second time on a fresh process.
///
/// Why: on Windows the first fresh-install run sometimes leaves lair or
/// the conductor in a state where the next clean start succeeds — the
/// retry automates the manual "close the app and reopen" workaround
/// users would otherwise have to do. The Vault adopted the same pattern
/// in `flowsta-vault` commit 3774082 for the same reason.
pub async fn start_holochain(
    app_handle: tauri::AppHandle,
    data_dir: PathBuf,
    resource_dir: PathBuf,
    passphrase: String,
) -> Result<StartupResult, String> {
    match start_holochain_attempt(
        app_handle.clone(),
        data_dir.clone(),
        resource_dir.clone(),
        passphrase.clone(),
    )
    .await
    {
        Ok(result) => return Ok(result),
        Err(e) => {
            log::warn!(
                "[start_holochain] first attempt failed: {} — auto-restarting",
                e,
            );
            let _ = app_handle.emit(
                "conductor-status",
                ConductorStatus::Starting {
                    message: "First-run setup needs a quick restart...".into(),
                },
            );
            // Brief pause so the admin WS port is released and any
            // process tear-down completes before the second attempt
            // binds the same resources.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    start_holochain_attempt(app_handle, data_dir, resource_dir, passphrase).await
}

/// One full startup attempt: lair → connect → conductor → install DNAs.
async fn start_holochain_attempt(
    app_handle: tauri::AppHandle,
    data_dir: PathBuf,
    resource_dir: PathBuf,
    passphrase: String,
) -> Result<StartupResult, String> {
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Starting lair-keystore...".into(),
        },
    );

    // 1. Start lair-keystore.
    let lair_dir = data_dir.join("lair");
    let (mut lair_child, connection_url) = lair::start_lair_process(&lair_dir, &passphrase)?;

    macro_rules! fail_with_lair_cleanup {
        ($err:expr) => {{
            let _ = lair_child.kill();
            let _ = lair_child.wait();
            return Err($err);
        }};
    }

    // 2. Wait for lair socket.
    if let Err(e) = lair::wait_for_lair_socket(&connection_url, 15).await {
        fail_with_lair_cleanup!(e);
    }

    // 3. Connect to lair. On Windows the named pipe is registered in the
    //    kernel namespace asynchronously after the server process spawns —
    //    "file not found" for the first second or two is normal. Retry until
    //    the pipe is up or the lair process dies. On Unix this almost always
    //    succeeds first try because step 2 already waited for the socket
    //    file.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Connecting to lair-keystore...".into(),
        },
    );
    let lair_client = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut last_err: Option<String> = None;
        loop {
            // Short-circuit if lair died — no amount of retrying brings it back.
            if let Ok(Some(status)) = lair_child.try_wait() {
                let logs = lair::read_lair_logs(&lair_dir, "server");
                fail_with_lair_cleanup!(format!(
                    "lair-keystore exited unexpectedly (status {}): {}",
                    status, logs
                ));
            }
            match lair::connect_to_lair(&connection_url, &passphrase).await {
                Ok(c) => break c,
                Err(e) => {
                    // "Pipe not ready yet" — keep waiting.
                    let not_ready = e.contains("os error 2")
                        || e.contains("cannot find the file")
                        || e.contains("No such file or directory");
                    if not_ready && std::time::Instant::now() < deadline {
                        last_err = Some(e);
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        continue;
                    }
                    let logs = lair::read_lair_logs(&lair_dir, "server");
                    fail_with_lair_cleanup!(format!(
                        "{} (lair output: {})",
                        last_err.unwrap_or(e),
                        logs,
                    ));
                }
            }
        }
    };
    log::info!("Connected to lair-keystore");

    // 4. Generate conductor config.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Starting Holochain conductor...".into(),
        },
    );
    let conductor_dir = data_dir.join("conductor");
    let config_path =
        match generate_conductor_config(&conductor_dir, &connection_url, ADMIN_WS_PORT) {
            Ok(p) => p,
            Err(e) => fail_with_lair_cleanup!(e),
        };

    // 5. Start conductor process.
    let mut conductor_child = match start_conductor_process(&config_path, &conductor_dir, &passphrase) {
        Ok(c) => c,
        Err(e) => fail_with_lair_cleanup!(e),
    };

    // 6. Wait for admin WebSocket.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Waiting for conductor...".into(),
        },
    );
    if let Err(e) =
        wait_for_admin_ws(ADMIN_WS_PORT, 30, &mut conductor_child, &conductor_dir).await
    {
        let _ = conductor_child.kill();
        let _ = conductor_child.wait();
        fail_with_lair_cleanup!(e);
    }

    // 7. Install ProofPoll DNA.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Installing DNA...".into(),
        },
    );

    macro_rules! fail_with_full_cleanup {
        ($err:expr) => {{
            let _ = conductor_child.kill();
            let _ = conductor_child.wait();
            fail_with_lair_cleanup!($err);
        }};
    }

    let install_result =
        match crate::dna::install_dnas(ADMIN_WS_PORT, &resource_dir).await {
            Ok(r) => r,
            Err(e) => fail_with_full_cleanup!(format!("DNA installation failed: {}", e)),
        };

    // 8. Attach app interface.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Setting up app interface...".into(),
        },
    );
    let (app_port, app_client, app_client_v1_2, app_client_v1_1, app_client_v1_0) =
        match crate::dna::setup_app_interface(
            ADMIN_WS_PORT,
            install_result.v1_0_available,
            install_result.v1_1_available,
            install_result.v1_2_available,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => fail_with_full_cleanup!(format!("App interface setup failed: {}", e)),
        };

    // 9. Get the agent key string for the frontend.
    let agent_key_str = install_result.agent_pub_key.to_string();

    // 10. Emit ready.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Ready {
            admin_port: ADMIN_WS_PORT,
            app_port,
        },
    );
    log::info!(
        "Holochain conductor ready (admin: {}, app: {}, agent: {}, migration: {})",
        ADMIN_WS_PORT,
        app_port,
        agent_key_str,
        install_result.needs_migration,
    );

    let conductor_pid = conductor_child.id();
    let handle = ConductorHandle {
        lair_child,
        conductor_child,
        admin_port: ADMIN_WS_PORT,
        app_port,
        conductor_pid,
    };

    Ok(StartupResult {
        handle,
        agent_key: agent_key_str,
        app_client,
        app_client_v1_2,
        app_client_v1_1,
        app_client_v1_0,
        lair_client,
        needs_migration: install_result.needs_migration,
    })
}

/// Spawn a background task that monitors the conductor process.
/// If the conductor exits unexpectedly, updates ConductorStatus and emits
/// a frontend event so the UI can show a recovery prompt.
pub fn spawn_health_monitor(
    conductor_pid: u32,
    state: std::sync::Arc<crate::commands::AppState>,
    app_handle: tauri::AppHandle,
) {
    tauri::async_runtime::spawn(async move {
        let pid = conductor_pid as i32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            // Check if process is still alive via kill(pid, 0) on Unix.
            // Windows lacks libc::kill — for now we just skip the proactive
            // check there; a conductor crash surfaces via the next failing
            // API call instead. To restore on Windows, use windows-sys
            // `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
            // `GetExitCodeProcess` and treat `STILL_ACTIVE` (259) as alive.
            #[cfg(unix)]
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            #[cfg(not(unix))]
            let alive = true;
            if !alive {
                let current = state.conductor_status.lock().unwrap().clone();
                // Only report if we were in Ready state (not already Error/Stopped)
                if matches!(current, ConductorStatus::Ready { .. }) {
                    log::error!("Conductor process (pid {}) exited unexpectedly", pid);
                    let err_status = ConductorStatus::Error {
                        message: "The Holochain conductor stopped unexpectedly. Restart the app to reconnect.".into(),
                    };
                    *state.conductor_status.lock().unwrap() = err_status.clone();
                    let _ = app_handle.emit("conductor-status", err_status);
                }
                break;
            }
        }
    });
}
