//! DNA installation and app client setup for ProofPoll.
//!
//! Supports multiple DNA versions for migration. The "active" version is v1.1;
//! v1.0 is kept installed (read-only) when migrating existing data.
//!
//! ## For forking developers
//!
//! This file manages the Holochain app lifecycle:
//!   - `install_dnas()` — Installs your hApp bundles, handles version upgrades
//!   - `setup_app_interface()` — Creates WebSocket connections for zome calls
//!
//! Change the version constants at the top to match your app/hApp names.
//! The `"proofpoll"` string passed to `AdminWebsocket::connect()` and
//! `AppWebsocket::connect()` is an origin identifier — change it to your app name.
//! The installation and WebSocket logic is otherwise app-agnostic.

use holochain_client::{
    AdminWebsocket, AllowedOrigins, AppStatusFilter, AppWebsocket,
    AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner, InstallAppPayload,
    IssueAppAuthenticationTokenPayload,
};
use holochain_types::app::AppBundleSource;
use holochain_types::prelude::AgentPubKey;
use std::path::Path;

// ── Version constants ─────────────────────────────────────────────────
//
// When adding a new version:
//   1. Add APP_ID_V1_X and HAPP_FILE_V1_X constants
//   2. Update ACTIVE_APP_ID to point to the new version
//   3. Update install_dnas() to install the new version
//   4. Add migration logic in migration.rs

pub const APP_ID_V1_0: &str = "proofpoll_v1_0";
pub const APP_ID_V1_1: &str = "proofpoll_v1_1";

const HAPP_FILE_V1_0: &str = "proofpoll_v1_0_happ.happ";
const HAPP_FILE_V1_1: &str = "proofpoll_v1_1_happ.happ";

/// The active version for all new reads and writes.
pub const ACTIVE_APP_ID: &str = APP_ID_V1_1;

/// Result of the DNA installation phase.
pub struct InstallResult {
    pub agent_pub_key: AgentPubKey,
    /// True if v1.0 data exists and needs to be migrated to v1.1.
    pub needs_migration: bool,
    /// True if v1.0 is installed and usable (for migration reads).
    pub v1_0_available: bool,
}

/// Install ProofPoll DNAs, handling upgrades from v1.0 to v1.1.
///
/// - Fresh install: installs v1.1 only.
/// - Upgrade: installs v1.1 alongside v1.0, flags migration needed.
/// - Already current: re-enables v1.1, checks if migration is still pending.
pub async fn install_dnas(admin_port: u16, resource_dir: &Path) -> Result<InstallResult, String> {
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WebSocket: {}", e))?;

    let existing_apps = admin_ws
        .list_apps(None)
        .await
        .map_err(|e| format!("Failed to list apps: {}", e))?;

    let mut v1_0_installed = false;
    let mut v1_0_agent_key: Option<AgentPubKey> = None;
    let mut v1_1_installed = false;
    let mut v1_1_agent_key: Option<AgentPubKey> = None;

    for app in &existing_apps {
        if app.installed_app_id == APP_ID_V1_0 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                // Disabled v1.0 — try to re-enable for migration reads
                log::warn!("v1.0 DNA disabled, attempting re-enable for migration...");
                match admin_ws.enable_app(APP_ID_V1_0.to_string()).await {
                    Ok(_) => {
                        v1_0_installed = true;
                        v1_0_agent_key = Some(app.agent_pub_key.clone());
                    }
                    Err(e) => {
                        log::warn!("Could not re-enable v1.0: {}. Migration will skip v1.0 data.", e);
                    }
                }
            } else {
                v1_0_installed = true;
                v1_0_agent_key = Some(app.agent_pub_key.clone());
            }
        }
        if app.installed_app_id == APP_ID_V1_1 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                log::warn!("v1.1 DNA disabled, reinstalling...");
                admin_ws
                    .uninstall_app(APP_ID_V1_1.to_string(), false)
                    .await
                    .map_err(|e| format!("Failed to uninstall disabled v1.1: {}", e))?;
            } else {
                v1_1_installed = true;
                v1_1_agent_key = Some(app.agent_pub_key.clone());
                // Re-enable to recover any disabled cells
                admin_ws
                    .enable_app(APP_ID_V1_1.to_string())
                    .await
                    .map_err(|e| format!("Failed to re-enable v1.1: {}", e))?;
            }
        }
    }

    // Install v1.1 if not present
    if !v1_1_installed {
        let agent_key = if let Some(key) = &v1_0_agent_key {
            // Reuse v1.0's agent key so identity links still work
            Some(key.clone())
        } else {
            None // Fresh install — let conductor generate
        };

        let happ_path = resource_dir.join(HAPP_FILE_V1_1);
        if !happ_path.exists() {
            return Err(format!(
                "ProofPoll v1.1 hApp bundle not found at {:?}",
                happ_path
            ));
        }

        log::info!("Installing ProofPoll v1.1 DNA from {:?}...", happ_path);
        let payload = InstallAppPayload {
            source: AppBundleSource::Path(happ_path),
            agent_key,
            installed_app_id: Some(APP_ID_V1_1.to_string()),
            network_seed: None,
            roles_settings: None,
            ignore_genesis_failure: false,
        };

        let app_info = admin_ws
            .install_app(payload)
            .await
            .map_err(|e| format!("Failed to install v1.1 DNA: {}", e))?;

        admin_ws
            .enable_app(APP_ID_V1_1.to_string())
            .await
            .map_err(|e| format!("Failed to enable v1.1 DNA: {}", e))?;

        v1_1_agent_key = Some(app_info.agent_pub_key);
        log::info!("ProofPoll v1.1 DNA installed and enabled");
    }

    // Force re-enable v1.1 to recover any disabled cells from previous runs
    // (CellDisabled can happen even when the app-level status shows Running)
    if let Err(e) = admin_ws.enable_app(APP_ID_V1_1.to_string()).await {
        log::warn!("Could not re-enable v1.1: {}", e);
    }

    // Verify v1.1 is enabled
    let enabled_apps = admin_ws
        .list_apps(Some(AppStatusFilter::Enabled))
        .await
        .map_err(|e| format!("Failed to verify installed apps: {}", e))?;

    let v1_1_enabled = enabled_apps
        .iter()
        .any(|app| app.installed_app_id == APP_ID_V1_1);

    if !v1_1_enabled {
        return Err("ProofPoll v1.1 DNA installation verification failed".to_string());
    }

    let agent_pub_key = v1_1_agent_key.ok_or("No agent key after installation")?;

    // Migration is needed if v1.0 exists and v1.1 was just installed
    let needs_migration = v1_0_installed && !v1_1_installed;

    Ok(InstallResult {
        agent_pub_key,
        needs_migration,
        v1_0_available: v1_0_installed,
    })
}

