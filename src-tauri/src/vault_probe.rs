//! This app's handle on the person's Flowsta Vault.
//!
//! Finding the right Vault (all three ports asked at once, another user
//! account's Vault ignored, the unlocked one preferred) is
//! `flowsta_vault_client::vault`; here are the app's own origin, its test
//! switch, and the commands the webview calls.

use flowsta_vault_client::vault::{self, VaultClient, VaultConfig};
use std::sync::OnceLock;
use std::time::Duration;

pub use flowsta_vault_client::vault::VaultProbe;
pub(crate) use flowsta_vault_client::vault::take_relaunch_marker;

/// The Origin header the Vault's bridge maps to this app's client_id. The
/// linked-apps registry records the origin the WEBVIEW presented during the
/// link ceremony, so every Rust-side request must present the same one: a
/// bare reqwest carries none, and the Vault then answers as to a stranger -
/// `/status` without the display name and picture (scope-gated per origin
/// since Vault 1.2), `/revoke-identity` refused. Found twice: the
/// /revoke-identity lesson, then the "F" avatar on Windows (2026-10-25).
/// `tauri dev` serves the dev server's origin; every built app (a debug
/// test build too) serves the bundled one.
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

/// The one client every part of the app asks through, so the page, the
/// identity checks and the backup all talk to the same Vault.
pub(crate) fn client() -> &'static VaultClient {
    static CLIENT: OnceLock<VaultClient> = OnceLock::new();
    CLIENT.get_or_init(|| VaultClient::new(VaultConfig { origin: webview_origin().to_string(), only_port: test_vault_port() }).expect("the Vault client"))
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

/// The Vault the sign-in must use, when a test build pins one (`None` in
/// every release build: the SDK then finds the Vault itself).
#[tauri::command]
pub fn test_vault_url() -> Option<String> {
    test_vault_port().map(|p| format!("http://127.0.0.1:{p}"))
}
