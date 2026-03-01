//! DNA installation and app client setup for ProofPoll.
//!
//! Simplified from Flowsta Vault: single DNA, uses conductor-generated agent key.
//! All zome calls go through the Rust AppWebsocket — no @holochain/client in the frontend.

use holochain_client::{
    AdminWebsocket, AllowedOrigins, AppStatusFilter, AppWebsocket,
    AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner, InstallAppPayload,
    IssueAppAuthenticationTokenPayload,
};
use holochain_types::app::AppBundleSource;
use holochain_types::prelude::AgentPubKey;
use std::path::Path;

const APP_ID: &str = "proofpoll_v1_0";
const HAPP_FILE: &str = "proofpoll_v1_0_happ.happ";

/// Install the ProofPoll DNA if not already present.
///
/// Unlike the Vault, ProofPoll lets the conductor generate a random agent key
/// (standard Holochain behavior). Returns the agent's public key.
pub async fn install_dna(admin_port: u16, resource_dir: &Path) -> Result<AgentPubKey, String> {
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WebSocket: {}", e))?;

    // Check if already installed.
    let existing_apps = admin_ws
        .list_apps(None)
        .await
        .map_err(|e| format!("Failed to list apps: {}", e))?;

    for app in &existing_apps {
        if app.installed_app_id == APP_ID {
            log::info!("ProofPoll DNA already installed, skipping");
            return Ok(app.agent_pub_key.clone());
        }
    }

    // Install the DNA.
    let happ_path = resource_dir.join(HAPP_FILE);
    if !happ_path.exists() {
        return Err(format!(
            "ProofPoll hApp bundle not found at {:?}",
            happ_path
        ));
    }

    log::info!("Installing ProofPoll DNA from {:?}...", happ_path);
    let payload = InstallAppPayload {
        source: AppBundleSource::Path(happ_path),
        agent_key: None, // Let the conductor generate a random key
        installed_app_id: Some(APP_ID.to_string()),
        network_seed: None,
        roles_settings: None,
        ignore_genesis_failure: false,
    };

    let app_info = admin_ws
        .install_app(payload)
        .await
        .map_err(|e| format!("Failed to install ProofPoll DNA: {}", e))?;

    admin_ws
        .enable_app(APP_ID.to_string())
        .await
        .map_err(|e| format!("Failed to enable ProofPoll DNA: {}", e))?;

    log::info!("ProofPoll DNA installed and enabled");

    // Verify it's enabled.
    let enabled_apps = admin_ws
        .list_apps(Some(AppStatusFilter::Enabled))
        .await
        .map_err(|e| format!("Failed to verify installed app: {}", e))?;

    let is_enabled = enabled_apps
        .iter()
        .any(|app| app.installed_app_id == APP_ID);

    if !is_enabled {
        return Err("ProofPoll DNA installation verification failed".to_string());
    }

    Ok(app_info.agent_pub_key)
}

/// Attach an app interface, authorize signing credentials, and connect
/// an AppWebsocket ready for zome calls. Returns (app_port, app_client).
pub async fn setup_app_interface(admin_port: u16) -> Result<(u16, AppWebsocket), String> {
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WebSocket: {}", e))?;

    let app_port = admin_ws
        .attach_app_interface(0, None, AllowedOrigins::Any, None)
        .await
        .map_err(|e| format!("Failed to attach app interface: {}", e))?;

    log::info!("App interface attached on port {}", app_port);

    // Issue a long-lived, reusable authentication token.
    let token_payload = IssueAppAuthenticationTokenPayload::for_installed_app_id(APP_ID.into())
        .expiry_seconds(0)
        .single_use(false);
    let issued = admin_ws
        .issue_app_auth_token(token_payload)
        .await
        .map_err(|e| format!("Failed to issue app auth token: {}", e))?;

    log::info!("App authentication token issued");

    // Authorize signing credentials for all provisioned cells.
    let signer = ClientAgentSigner::default();
    let apps = admin_ws
        .list_apps(Some(AppStatusFilter::Enabled))
        .await
        .map_err(|e| format!("Failed to list apps: {}", e))?;

    for app in &apps {
        for cells in app.cell_info.values() {
            for cell in cells {
                if let CellInfo::Provisioned(provisioned) = cell {
                    let cell_id = provisioned.cell_id.clone();
                    let creds = admin_ws
                        .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                            cell_id: cell_id.clone(),
                            functions: None,
                        })
                        .await
                        .map_err(|e| format!("Failed to authorize signing: {}", e))?;

                    signer.add_credentials(cell_id, creds);
                    log::info!("Signing credentials authorized for cell");
                }
            }
        }
    }

    // Connect the AppWebsocket with the signer — ready for zome calls.
    let app_ws = AppWebsocket::connect(
        format!("localhost:{}", app_port),
        issued.token,
        signer.into(),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect app WebSocket: {}", e))?;

    log::info!("App WebSocket connected on port {}", app_port);

    Ok((app_port, app_ws))
}
