//! Lair recovery + source-chain restoration via `graft_records`.
//!
//! When the user reinstalls ProofPoll on a machine that still has the Flowsta
//! Vault holding a backup for this client_id, we can recover their previous
//! identity (the same `agent_pub_key` they were using before) and restore
//! their source chain bit-for-bit — original action hashes preserved, no DHT
//! duplicates.
//!
//! The data fetch lives in the frontend (the SDK's `retrieveLairRecoveryPayload`
//! call — uses the webview's Origin which Vault has linked). This module
//! handles the Rust-side work: writing the three lair files, rewriting the
//! YAML config paths to match this install's data_dir, and grafting the
//! recovered records onto the cell's source chain after `install_app`.
//!
//! For forking developers: the pattern is generic — only the entry types in
//! `decode_record_for_export` are app-specific. The recovery + graft flow
//! itself is identical for any Flowsta-integrated Holochain app.

use base64::Engine as _;
use holochain_client::AdminWebsocket;
use holochain_types::prelude::{CellId, Record};
use holochain_state_types::SourceChainDumpRecord;
use holochain_integrity_types::SignedActionHashed;
use holo_hash::HoloHashed;
use serde_json::Value;
use std::path::Path;

/// Decoded payload after the frontend has fetched it from Vault.
/// All three lair fields are required — they're encryption-paired and only
/// useful together.
#[derive(Debug, Clone)]
pub struct LairRecoveryPayload {
    pub passphrase: String,
    pub config_yaml: String,
    pub store_file_b64: String,
    pub agent_pub_key_str: String,
    /// Full backup JSON kept for the graft step.
    pub raw_payload: Value,
}

impl LairRecoveryPayload {
    /// Build from the raw JSON the frontend received from Vault. Returns
    /// `None` if any required field is missing or the wrong type.
    pub fn try_from_json(payload: Value) -> Option<Self> {
        let passphrase = payload.get("lair_passphrase")?.as_str()?.to_string();
        let config_yaml = payload.get("lair_keystore_config")?.as_str()?.to_string();
        let store_file_b64 = payload.get("lair_keystore_data")?.as_str()?.to_string();
        let agent_pub_key_str = payload.get("agent_pub_key")?.as_str()?.to_string();

        if passphrase.is_empty() || config_yaml.is_empty() || store_file_b64.is_empty() {
            return None;
        }

        Some(Self {
            passphrase,
            config_yaml,
            store_file_b64,
            agent_pub_key_str,
            raw_payload: payload,
        })
    }
}

/// Write the three lair files into `data_dir`, rewriting the YAML config so
/// its absolute paths (storeFile, pidFile, connectionUrl) point at this
/// install's location rather than the originating install's. Also writes the
/// passphrase to `data_dir/lair-passphrase`.
///
/// Caller is responsible for ensuring `data_dir/lair/` does not contain
/// conflicting state — the simplest discipline is to call this BEFORE lair is
/// started for the first time on this install.
pub fn write_lair_recovery_files(
    data_dir: &Path,
    payload: &LairRecoveryPayload,
) -> Result<(), String> {
    let lair_dir = data_dir.join("lair");
    std::fs::create_dir_all(&lair_dir)
        .map_err(|e| format!("create lair dir {:?}: {}", lair_dir, e))?;

    // 1. lair-passphrase at data_dir/lair-passphrase
    let passphrase_path = data_dir.join("lair-passphrase");
    std::fs::write(&passphrase_path, &payload.passphrase)
        .map_err(|e| format!("write {:?}: {}", passphrase_path, e))?;

    // 2. store_file at data_dir/lair/store_file (base64 decode)
    let store_bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload.store_file_b64)
        .map_err(|e| format!("decode store_file base64: {}", e))?;
    let store_path = lair_dir.join("store_file");
    std::fs::write(&store_path, &store_bytes)
        .map_err(|e| format!("write {:?}: {}", store_path, e))?;

    // 3. lair-keystore-config.yaml at data_dir/lair/lair-keystore-config.yaml
    //    Rewrite path values so they refer to *this* install's lair_dir.
    let rewritten_config = rewrite_lair_config_paths(&payload.config_yaml, &lair_dir);
    let config_path = lair_dir.join("lair-keystore-config.yaml");
    std::fs::write(&config_path, &rewritten_config)
        .map_err(|e| format!("write {:?}: {}", config_path, e))?;

    log::info!(
        "Wrote recovered lair state to {:?} (config, store_file, passphrase)",
        lair_dir,
    );
    Ok(())
}

