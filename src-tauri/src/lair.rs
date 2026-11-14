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
use std::path::Path;
use std::sync::Arc;

/// Make `lair-keystore-config.yaml` point at the directory it now lives in.
///
/// Lair writes three absolute paths at init: `connectionUrl`
/// (`unix://<dir>/socket?k=...`), `pidFile` and `storeFile`. The URL's path
/// is PERCENT-ENCODED (`Application%20Support` on every Mac), so replacing
/// the old directory as plain text fixes the two file paths and silently
/// leaves the socket address behind. This parses the URL instead, compares
/// decoded paths, and lets the URL type do the encoding. Idempotent - safe
/// on every start, and it repairs a config left stale by an earlier move.
pub(crate) fn repoint_config(lair_dir: &Path) -> Result<bool, String> {
    let config_path = lair_dir.join("lair-keystore-config.yaml");
    if !config_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("lair config unreadable: {e}"))?;
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        if let Some(rest) = trimmed.strip_prefix("connectionUrl:") {
            let raw = rest.trim();
            if let Ok(mut url) = lair_keystore_api::dependencies::url::Url::parse(raw) {
                if url.scheme() == "unix" {
                    let decoded = percent_encoding::percent_decode_str(url.path())
                        .decode_utf8_lossy()
                        .to_string();
                    let expected = lair_dir.join("socket");
                    let decoded = Path::new(&decoded);
                    // A socket already moved to the short runtime path for
                    // THIS root (see `ensure_socket_fits`) is where it
                    // belongs; only a foreign address is pulled back here.
                    #[cfg(not(windows))]
                    let at_runtime_path = decoded == runtime_socket_path(lair_dir);
                    #[cfg(windows)]
                    let at_runtime_path = false;
                    if decoded != expected && !at_runtime_path {
                        url.set_path(&expected.to_string_lossy());
                        out.push(format!("{indent}connectionUrl: {url}"));
                        changed = true;
                        continue;
                    }
                }
            }
        } else {
            let mut handled = false;
            for (key, file) in [("pidFile:", "pid_file"), ("storeFile:", "store_file")] {
                if let Some(rest) = trimmed.strip_prefix(key) {
                    let expected = lair_dir.join(file);
                    if Path::new(rest.trim()) != expected {
                        out.push(format!("{indent}{key} {}", expected.display()));
                        changed = true;
                        handled = true;
                    }
                    break;
                }
            }
            if handled {
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !changed {
        return Ok(false);
    }
    let tmp = config_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, out.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &config_path).map_err(|e| e.to_string())?;
    log::info!("[lair] key store config repointed to {:?}", lair_dir);
    Ok(true)
}

/// The short runtime socket for the key store under `lair_dir`. The 0.6
/// store (`lair/`) keeps the name derived from its profile root, which
/// installs from before the 0.7 line already point at; every other store
/// (`lair-07/`) is named after its own directory. Two stores under one root
/// must never share a socket: the second removes the first's "stale" file
/// and binds in its place, and a conductor then shakes hands with a key
/// store of the other version (found on the first macOS install of 0.5.0).
#[cfg(not(windows))]
pub(crate) fn runtime_socket_path(lair_dir: &Path) -> std::path::PathBuf {
    let legacy = lair_dir.file_name().map(|n| n == "lair").unwrap_or(false);
    if legacy {
        crate::profiles::short_socket_path(lair_dir.parent().unwrap_or(lair_dir))
    } else {
        crate::profiles::short_socket_path(lair_dir)
    }
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
    match tokio::time::timeout(CONNECT_TIMEOUT, lair_keystore_api::ipc_keystore_connect(url, passphrase_array)).await {
        Ok(r) => r.map_err(|e| format!("Failed to connect to lair: {}", e)),
        Err(_) => Err(format!("Lair did not answer within {}s", CONNECT_TIMEOUT.as_secs())),
    }
}

/// How long one connection attempt may take: a connect that never answers
/// used to hold the start forever.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
fn read_connection_url(config_path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(config_path).map_err(|e| format!("Failed to read lair config at {:?}: {}", config_path, e))?;
    content
        .lines()
        .find_map(|l| l.trim().strip_prefix("connectionUrl:").map(|u| u.trim().to_string()))
        .ok_or_else(|| format!("No connectionUrl found in lair config: {:?}", config_path))
}

#[cfg(test)]
mod tests {

    /// The config lair writes on a Mac: the socket address is a URL, so the
    /// space in "Application Support" is `%20` there and a plain space in
    /// the two file paths.
    fn mac_style_config(dir: &Path) -> String {
        let url_path = dir.to_string_lossy().replace(' ', "%20");
        format!(
            "---\nconnectionUrl: unix://{url_path}/socket?k=AbC-123_x\npidFile: {0}/pid_file\nstoreFile: {0}/store_file\nsignatureFallback: none\n",
            dir.display()
        )
    }

