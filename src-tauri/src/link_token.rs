//! The link token of the open profile.
//!
//! When the person links ProofPoll in their Flowsta Vault, the Vault gives
//! the link a private token. From then on the token is what tells this
//! ProofPoll apart from any other app that presents the same origin (every
//! Tauri app's webview does), and what proves that a later call, a
//! disconnect among them, comes from the app that made the link.
//!
//! The page makes the link, so the page receives the token and hands it
//! here (`save_vault_link_token`). It is kept in the profile's folder,
//! beside `identity-link.json`, because each profile has its own key and so
//! its own link. At each start the open profile's token is given to the
//! Vault client ([`open_profile`]) and the page reads it back for the
//! JavaScript SDK (`get_vault_link_token`), so both present the same one.
//!
//! A profile linked before link tokens has no file: nothing is sent, and
//! the Vault answers it as it always did.
//!
//! The token is a secret. It is never logged, here or in the page.

use flowsta_vault_client::vault::{Refused, VaultClient};
use std::path::{Path, PathBuf};

pub const LINK_TOKEN_FILE: &str = "vault-link-token.json";

/// What the page listens for: the Vault refused a call over this profile's
/// link (the token is no longer known there, or the link needs one this
/// profile does not hold). The cure is to sign in with Flowsta again.
pub(crate) const REFUSED_EVENT: &str = "vault-link-refused";

/// The file's contents. No `Debug`: a token must not reach a log by way of
/// a `{:?}`.
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    link_token: String,
}

pub fn link_token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LINK_TOKEN_FILE)
}

/// The profile's link token, or None when it has none (never linked, linked
/// before link tokens, or the file cannot be read: the app then works as an
/// app without a token, and signing in again writes a new one).
pub fn load(data_dir: &Path) -> Option<String> {
    let json = std::fs::read_to_string(link_token_path(data_dir)).ok()?;
    let stored: Stored = serde_json::from_str(&json).ok()?;
    Some(stored.link_token).filter(|t| !t.is_empty())
}

/// Keep `token` as the profile's link token, replacing any earlier one.
/// Written beside the old file and moved over it, so a failed write never
/// leaves half a token; readable by this user only where the system has
/// file modes.
pub fn save(data_dir: &Path, token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err("no link token to keep".into());
    }
    let path = link_token_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&Stored { link_token: token.to_string() }).map_err(|e| e.to_string())?;
    write_private(&tmp, body.as_bytes()).map_err(|e| format!("link token write failed: {}", e))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("link token rename failed: {}", e))
}

/// Forget the profile's link token. Nothing to forget is not an error.
pub fn delete(data_dir: &Path) {
    let _ = std::fs::remove_file(link_token_path(data_dir));
}

#[cfg(unix)]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(body)
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    // The profile folder is under the user's own app data, which other
    // accounts cannot read.
    std::fs::write(path, body)
}

// ── The open profile and the Vault client ─────────────────────────────

/// At start, once the profile is picked: its token (or none) is what the
/// Vault client presents from now on.
pub(crate) fn open_profile(vault: &VaultClient, data_dir: &Path) {
    vault.set_link_token(load(data_dir));
}

/// After a sign-in: keep the token the Vault gave for this profile's link
/// and present it from now on.
pub(crate) fn keep(vault: &VaultClient, data_dir: &Path, token: &str) -> Result<(), String> {
    save(data_dir, token)?;
    vault.set_link_token(Some(token.to_string()));
    Ok(())
}

/// The profile's link is gone (disconnected, or made again without a
/// token): forget the token, on disk and in the Vault client.
pub(crate) fn forget(vault: &VaultClient, data_dir: &Path) {
    delete(data_dir);
    vault.set_link_token(None);
}

/// Whether the Vault's `error` code says the call was refused over this
/// app's link token.
pub(crate) fn link_refusal(code: &str) -> Option<Refused> {
    Refused::from_code(code).filter(|r| matches!(r, Refused::InvalidLinkToken | Refused::LinkTokenRequired))
}

/// What to do when the Vault answered a call with `code`. A token the Vault
/// no longer knows is forgotten, unless a newer one has replaced the one
/// that was `presented` (the person signed in again meanwhile). Returns
/// true when the refusal was about the link token: the page then offers to
/// sign in again.
pub(crate) fn after_refusal(vault: &VaultClient, data_dir: &Path, code: &str, presented: Option<&str>) -> bool {
    match link_refusal(code) {
        Some(Refused::InvalidLinkToken) => {
            if presented.is_some() && vault.link_token().as_deref() == presented {
                forget(vault, data_dir);
            }
            true
        }
        Some(_) => true,
        None => false,
    }
}

/// [`after_refusal`] for the app's own Vault client and the open profile,
/// telling the page when the link token was the reason.
pub(crate) fn note_refusal(app: &tauri::AppHandle, data_dir: &Path, code: &str, presented: Option<&str>) {
    if after_refusal(crate::vault_probe::client(), data_dir, code, presented) {
        log::warn!("[vault] the Vault refused this profile's link ({}): signing in again makes a new one", code);
        let _ = tauri::Emitter::emit(app, REFUSED_EVENT, ());
    }
}

// ── Tauri commands ────────────────────────────────────────────────────

/// The open profile's link token, for the page to give the JavaScript SDK
/// (`setVaultLinkToken`). None: the profile has none.
#[tauri::command]
pub fn get_vault_link_token() -> Option<String> {
    crate::vault_probe::client().link_token()
}

