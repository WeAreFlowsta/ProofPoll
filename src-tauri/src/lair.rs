//! Lair keystore management — reusable infrastructure.
//!
//! Lair is Holochain's key management daemon. It stores the agent's Ed25519
//! signing key and handles cryptographic operations. This module starts
//! lair-keystore as a child process and connects to it via a Unix socket
//! (Linux/macOS) or a Windows named pipe.
//!
//! For forking developers: this file needs no changes. It works for any
//! Holochain app that uses the standard random agent key approach.

use lair_keystore_api::prelude::*;
use percent_encoding::percent_decode_str;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::process_ext::CommandExt as _;
use crate::sidecar::sidecar_path;

/// Start a lair-keystore process.
///
/// On first run (no config file), initializes the keystore.
/// Then starts the server process.
/// Returns the child process handle and the connection URL.
pub fn start_lair_process(
    lair_dir: &Path,
    passphrase: &str,
) -> Result<(Child, String), String> {
    std::fs::create_dir_all(lair_dir)
        .map_err(|e| format!("Failed to create lair directory: {}", e))?;

    let config_path = lair_dir.join("lair-keystore-config.yaml");
    let is_first_run = !config_path.exists();

    if is_first_run {
        log::info!("First run: initializing lair-keystore...");
        // Redirect init's stdio to disk so a silent crash leaves a trail.
        // Piped handles that nothing ever drains are functionally /dev/null.
        let init_stdout = open_log_file(lair_dir, "lair-init-stdout.log")?;
        let init_stderr = open_log_file(lair_dir, "lair-init-stderr.log")?;
        let mut child = Command::new(sidecar_path("proofpoll-lair-keystore"))
            .arg("init")
            .arg("--piped")
            .current_dir(lair_dir)
            .stdin(Stdio::piped())
            .stdout(init_stdout)
            .stderr(init_stderr)
            .tie_to_parent()
            .spawn_hidden()
            .map_err(|e| format!("Failed to spawn lair-keystore init: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(format!("{}\n", passphrase).as_bytes())
                .map_err(|e| format!("Failed to write passphrase to lair init: {}", e))?;
        }

        let status = child
            .wait()
            .map_err(|e| format!("Failed to wait for lair init: {}", e))?;
        if !status.success() {
            return Err(format!(
                "lair-keystore init failed (status {}): {}",
                status,
                read_lair_logs(lair_dir, "init"),
            ));
        }
        log::info!("Lair-keystore initialized successfully");
    }

    // Read connection URL from config file.
    let connection_url = read_connection_url(&config_path)?;

    // Clean up stale socket file from a previous run. Unix only — Windows
    // named pipes live in the kernel namespace, not on disk, so there's
    // nothing to remove.
    #[cfg(unix)]
    {
        let socket_path = lair_dir.join("socket");
        if socket_path.exists() {
            log::info!("Removing stale lair socket: {:?}", socket_path);
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    // Start the lair server. Redirect stdio to disk for the same reason as
    // init above — without this, a server-side crash is invisible.
    log::info!("Starting lair-keystore server...");
    let server_stdout = open_log_file(lair_dir, "lair-server-stdout.log")?;
    let server_stderr = open_log_file(lair_dir, "lair-server-stderr.log")?;
    let mut child = Command::new(sidecar_path("proofpoll-lair-keystore"))
        .arg("server")
        .arg("--piped")
        .current_dir(lair_dir)
        .stdin(Stdio::piped())
        .stdout(server_stdout)
        .stderr(server_stderr)
        .tie_to_parent()
        .spawn_hidden()
        .map_err(|e| format!("Failed to spawn lair-keystore server: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(format!("{}\n", passphrase).as_bytes())
            .map_err(|e| format!("Failed to write passphrase to lair server: {}", e))?;
    }

    log::info!("Lair-keystore server started (pid {})", child.id());
    Ok((child, connection_url))
}

fn open_log_file(lair_dir: &Path, name: &str) -> Result<std::fs::File, String> {
    let path = lair_dir.join(name);
    std::fs::File::create(&path)
        .map_err(|e| format!("Failed to create lair log {:?}: {}", path, e))
}

/// Read the lair-keystore log files (stderr preferred, stdout as fallback)
/// for the given stage (`"init"` or `"server"`). Output is trimmed and
/// truncated so callers can splice it directly into an error message
/// without blowing it out.
pub fn read_lair_logs(lair_dir: &Path, stage: &str) -> String {
    let stderr_path = lair_dir.join(format!("lair-{}-stderr.log", stage));
    let stdout_path = lair_dir.join(format!("lair-{}-stdout.log", stage));

    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();

    let output = if !stderr.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    let trimmed = output.trim();
    if trimmed.is_empty() {
        "(no output captured)".to_string()
    } else if trimmed.len() > 500 {
        format!("{}...", &trimmed[..500])
    } else {
        trimmed.to_string()
    }
}

/// Read the connection URL from lair's config file.
fn read_connection_url(config_path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| format!("Failed to read lair config at {:?}: {}", config_path, e))?;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("connectionUrl:") {
            let url = line
                .strip_prefix("connectionUrl:")
                .unwrap()
                .trim()
                .to_string();
            return Ok(url);
        }
    }

    Err(format!(
        "No connectionUrl found in lair config: {:?}",
        config_path
    ))
}

/// Connect to a running lair-keystore via its connection URL.
///
/// Single attempt — see `connect_to_lair_with_retry` in `conductor.rs` for
/// the retrying wrapper that's actually used during startup.
pub async fn connect_to_lair(
    connection_url: &str,
    passphrase: &str,
) -> Result<LairClient, String> {
    let url = lair_keystore_api::dependencies::url::Url::parse(connection_url)
        .map_err(|e| format!("Invalid lair connection URL: {}", e))?;
    let passphrase_array: SharedLockedArray = Arc::new(std::sync::Mutex::new(
        lair_keystore_api::dependencies::sodoken::LockedArray::from(
            passphrase.as_bytes().to_vec(),
        ),
    ));
    lair_keystore_api::ipc_keystore_connect(url, passphrase_array)
        .await
        .map_err(|e| format!("Failed to connect to lair: {}", e))
}

/// Wait for the lair connection to be ready.
///
/// On Unix, polls until the socket file exists.
///
/// On Windows, returns immediately — the named pipe lives in the kernel
/// namespace with no on-disk counterpart to poll, so readiness is detected
/// by the connect-with-retry loop in `conductor.rs` instead.
pub async fn wait_for_lair_socket(connection_url: &str, timeout_secs: u64) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        return Ok(());
    }

    let url = lair_keystore_api::dependencies::url::Url::parse(connection_url)
        .map_err(|e| format!("Invalid connection URL: {}", e))?;
    // url.path() returns percent-encoded path (e.g. %20 for spaces).
    // Decode it so we match the actual filesystem path — without this, macOS
    // paths under "Application Support" fail because we'd poll for a literal
    // "Application%20Support" directory.
    let decoded_path = percent_decode_str(url.path()).decode_utf8_lossy();
    let socket_path = std::path::PathBuf::from(decoded_path.as_ref());

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    while std::time::Instant::now() < deadline {
        if socket_path.exists() {
            log::info!("Lair socket ready at {:?}", socket_path);
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    Err(format!(
        "Lair socket not ready after {}s: {:?}",
        timeout_secs, socket_path
    ))
}
