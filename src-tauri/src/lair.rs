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

        // Wipe any stale state files left over from a previous install.
        // Windows uninstallers commonly clear the config file in AppData
        // but leave the encrypted `store_file` behind — the next install
        // generates a fresh random passphrase and a fresh config, then
        // lair-server tries to open the orphaned store_file with the new
        // passphrase and crashes with `sqlcipher_page_cipher: hmac check
        // failed`. Removing the known lair-managed files before init
        // guarantees we're truly starting from a clean slate.
        for name in &["store_file", "pid_file", "socket"] {
            let p = lair_dir.join(name);
            if p.exists() {
                log::warn!(
                    "Removing stale lair file from a previous install: {:?}",
                    p,
                );
                if let Err(e) = std::fs::remove_file(&p) {
                    log::error!(
                        "FAILED to remove stale lair file {:?}: {} — lair-server will likely crash with an hmac error",
                        p, e,
                    );
                }
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The scratch-lair harness: spins the real bundled lair binary against
    /// a temp dir. Probes the exact seam behind "failed to save draft" -
    /// encrypted entries call lair's crypto_box on the AGENT seed, and since
    /// fresh installs derive their agent from an IMPORTED seed, that path
    /// must behave identically to a lair-generated one.
    #[tokio::test(flavor = "multi_thread")]
    async fn imported_seed_crypto_box_matches_generated_seed() {
        let dir = std::env::temp_dir().join(format!("pp-lair-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let passphrase = "test-passphrase-0123456789";

        let (mut child, url) =
            start_lair_process(&dir, passphrase).expect("lair start");
        let result = async {
            wait_for_lair_socket(&url, 15).await?;
            // The server accepts connections a beat after the socket exists.
            let mut client = None;
            for _ in 0..20 {
                match connect_to_lair(&url, passphrase).await {
                    Ok(c) => {
                        client = Some(c);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                }
            }
            let client = client.ok_or("could not connect to scratch lair")?;

            // Baseline: a lair-generated seed (the pre-escrow agent model).
            let generated = client
                .new_seed("generated-agent".into(), None, false)
                .await
                .map_err(|e| format!("new_seed: {}", e))?;
            let gen_pub: [u8; 32] = *generated.ed25519_pub_key.0;
            let (n1, c1) = crate::crypto::encrypt_to_self(&client, gen_pub, b"baseline")
                .await
                .map_err(|e| format!("generated-seed encrypt: {}", e))?;
            let p1 = crate::crypto::decrypt_from_self(&client, gen_pub, n1, &c1)
                .await
                .map_err(|e| format!("generated-seed decrypt: {}", e))?;
            if p1 != b"baseline" {
                return Err("generated-seed roundtrip mismatch".to_string());
            }

            // The escrow model: an app-side seed IMPORTED into lair - what
            // every fresh install's agent is since the seed escrow landed.
            let seed = [7u8; 32];
            let imported = import_seed_to_lair(&client, &seed, "proofpoll-device-seed")
                .await
                .map_err(|e| format!("import_seed: {}", e))?;
            let imp_pub: [u8; 32] = *imported.ed25519_pub_key.0;
            let (n2, c2) = crate::crypto::encrypt_to_self(&client, imp_pub, b"draft")
                .await
                .map_err(|e| format!("imported-seed encrypt: {}", e))?;
            let p2 = crate::crypto::decrypt_from_self(&client, imp_pub, n2, &c2)
                .await
                .map_err(|e| format!("imported-seed decrypt: {}", e))?;
            if p2 != b"draft" {
                return Err("imported-seed roundtrip mismatch".to_string());
            }
            Ok::<(), String>(())
        }
        .await;

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        result.expect("scratch-lair crypto probe");
    }
}

/// Import a 32-byte seed into a running lair keystore (ported from the
/// Flowsta Vault, which installs its conductor agent from an app-held seed
/// the same way). Lair's `import_seed` API requires the payload sealed via
/// x25519 crypto_box to a key lair controls; this wraps that transparently.
/// Idempotent: an existing entry under `tag` is returned as-is.
pub async fn import_seed_to_lair(
    client: &LairClient,
    seed_bytes: &[u8; 32],
    tag: &str,
) -> LairResult<SeedInfo> {
    use lair_keystore_api::dependencies::sodoken;

    match client.get_entry(tag.into()).await {
        Ok(LairEntryInfo::Seed { seed_info, .. }) => {
            log::info!("Seed '{}' already exists in lair, reusing", tag);
            return Ok(seed_info);
        }
        _ => {}
    }

    let helper = match client.new_seed("_import_helper".into(), None, false).await {
        Ok(h) => h,
        Err(_) => match client.get_entry("_import_helper".into()).await {
            Ok(LairEntryInfo::Seed { seed_info, .. }) => seed_info,
            _ => {
                return Err(lair_keystore_api::dependencies::one_err::OneErr::new(
                    "Failed to create or fetch _import_helper seed",
                ))
            }
        },
    };

    let mut sender_pub = [0u8; sodoken::crypto_box::XSALSA_PUBLICKEYBYTES];
    let mut sender_sec =
        sodoken::SizedLockedArray::<{ sodoken::crypto_box::XSALSA_SECRETKEYBYTES }>::new()?;
    sodoken::crypto_box::xsalsa_keypair(&mut sender_pub, &mut *sender_sec.lock())?;

    let mut nonce = [0u8; sodoken::crypto_box::XSALSA_NONCEBYTES];
    sodoken::random::randombytes_buf(&mut nonce)?;

    let recipient_pub_bytes: &[u8; 32] = &*helper.x25519_pub_key.0;
    let mut cipher = vec![0u8; seed_bytes.len() + sodoken::crypto_box::XSALSA_MACBYTES];
    sodoken::crypto_box::xsalsa_easy(
        &mut cipher,
        seed_bytes,
        &nonce,
        recipient_pub_bytes,
        &*sender_sec.lock(),
    )?;

    let sender_pub_key = BinDataSized(Arc::new(sender_pub));
    client
        .import_seed(
            sender_pub_key,
            helper.x25519_pub_key,
            None,
            nonce,
            cipher.into(),
            tag.into(),
            false,
        )
        .await
}