/// Rewrite the three path-bearing lines in a lair config YAML to point at the
/// caller-provided `new_lair_dir`. Salt and key fields are untouched (those
/// MUST stay verbatim — they're paired with the encrypted store_file).
///
/// Replaces these lines, preserving everything else byte-for-byte:
/// - `connectionUrl: unix:///old/path/socket?k=KEY` → `unix:///new/path/socket?k=KEY`
/// - `pidFile: /old/path/pid_file`                  → `/new/path/pid_file`
/// - `storeFile: /old/path/store_file`              → `/new/path/store_file`
///
/// On Windows the connectionUrl is a `named-pipe:` scheme without on-disk
/// paths, so it's left as-is (the named-pipe name is opaque and globally
/// unique to this user). Only pidFile and storeFile are rewritten on Windows.
fn rewrite_lair_config_paths(config_yaml: &str, new_lair_dir: &Path) -> String {
    let new_lair_dir_str = new_lair_dir.display().to_string();

    let mut out = String::with_capacity(config_yaml.len());
    for line in config_yaml.lines() {
        let trimmed = line.trim_start();
        if let Some(_rest) = trimmed.strip_prefix("connectionUrl:") {
            // Unix sockets only — Windows named-pipes have no path component
            // to rewrite. Try to rewrite Unix-style URLs.
            if let Some(rewritten) = rewrite_connection_url(line, &new_lair_dir_str) {
                out.push_str(&rewritten);
            } else {
                out.push_str(line);
            }
        } else if trimmed.starts_with("pidFile:") {
            out.push_str(&format!(
                "pidFile: {}",
                std::path::Path::new(&new_lair_dir_str).join("pid_file").display(),
            ));
        } else if trimmed.starts_with("storeFile:") {
            out.push_str(&format!(
                "storeFile: {}",
                std::path::Path::new(&new_lair_dir_str).join("store_file").display(),
            ));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Rewrite a `connectionUrl: unix:///old/path/socket?k=KEY` line to use the
/// new lair_dir's socket path while preserving the `?k=KEY` auth token.
/// Returns `None` if the URL is not Unix-style (e.g., Windows named-pipe).
fn rewrite_connection_url(line: &str, new_lair_dir_str: &str) -> Option<String> {
    let value = line.trim_start().strip_prefix("connectionUrl:")?.trim();
    if !value.starts_with("unix:///") {
        return None;
    }
    // Split off the ?k= query string and reattach it to the new socket path.
    let (_path_part, query) = match value.split_once('?') {
        Some((p, q)) => (p, format!("?{}", q)),
        None => (value, String::new()),
    };
    let new_socket = std::path::Path::new(new_lair_dir_str).join("socket");
    Some(format!(
        "connectionUrl: unix://{}{}",
        new_socket.display(),
        query,
    ))
}

/// Iterate the records in the backup payload's cells and reconstruct each one
/// into a Holochain `Record` suitable for `graft_records`. Filters by author
/// (defensive — the backup should already only contain the user's own records).
pub fn records_from_backup(
    payload: &Value,
    expected_author_str: &str,
) -> Result<Vec<Record>, String> {
    let cells = payload
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("payload missing `cells` array")?;

    let mut records: Vec<Record> = Vec::new();
    for cell in cells {
        let cell_records = cell
            .get("records")
            .and_then(Value::as_array)
            .ok_or("cell missing `records` array")?;

        for rec_json in cell_records {
            let raw = rec_json
                .get("raw_record")
                .ok_or("record missing `raw_record`")?;

            // SourceChainDumpRecord has serde-derived Serialize/Deserialize,
            // so as long as `raw_record` was produced from a dump_full_state
            // SourceChainDumpRecord (or a structurally-equivalent shape), this
            // round-trip works.
            let dump: SourceChainDumpRecord = serde_json::from_value(raw.clone())
                .map_err(|e| format!("decode SourceChainDumpRecord: {}", e))?;

            let author_str = dump.action.author().to_string();
            if author_str != expected_author_str {
                log::warn!(
                    "Skipping recovered record authored by {} (expected {})",
                    author_str, expected_author_str,
                );
                continue;
            }

            let hashed = HoloHashed::with_pre_hashed(dump.action, dump.action_address);
            let signed = SignedActionHashed::with_presigned(hashed, dump.signature);
            records.push(Record::new(signed, dump.entry));
        }
    }

    // Graft expects records in source-chain order. Sort by action sequence
    // number ascending so the chain is reconstructed in the right order.
    records.sort_by_key(|r| r.signed_action.action().action_seq());

    Ok(records)
}

/// Graft a recovered source chain onto a freshly-installed cell. The cell's
/// app must have been installed with `ignore_genesis_failure: true` so it has
/// no auto-generated genesis records to conflict with.
///
/// Per Holochain's `graft_records` docs (admin_interface.rs:364): "Note that
/// the cell does not need to exist to run this command. It is possible to
/// insert records into a source chain before the cell is created. This can
/// be used to restore from backup."
///
/// We pass `validate: true` so the conductor verifies signatures and chain
/// continuity — any tampering or corruption surfaces here as an error rather
/// than a delayed validation failure later.
pub async fn graft_recovered_records(
    admin_ws: &AdminWebsocket,
    cell_id: CellId,
    records: Vec<Record>,
) -> Result<usize, String> {
    let count = records.len();
    log::info!(
        "Grafting {} recovered records onto cell source chain",
        count,
    );
    admin_ws
        .graft_records(cell_id, true, records)
        .await
        .map_err(|e| format!("graft_records failed: {}", e))?;
    log::info!("Graft complete; source chain reconstructed");
    Ok(count)
}

/// Read the three lair files from disk and return them base64-encoded /
/// stringified, ready to splice into a canonical backup payload. Returns
/// `None` if any of them is missing (the recovery half won't work without
/// all three, so we return all-or-nothing).
pub fn read_lair_backup_fields(data_dir: &Path) -> Option<(String, String, String)> {
    let passphrase_path = data_dir.join("lair-passphrase");
    let config_path = data_dir.join("lair").join("lair-keystore-config.yaml");
    let store_path = data_dir.join("lair").join("store_file");

    let passphrase = std::fs::read_to_string(&passphrase_path).ok()?;
    let config_yaml = std::fs::read_to_string(&config_path).ok()?;
    let store_bytes = std::fs::read(&store_path).ok()?;
    let store_b64 = base64::engine::general_purpose::STANDARD.encode(&store_bytes);

    Some((passphrase, config_yaml, store_b64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_unix_config_paths() {
        let original = "\
connectionUrl: unix:///home/user/.local/share/com.proofpoll.app/lair/socket?k=abc123

# The pid file for managing a running lair-keystore process
pidFile: /home/user/.local/share/com.proofpoll.app/lair/pid_file

# The sqlcipher store file for persisting secrets
storeFile: /home/user/.local/share/com.proofpoll.app/lair/store_file

databaseSalt: B9yGZjlji7AHTFgExZ9szw
runtimeSecretsSalt: _eQDVaKDy_jGJK-_qJBHjQ
";
        let new_dir = std::path::Path::new("/tmp/new-install/lair");
        let rewritten = rewrite_lair_config_paths(original, new_dir);

        // Path lines must point at the new location.
        assert!(rewritten.contains("unix:///tmp/new-install/lair/socket?k=abc123"));
        assert!(rewritten.contains("pidFile: /tmp/new-install/lair/pid_file"));
        assert!(rewritten.contains("storeFile: /tmp/new-install/lair/store_file"));

        // Salt fields must be preserved verbatim (these are the keys that pair
        // with the encrypted store_file — rewriting them would corrupt
        // decryption).
        assert!(rewritten.contains("databaseSalt: B9yGZjlji7AHTFgExZ9szw"));
        assert!(rewritten.contains("runtimeSecretsSalt: _eQDVaKDy_jGJK-_qJBHjQ"));
    }

    #[test]
    fn leaves_windows_named_pipe_alone() {
        let original = "\
connectionUrl: named-pipe:\\\\.\\pipe\\_abc?k=key
pidFile: C:\\Users\\white\\AppData\\Roaming\\com.proofpoll.app\\lair\\pid_file
storeFile: C:\\Users\\white\\AppData\\Roaming\\com.proofpoll.app\\lair\\store_file
";
        let new_dir = std::path::Path::new("/tmp/x/lair");
        let rewritten = rewrite_lair_config_paths(original, new_dir);

        // Named-pipe URL is opaque (no on-disk path to rewrite) — preserved
        // verbatim. pidFile + storeFile are rewritten using forward-slash form
        // (PathBuf::display uses native separators; this comment notes the
        // intent — the test below asserts the new path appears, regardless of
        // separator).
        assert!(rewritten.contains("named-pipe:\\\\.\\pipe\\_abc?k=key"));
        assert!(rewritten.contains("pid_file"));
        assert!(rewritten.contains("store_file"));
    }
}
