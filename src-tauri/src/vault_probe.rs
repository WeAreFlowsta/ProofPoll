//! This app's handle on the person's Flowsta Vault.
//!
//! Finding the right Vault (all three ports asked at once, another user
//! account's Vault ignored, the unlocked one preferred) is
//! `flowsta_vault_client::vault`; here are the app's own origin and client
//! id, its test switch, and the commands the webview calls.
//!
//! Every call this app makes to the Vault from Rust goes through
//! [`client`]: its `get` and `post` say which app the call is from (the
//! origin, the client id, and the open profile's link token once there is
//! one, see `link_token.rs`).

use flowsta_vault_client::vault::{self, VaultClient, VaultConfig};
use std::sync::OnceLock;
use std::time::Duration;

pub use flowsta_vault_client::vault::VaultProbe;
pub(crate) use flowsta_vault_client::vault::take_relaunch_marker;

/// The Origin header every request to the Vault carries. The Vault records
/// the origin the WEBVIEW presented when the person linked the app, and
/// accepts that link's token only from the same origin, so every Rust-side
/// request must present the same one: a bare reqwest carries none, and the
/// Vault then answers as to a stranger - `/status` without the display name
/// and picture (scope-gated per origin since Vault 1.2), other routes
/// refused. `tauri dev` serves the dev server's origin; every built app (a
/// debug test build too) serves the bundled one.
pub(crate) fn webview_origin() -> &'static str {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    // devUrl in tauri.conf.json
    ORIGIN.get_or_init(|| vault::tauri_origin(tauri::is_dev(), "http://localhost:5174"))
}

/// A debug build takes `PROOFPOLL_TEST_VAULT_PORT`: the one Vault a test
/// instance talks to (several test Vaults answer on one machine). A release
/// build never reads it.
pub(crate) fn test_vault_port() -> Option<u16> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var("PROOFPOLL_TEST_VAULT_PORT").ok().and_then(|v| v.trim().parse::<u16>().ok()) {
        return Some(p);
    }
    None
}

/// This app's Flowsta client id: the one the page is built with
/// (`VITE_FLOWSTA_CLIENT_ID`; `build.rs` passes it on). Every Tauri app's
/// webview presents the same origin, so the client id is what lets the
/// Vault tell this app from another one on the same computer. None in a
/// build that was given no client id.
pub(crate) fn client_id() -> Option<&'static str> {
    Some(env!("PROOFPOLL_CLIENT_ID")).filter(|id| !id.is_empty())
}

/// The one client every part of the app asks through, so the page, the
/// identity checks and the backup all talk to the same Vault, as the same
/// app.
pub(crate) fn client() -> &'static VaultClient {
    static CLIENT: OnceLock<VaultClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        VaultClient::new(VaultConfig { origin: webview_origin().to_string(), only_port: test_vault_port(), client_id: client_id().map(String::from) })
            .expect("the Vault client")
    })
}

/// The Vault to talk to right now, or None when none of this user's Vaults
/// answers.
pub(crate) async fn find_vault(timeout: Duration) -> Option<VaultProbe> {
    client().find(timeout).await
}

/// Tauri command for the webview: the same answer the Rust gates use, so
/// the page can never talk to a different Vault than the identity guard.
#[tauri::command]
pub async fn probe_vault() -> Option<VaultProbe> {
    find_vault(Duration::from_millis(2000)).await
}

/// Relaunch ProofPoll. At launch the profile is picked from the identity the
/// Vault has unlocked (`profiles::select_profile_root`), so after a switch
/// this is how ProofPoll moves onto the new identity's own profile instead
/// of staying read-only under the old name.
#[tauri::command]
pub fn restart_app(app: tauri::AppHandle, state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>) {
    log::info!("Restarting into the Vault's current identity");
    // The relaunch reads this once: bring the window back to the front
    // (the OS hands focus to whatever was behind us, usually the Vault)
    // and, if the profile it opens is not signed in yet, start the sign-in.
    if let Err(e) = vault::write_relaunch_marker(&state.device_root) {
        log::warn!("relaunch marker not written: {}", e);
    }
    app.restart();
}

/// Whether the unlocked Vault holds an identity other than the one this
/// profile is connected under, when it will not say which. The Vault names
/// its identity only to an app connected under it, so after the person
/// switched to an identity ProofPoll has never been connected under, its
/// status reads "unlocked" and names nobody. Asked about the identity this
/// profile IS connected under, it answers plainly that it holds another.
#[tauri::command]
pub async fn vault_holds_another_identity(
    app_agent_pub_key: String,
    state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
) -> Result<bool, String> {
    let Some(link) = crate::commands::load_identity_link(&state.data_dir) else { return Ok(false) };
    let Some(probe) = find_vault(Duration::from_millis(2000)).await else { return Ok(false) };
    if !probe.unlocked || !probe.initialized || probe.agent_pub_key.is_some() {
        return Ok(false);
    }
    let answer = client()
        .get(&format!("{}/link-status", probe.url))
        .query(&[("app_agent_pub_key", app_agent_pub_key.as_str()), ("expected_identity", link.vault_agent_pub_key.as_str())])
        .timeout(Duration::from_secs(4))
        .send()
        .await;
    let Ok(resp) = answer else { return Ok(false) };
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    Ok(says_another_identity(status.as_u16(), &body))
}

/// The Vault's plain answer that it holds a different identity than the one asked about.
fn says_another_identity(status: u16, body: &serde_json::Value) -> bool {
    status == 409 && body.get("error").and_then(|e| e.as_str()) == Some("identity_mismatch")
}

/// The app is about to restart itself after installing an update: the
/// next start brings its window to the front (macOS leaves a relaunched
/// app behind the others).
const RAISE_MARKER: &str = "raise-window-once";
pub(crate) fn write_raise_marker(device_root: &std::path::Path) {
    let _ = std::fs::write(device_root.join(RAISE_MARKER), b"1");
}
pub(crate) fn take_raise_marker(device_root: &std::path::Path) -> bool {
    let p = device_root.join(RAISE_MARKER);
    p.exists() && std::fs::remove_file(&p).is_ok()
}

/// The Vault the sign-in must use, when a test build pins one (`None` in
/// every release build: the SDK then finds the Vault itself).
#[tauri::command]
pub fn test_vault_url() -> Option<String> {
    test_vault_port().map(|p| format!("http://127.0.0.1:{p}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_vaults_plain_answer_counts_as_another_identity() {
        assert!(says_another_identity(409, &serde_json::json!({"error": "identity_mismatch"})));
        assert!(!says_another_identity(409, &serde_json::json!({"error": "identity_unconfirmed"})));
        assert!(!says_another_identity(200, &serde_json::json!({"linked": false})));
        assert!(!says_another_identity(403, &serde_json::json!({"error": "invalid_link_token"})));
        assert!(!says_another_identity(409, &serde_json::Value::Null));
    }

    #[test]
    fn the_raise_marker_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!take_raise_marker(dir.path()));
        write_raise_marker(dir.path());
        assert!(take_raise_marker(dir.path()));
        assert!(!take_raise_marker(dir.path()));
    }
}
