//! Holochain conductor lifecycle management for ProofPoll.
//!
//! Starts lair-keystore and holochain as child processes, installs the DNA,
//! and sets up WebSocket connections for zome calls.
//!
//! ## For forking developers
//!
//! This file is reusable infrastructure. The only things you might change:
//!   - `ADMIN_WS_PORT` (4466) — change if running alongside other Holochain apps
//!   - `BOOTSTRAP_URL` / `SIGNAL_URL` — change to your own bootstrap server
//!     for production, or keep the test server for development
//!   - The startup sequence calls `install_dnas()` from `dna.rs` — that's
//!     where your app-specific hApp bundle names are configured

use crate::lair;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use tauri::Emitter;

/// Admin WebSocket port for the local Holochain conductor.
/// Different from Flowsta Vault's 4455 so both can run simultaneously.
/// Change this if running alongside other Holochain apps.
pub const ADMIN_WS_PORT: u16 = 4466;

/// Holochain bootstrap/signaling server for peer discovery.
/// This is the public test server — for production, deploy your own
/// kitsune2-bootstrap-srv instance and update these URLs.
const BOOTSTRAP_URL: &str = "https://dev-test-bootstrap2.holochain.org/";
const SIGNAL_URL: &str = "wss://dev-test-bootstrap2.holochain.org/";

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

    let config = format!(
        r#"data_root_path: {data_root}
keystore:
  type: lair_server
  connection_url: "{lair_url}"
admin_interfaces:
- driver:
    type: websocket
    port: {admin_port}
    allowed_origins: '*'
network:
  bootstrap_url: {bootstrap}
  signal_url: {signal}
db_sync_strategy: Resilient
"#,
        data_root = conductor_dir.display(),
        admin_port = admin_port,
        lair_url = lair_connection_url,
        bootstrap = BOOTSTRAP_URL,
        signal = SIGNAL_URL,
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

    let mut child = std::process::Command::new("holochain")
        .arg("-c")
        .arg(config_path)
        .arg("--piped")
        .stdin(Stdio::piped())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
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
    pub app_client_v1_0: Option<holochain_client::AppWebsocket>,
    pub lair_client: lair_keystore_api::prelude::LairClient,
    pub needs_migration: bool,
}

pub async fn start_holochain(
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

    // 3. Connect to lair.
    let _ = app_handle.emit(
        "conductor-status",
        ConductorStatus::Starting {
            message: "Connecting to lair-keystore...".into(),
        },
    );
    let lair_client = match lair::connect_to_lair(&connection_url, &passphrase).await {
        Ok(c) => c,
        Err(e) => fail_with_lair_cleanup!(e),
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
    let (app_port, app_client, app_client_v1_0) =
        match crate::dna::setup_app_interface(ADMIN_WS_PORT, install_result.v1_0_available).await {
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

            // Check if process is still alive via kill(pid, 0)
            let alive = unsafe { libc::kill(pid, 0) } == 0;
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