/// Attach an app interface, authorize signing credentials, and connect
/// AppWebsockets for all installed versions.
///
/// Returns (app_port, active_client, optional_v1_0_client).
pub async fn setup_app_interface(
    admin_port: u16,
    v1_0_available: bool,
) -> Result<(u16, AppWebsocket, Option<AppWebsocket>), String> {
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

    // Authorize signing credentials for ALL provisioned cells (both versions).
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
                    match admin_ws
                        .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                            cell_id: cell_id.clone(),
                            functions: None,
                        })
                        .await
                    {
                        Ok(creds) => {
                            signer.add_credentials(cell_id, creds);
                            log::info!(
                                "Signing credentials authorized for cell in {}",
                                app.installed_app_id
                            );
                        }
                        Err(e) => {
                            // Cell may be disabled (e.g., old v1.0 after migration).
                            // Skip it — only the active version needs signing credentials.
                            log::warn!(
                                "Could not authorize signing for cell in {}: {}. Skipping.",
                                app.installed_app_id, e
                            );
                        }
                    }
                }
            }
        }
    }

    let signer_arc: std::sync::Arc<dyn holochain_client::AgentSigner + Send + Sync> = signer.into();

    // Connect the active (v1.1) AppWebsocket.
    let token_v1_1 = admin_ws
        .issue_app_auth_token(
            IssueAppAuthenticationTokenPayload::for_installed_app_id(ACTIVE_APP_ID.into())
                .expiry_seconds(0)
                .single_use(false),
        )
        .await
        .map_err(|e| format!("Failed to issue v1.1 auth token: {}", e))?;

    let app_ws_v1_1 = AppWebsocket::connect(
        format!("localhost:{}", app_port),
        token_v1_1.token,
        signer_arc.clone(),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect v1.1 app WebSocket: {}", e))?;

    log::info!("v1.1 App WebSocket connected on port {}", app_port);

    // Connect the v1.0 AppWebsocket if available (for migration reads).
    let app_ws_v1_0 = if v1_0_available {
        match admin_ws
            .issue_app_auth_token(
                IssueAppAuthenticationTokenPayload::for_installed_app_id(APP_ID_V1_0.into())
                    .expiry_seconds(0)
                    .single_use(false),
            )
            .await
        {
            Ok(token_v1_0) => {
                match AppWebsocket::connect(
                    format!("localhost:{}", app_port),
                    token_v1_0.token,
                    signer_arc,
                    Some("proofpoll".to_string()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.0 App WebSocket connected for migration reads");
                        Some(ws)
                    }
                    Err(e) => {
                        log::warn!("Could not connect v1.0 WebSocket: {}. Migration will be skipped.", e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("Could not issue v1.0 auth token: {}. Migration will be skipped.", e);
                None
            }
        }
    } else {
        None
    };

    Ok((app_port, app_ws_v1_1, app_ws_v1_0))
}