    #[test]
    #[cfg(not(windows))]
    fn two_key_stores_under_one_root_get_different_runtime_sockets() {
        let root = Path::new("/Users/someone/Library/Application Support/com.proofpoll.app/profiles/0123456789abcdef");
        let legacy = super::runtime_socket_path(&root.join("lair"));
        let line_07 = super::runtime_socket_path(&root.join("lair-07"));
        assert_ne!(legacy, line_07);
        // The 0.6 store keeps the name existing installs already point at.
        assert_eq!(legacy, crate::profiles::short_socket_path(root));
        assert_eq!(line_07, crate::profiles::short_socket_path(&root.join("lair-07")));
    }

    // Lair's unix:// socket addresses (macOS and Linux); a Windows named
    // pipe does not move with the folder.
    #[cfg(unix)]
    #[test]
    fn a_moved_key_store_is_repointed_when_its_path_has_a_space() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Application Support").join("app").join("lair");
        let new = tmp.path().join("Application Support").join("app").join("profiles").join("local").join("lair");
        std::fs::create_dir_all(&new).unwrap();
        // Moved intact: the config still names the old directory everywhere.
        std::fs::write(new.join("lair-keystore-config.yaml"), mac_style_config(&old)).unwrap();

        assert_eq!(repoint_config(&new), Ok(true));
        let url = read_connection_url(&new.join("lair-keystore-config.yaml")).unwrap();
        let parsed = lair_keystore_api::dependencies::url::Url::parse(&url).unwrap();
        let decoded = percent_encoding::percent_decode_str(parsed.path()).decode_utf8_lossy().to_string();
        assert_eq!(Path::new(&decoded), new.join("socket"), "the socket address follows the move");
        assert!(url.contains("Application%20Support"), "and stays a valid encoded URL: {url}");
        assert!(url.ends_with("?k=AbC-123_x"), "the key in the address is kept: {url}");
        let after = std::fs::read_to_string(new.join("lair-keystore-config.yaml")).unwrap();
        assert!(after.contains(&format!("pidFile: {}", new.join("pid_file").display())));
        assert!(after.contains(&format!("storeFile: {}", new.join("store_file").display())));
        assert!(after.contains("signatureFallback: none"), "other settings untouched");
        // Idempotent: a second start changes nothing.
        assert_eq!(repoint_config(&new), Ok(false));
    }

    // Lair's unix:// socket addresses (macOS and Linux); a Windows named
    // pipe does not move with the folder.
    #[cfg(unix)]
    #[test]
    fn a_config_left_half_fixed_by_a_text_replace_is_healed() {
        // What 0.7.3-beta.6 left behind on a Mac: the two file paths moved,
        // the encoded socket address did not.
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("Application Support").join("app").join("lair");
        let new = tmp.path().join("Application Support").join("app").join("profiles").join("local").join("lair");
        std::fs::create_dir_all(&new).unwrap();
        let half = mac_style_config(&old).replace(
            &old.to_string_lossy().to_string(),
            &new.to_string_lossy().to_string(),
        );
        assert!(half.contains(&format!("pidFile: {}", new.join("pid_file").display())));
        assert!(half.contains("app/lair/socket"), "precondition: the socket address is still the old one");
        std::fs::write(new.join("lair-keystore-config.yaml"), half).unwrap();

        assert_eq!(repoint_config(&new), Ok(true));
        let url = read_connection_url(&new.join("lair-keystore-config.yaml")).unwrap();
        assert!(url.contains("profiles/local/lair/socket"), "{url}");
    }

    // Lair's unix:// socket addresses (macOS and Linux); a Windows named
    // pipe does not move with the folder.
    #[cfg(unix)]
    #[test]
    fn a_config_already_in_place_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Application Support").join("lair");
        std::fs::create_dir_all(&dir).unwrap();
        let original = mac_style_config(&dir);
        std::fs::write(dir.join("lair-keystore-config.yaml"), &original).unwrap();
        assert_eq!(repoint_config(&dir), Ok(false));
        assert_eq!(std::fs::read_to_string(dir.join("lair-keystore-config.yaml")).unwrap(), original);
    }


    use super::*;

    /// The scratch-lair harness: spins the real bundled lair binary against
    /// a temp dir. Probes the exact seam behind "failed to save draft" -
    /// encrypted entries call lair's crypto_box on the AGENT seed, and since
    /// fresh installs derive their agent from an IMPORTED seed, that path
    /// must behave identically to a lair-generated one.
    #[tokio::test(flavor = "multi_thread")]
    async fn imported_seed_crypto_box_matches_generated_seed() {
        // Needs the real bundled lair binary: CI has placeholders.
        let real = std::process::Command::new(crate::sidecar::sidecar_path("proofpoll-lair-keystore"))
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("lair_keystore"))
            .unwrap_or(false);
        if !real {
            eprintln!("skipped: no real lair-keystore binary here");
            return;
        }
        let dir = std::env::temp_dir().join(format!("pp-lair-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let passphrase = "test-passphrase-0123456789";

        let plan = crate::conductor::socket_plan();
        let (mut child, url) =
            flowsta_hc_lines::keystore::start(&dir, &crate::sidecar::sidecar_path("proofpoll-lair-keystore"), passphrase, &plan).expect("lair start");
        let result = async {
            flowsta_hc_lines::keystore::wait_ready(&url, std::time::Duration::from_secs(15), &mut child, &dir).await.map_err(|e| e.to_string())?;
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

#[cfg(all(test, not(windows)))]
mod socket_tests {

    #[test]
    fn a_profile_fits_when_only_the_runtime_socket_does() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("y".repeat(110));
        assert!(!crate::profiles::in_root_socket_fits(&root));
        assert!(crate::profiles::lair_socket_path_fits(&root), "the short socket carries it");
    }
}


// ── The 0.7 lair (lair_keystore_api 0.7.1) ────────────────────────────

/// Connect to a lair 0.7.1 server with the 0.7.1 client.
pub async fn connect_to_lair_07(
    connection_url: &str,
    passphrase: &str,
) -> Result<lair_keystore_api_07::prelude::LairClient, String> {
    let url = lair_keystore_api_07::dependencies::url::Url::parse(connection_url)
        .map_err(|e| format!("Invalid lair connection URL: {}", e))?;
    let passphrase_array: lair_keystore_api_07::prelude::SharedLockedArray = Arc::new(std::sync::Mutex::new(
        lair_keystore_api_07::dependencies::sodoken::LockedArray::from(
            passphrase.as_bytes().to_vec(),
        ),
    ));
    match tokio::time::timeout(CONNECT_TIMEOUT, lair_keystore_api_07::ipc_keystore_connect(url, passphrase_array)).await {
        Ok(r) => r.map_err(|e| format!("Failed to connect to lair (0.7): {}", e)),
        Err(_) => Err(format!("Lair (0.7) did not answer within {}s", CONNECT_TIMEOUT.as_secs())),
    }
}

/// Import the same seed into the 0.7 lair under the same tag (so the agent
/// key is identical on both lines). Returns the Ed25519 public key.
pub async fn import_seed_to_lair_07(
    client: &lair_keystore_api_07::prelude::LairClient,
    seed_bytes: &[u8; 32],
    tag: &str,
) -> Result<[u8; 32], String> {
    use lair_keystore_api_07::dependencies::sodoken;
    use lair_keystore_api_07::prelude::{BinDataSized, LairEntryInfo};
    if let Ok(LairEntryInfo::Seed { seed_info, .. }) = client.get_entry(tag.into()).await {
        log::info!("Seed '{}' already exists in lair (0.7), reusing", tag);
        return Ok(*seed_info.ed25519_pub_key.0);
    }
    let helper = match client.new_seed("_import_helper".into(), None, false).await {
        Ok(h) => h,
        Err(_) => match client.get_entry("_import_helper".into()).await {
            Ok(LairEntryInfo::Seed { seed_info, .. }) => seed_info,
            _ => return Err("Failed to create or fetch _import_helper seed (0.7)".into()),
        },
    };
    let mut sender_pub = [0u8; sodoken::crypto_box::XSALSA_PUBLICKEYBYTES];
    let mut sender_sec =
        sodoken::SizedLockedArray::<{ sodoken::crypto_box::XSALSA_SECRETKEYBYTES }>::new()
            .map_err(|e| e.to_string())?;
    sodoken::crypto_box::xsalsa_keypair(&mut sender_pub, &mut *sender_sec.lock())
        .map_err(|e| e.to_string())?;
    let mut nonce = [0u8; sodoken::crypto_box::XSALSA_NONCEBYTES];
    sodoken::random::randombytes_buf(&mut nonce).map_err(|e| e.to_string())?;
    let recipient_pub_bytes: &[u8; 32] = &*helper.x25519_pub_key.0;
    let mut cipher = vec![0u8; seed_bytes.len() + sodoken::crypto_box::XSALSA_MACBYTES];
    sodoken::crypto_box::xsalsa_easy(
        &mut cipher,
        seed_bytes,
        &nonce,
        recipient_pub_bytes,
        &*sender_sec.lock(),
    )
    .map_err(|e| e.to_string())?;
    let sender_pub_key = BinDataSized(Arc::new(sender_pub));
    let info = client
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
        .map_err(|e| format!("seed import into lair (0.7) failed: {}", e))?;
    Ok(*info.ed25519_pub_key.0)
}