/// The page signed in and the Vault gave the link this token.
#[tauri::command]
pub fn save_vault_link_token(state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>, token: String) -> Result<(), String> {
    keep(crate::vault_probe::client(), &state.data_dir, &token)
}

/// The page disconnected, or the link was made again without a token.
#[tauri::command]
pub fn clear_vault_link_token(state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>) {
    forget(crate::vault_probe::client(), &state.data_dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowsta_vault_client::vault::VaultConfig;

    fn vault() -> VaultClient {
        VaultClient::new(VaultConfig { origin: "tauri://localhost".into(), only_port: None, client_id: Some("flowsta_app_test".into()) }).unwrap()
    }

    #[test]
    fn a_token_round_trips_and_a_newer_one_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), None, "a profile linked before link tokens has no file");
        save(dir.path(), "tok-1").unwrap();
        assert_eq!(load(dir.path()).as_deref(), Some("tok-1"));
        save(dir.path(), "tok-2").unwrap();
        assert_eq!(load(dir.path()).as_deref(), Some("tok-2"));
        let left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, vec![LINK_TOKEN_FILE.to_string()], "no temporary file is left behind");
    }

    #[test]
    fn the_file_is_json_beside_the_identity_link() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), "tok-1").unwrap();
        let path = link_token_path(dir.path());
        assert_eq!(path.parent(), flowsta_vault_client::identity::identity_link_path(dir.path()).parent());
        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({ "link_token": "tok-1" }));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_this_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for token in ["tok-1", "tok-2"] {
            save(dir.path(), token).unwrap();
            let mode = std::fs::metadata(link_token_path(dir.path())).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_empty_token_is_never_kept_and_an_unreadable_file_is_no_token() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save(dir.path(), "").is_err());
        assert!(!link_token_path(dir.path()).exists());
        for body in ["", "not json", "{}", r#"{"link_token":""}"#, r#"{"link_token":7}"#] {
            std::fs::write(link_token_path(dir.path()), body).unwrap();
            assert_eq!(load(dir.path()), None, "{body:?}");
        }
        // Signing in again repairs it.
        save(dir.path(), "tok-1").unwrap();
        assert_eq!(load(dir.path()).as_deref(), Some("tok-1"));
    }

    #[test]
    fn deleting_forgets_the_token_and_can_be_repeated() {
        let dir = tempfile::tempdir().unwrap();
        delete(dir.path());
        save(dir.path(), "tok-1").unwrap();
        delete(dir.path());
        delete(dir.path());
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn the_vault_client_presents_the_open_profile_s_token() {
        let (with, without) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        save(with.path(), "tok-1").unwrap();
        let vault = vault();

        open_profile(&vault, with.path());
        assert_eq!(vault.link_token().as_deref(), Some("tok-1"));
        // Another profile, linked before link tokens: nothing is presented,
        // and nothing of the first profile's is carried over.
        open_profile(&vault, without.path());
        assert_eq!(vault.link_token(), None);
        assert_eq!(load(with.path()).as_deref(), Some("tok-1"), "each profile keeps its own");
    }

    #[test]
    fn keeping_and_forgetting_change_the_file_and_the_client_together() {
        let dir = tempfile::tempdir().unwrap();
        let vault = vault();
        keep(&vault, dir.path(), "tok-1").unwrap();
        assert_eq!((load(dir.path()), vault.link_token()), (Some("tok-1".to_string()), Some("tok-1".to_string())));
        assert!(keep(&vault, dir.path(), "").is_err());
        assert_eq!(vault.link_token().as_deref(), Some("tok-1"), "a token that could not be kept is not presented either");
        forget(&vault, dir.path());
        assert_eq!((load(dir.path()), vault.link_token()), (None, None));
    }

    #[test]
    fn a_refused_token_is_forgotten_unless_a_newer_one_replaced_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = vault();

        keep(&vault, dir.path(), "tok-1").unwrap();
        assert!(after_refusal(&vault, dir.path(), "invalid_link_token", Some("tok-1")));
        assert_eq!((load(dir.path()), vault.link_token()), (None, None));

        // The person signed in again while the refused call was under way.
        keep(&vault, dir.path(), "tok-2").unwrap();
        assert!(after_refusal(&vault, dir.path(), "invalid_link_token", Some("tok-1")));
        assert_eq!(load(dir.path()).as_deref(), Some("tok-2"));

        // The link needs a token and this profile holds none: nothing to
        // forget, and the page still offers the sign-in.
        forget(&vault, dir.path());
        assert!(after_refusal(&vault, dir.path(), "link_token_required", None));
    }

    #[test]
    fn other_answers_leave_the_token_alone() {
        let dir = tempfile::tempdir().unwrap();
        let vault = vault();
        keep(&vault, dir.path(), "tok-1").unwrap();
        for code in ["not_linked", "app_ambiguous", "origin_not_registered", "vault_locked", "vault unreachable: timed out", ""] {
            assert!(!after_refusal(&vault, dir.path(), code, Some("tok-1")), "{code}");
            assert_eq!(link_refusal(code), None);
        }
        assert_eq!(load(dir.path()).as_deref(), Some("tok-1"));
        assert_eq!(link_refusal("invalid_link_token"), Some(Refused::InvalidLinkToken));
        assert_eq!(link_refusal("link_token_required"), Some(Refused::LinkTokenRequired));
    }
}
