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
pub const APP_ID_V1_2: &str = "proofpoll_v1_2";
pub const APP_ID_V1_3: &str = "proofpoll_v1_3";

const HAPP_FILE_V1_0: &str = "proofpoll_v1_0_happ.happ";
const HAPP_FILE_V1_1: &str = "proofpoll_v1_1_happ.happ";
const HAPP_FILE_V1_2: &str = "proofpoll_v1_2_happ.happ";
const HAPP_FILE_V1_3: &str = "proofpoll_v1_3_happ.happ";

/// The active version for all new reads and writes.
pub const ACTIVE_APP_ID: &str = APP_ID_V1_3;

/// Result of the DNA installation phase.
pub struct InstallResult {
    pub agent_pub_key: AgentPubKey,
    /// True if v1.2 data exists and needs to be migrated to v1.3.
    pub needs_migration: bool,
    /// True if v1.0 is installed and usable (for legacy reads).
    pub v1_0_available: bool,
    /// True if v1.1 is installed and usable (for migration reads).
    pub v1_1_available: bool,
    /// True if v1.2 is installed and usable (for migration reads).
    pub v1_2_available: bool,
}

/// Install ProofPoll DNAs, handling upgrades across all versions.
///
/// - Fresh install: installs v1.3 only.
/// - Upgrade from v1.2: installs v1.3 alongside v1.2, flags migration needed.
/// - Already current: re-enables v1.3, keeps older versions for migration reads.
pub async fn install_dnas(
    admin_port: u16,
    resource_dir: &Path,
) -> Result<InstallResult, String> {
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
    let mut v1_2_installed = false;
    let mut v1_2_agent_key: Option<AgentPubKey> = None;
    let mut v1_3_installed = false;
    let mut v1_3_agent_key: Option<AgentPubKey> = None;

    for app in &existing_apps {
        if app.installed_app_id == APP_ID_V1_0 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                // Disabled v1.0 — try to re-enable for legacy reads
                log::warn!("v1.0 DNA disabled, attempting re-enable...");
                match admin_ws.enable_app(APP_ID_V1_0.to_string()).await {
                    Ok(_) => {
                        v1_0_installed = true;
                        v1_0_agent_key = Some(app.agent_pub_key.clone());
                    }
                    Err(e) => {
                        log::warn!("Could not re-enable v1.0: {}. Legacy reads will be unavailable.", e);
                    }
                }
            } else {
                v1_0_installed = true;
                v1_0_agent_key = Some(app.agent_pub_key.clone());
            }
        }
        if app.installed_app_id == APP_ID_V1_1 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                // Disabled v1.1 — try to re-enable for migration reads
                log::warn!("v1.1 DNA disabled, attempting re-enable...");
                match admin_ws.enable_app(APP_ID_V1_1.to_string()).await {
                    Ok(_) => {
                        v1_1_installed = true;
                        v1_1_agent_key = Some(app.agent_pub_key.clone());
                    }
                    Err(e) => {
                        log::warn!("Could not re-enable v1.1: {}. Migration reads may be unavailable.", e);
                    }
                }
            } else {
                v1_1_installed = true;
                v1_1_agent_key = Some(app.agent_pub_key.clone());
                // Re-enable to recover any disabled cells
                if let Err(e) = admin_ws.enable_app(APP_ID_V1_1.to_string()).await {
                    log::warn!("Could not re-enable v1.1: {}", e);
                }
            }
        }
        if app.installed_app_id == APP_ID_V1_2 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                log::warn!("v1.2 DNA disabled, attempting re-enable...");
                match admin_ws.enable_app(APP_ID_V1_2.to_string()).await {
                    Ok(_) => {
                        v1_2_installed = true;
                        v1_2_agent_key = Some(app.agent_pub_key.clone());
                    }
                    Err(e) => {
                        log::warn!("Could not re-enable v1.2: {}. Migration reads may be unavailable.", e);
                    }
                }
            } else {
                v1_2_installed = true;
                v1_2_agent_key = Some(app.agent_pub_key.clone());
                if let Err(e) = admin_ws.enable_app(APP_ID_V1_2.to_string()).await {
                    log::warn!("Could not re-enable v1.2: {}", e);
                }
            }
        }
        if app.installed_app_id == APP_ID_V1_3 {
            if matches!(app.status, holochain_types::prelude::AppStatus::Disabled(_)) {
                log::warn!("v1.3 DNA disabled, reinstalling...");
                admin_ws
                    .uninstall_app(APP_ID_V1_3.to_string(), false)
                    .await
                    .map_err(|e| format!("Failed to uninstall disabled v1.3: {}", e))?;
            } else {
                v1_3_installed = true;
                v1_3_agent_key = Some(app.agent_pub_key.clone());
                admin_ws
                    .enable_app(APP_ID_V1_3.to_string())
                    .await
                    .map_err(|e| format!("Failed to re-enable v1.3: {}", e))?;
            }
        }
    }

    // Install v1.3 if not present — reuse the most recent agent
    // key so identity links survive.
    if !v1_3_installed {
        let agent_key = v1_2_agent_key.as_ref()
            .or(v1_1_agent_key.as_ref())
            .or(v1_0_agent_key.as_ref())
            .cloned();

        let happ_path = resource_dir.join(HAPP_FILE_V1_3);
        if !happ_path.exists() {
            return Err(format!(
                "ProofPoll v1.3 hApp bundle not found at {:?}",
                happ_path
            ));
        }

        log::info!("Installing ProofPoll v1.3 DNA from {:?}...", happ_path);
        let payload = InstallAppPayload {
            source: AppBundleSource::Path(happ_path),
            agent_key,
            installed_app_id: Some(APP_ID_V1_3.to_string()),
            network_seed: None,
            roles_settings: None,
            ignore_genesis_failure: false,
        };

        let app_info = admin_ws
            .install_app(payload)
            .await
            .map_err(|e| format!("Failed to install v1.3 DNA: {}", e))?;

        admin_ws
            .enable_app(APP_ID_V1_3.to_string())
            .await
            .map_err(|e| format!("Failed to enable v1.3 DNA: {}", e))?;

        v1_3_agent_key = Some(app_info.agent_pub_key);
        log::info!("ProofPoll v1.3 DNA installed and enabled");
    }

    // Force re-enable v1.3 to recover any disabled cells from previous runs
    if let Err(e) = admin_ws.enable_app(APP_ID_V1_3.to_string()).await {
        log::warn!("Could not re-enable v1.3: {}", e);
    }

    // Verify v1.3 is enabled
    let enabled_apps = admin_ws
        .list_apps(Some(AppStatusFilter::Enabled))
        .await
        .map_err(|e| format!("Failed to verify installed apps: {}", e))?;

    let v1_3_enabled = enabled_apps
        .iter()
        .any(|app| app.installed_app_id == APP_ID_V1_3);

    if !v1_3_enabled {
        return Err("ProofPoll v1.3 DNA installation verification failed".to_string());
    }

    let agent_pub_key = v1_3_agent_key.ok_or("No agent key after installation")?;

    // Migration needed if v1.2 exists and v1.3 was just installed
    let needs_migration = v1_2_installed && !v1_3_installed;

    Ok(InstallResult {
        agent_pub_key,
        needs_migration,
        v1_0_available: v1_0_installed,
        v1_1_available: v1_1_installed,
        v1_2_available: v1_2_installed,
    })
}

/// Attach an app interface, authorize signing credentials, and connect
/// AppWebsockets for all installed versions.
///
/// Returns (app_port, v1.3_active_client, optional_v1_2_client, optional_v1_1_client, optional_v1_0_client).
pub async fn setup_app_interface(
    admin_port: u16,
    v1_0_available: bool,
    v1_1_available: bool,
    v1_2_available: bool,
) -> Result<(u16, AppWebsocket, Option<AppWebsocket>, Option<AppWebsocket>, Option<AppWebsocket>), String> {
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

    // Ensure all cells are actually ready before we try to authorize them.
    // The conductor can report Enabled status while cells are still
    // initializing after a restart — calling authorize_signing_credentials
    // too early returns CellDisabled. This pre-check waits up to ~18s with
    // periodic re-enable attempts so the per-cell loop below sees ready
    // cells. Without this, dev sessions left idle for a while come back to
    // life with disabled cells and zome calls silently fail.
    ensure_apps_enabled(&admin_ws).await;

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
                            let e_str = e.to_string();
                            if e_str.contains("CellDisabled") {
                                // Cell is disabled even though the app showed as Enabled —
                                // this can happen after conductor restarts. Re-enable and retry.
                                log::warn!(
                                    "Cell disabled in {}, re-enabling and retrying...",
                                    app.installed_app_id
                                );
                                if let Err(enable_err) = admin_ws
                                    .enable_app(app.installed_app_id.clone())
                                    .await
                                {
                                    log::warn!(
                                        "Could not re-enable {}: {}. Signing skipped.",
                                        app.installed_app_id, enable_err
                                    );
                                } else {
                                    match admin_ws
                                        .authorize_signing_credentials(
                                            AuthorizeSigningCredentialsPayload {
                                                cell_id: cell_id.clone(),
                                                functions: None,
                                            },
                                        )
                                        .await
                                    {
                                        Ok(creds) => {
                                            signer.add_credentials(cell_id, creds);
                                            log::info!(
                                                "Signing credentials authorized for cell in {} (after re-enable)",
                                                app.installed_app_id
                                            );
                                        }
                                        Err(retry_err) => {
                                            log::warn!(
                                                "Still could not authorize signing for cell in {} after re-enable: {}. Skipping.",
                                                app.installed_app_id, retry_err
                                            );
                                        }
                                    }
                                }
                            } else {
                                // Non-CellDisabled error — log and skip.
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
    }

    let signer_arc: std::sync::Arc<dyn holochain_client::AgentSigner + Send + Sync> = signer.into();

    // Connect the active (v1.3) AppWebsocket.
    let token_v1_3 = admin_ws
        .issue_app_auth_token(
            IssueAppAuthenticationTokenPayload::for_installed_app_id(ACTIVE_APP_ID.into())
                .expiry_seconds(0)
                .single_use(false),
        )
        .await
        .map_err(|e| format!("Failed to issue v1.3 auth token: {}", e))?;

    let app_ws_v1_3 = AppWebsocket::connect(
        format!("localhost:{}", app_port),
        token_v1_3.token,
        signer_arc.clone(),
        Some("proofpoll".to_string()),
    )
    .await
    .map_err(|e| format!("Failed to connect v1.3 app WebSocket: {}", e))?;

    log::info!("v1.3 App WebSocket connected on port {}", app_port);

    // Connect the v1.2 AppWebsocket if available (for migration reads).
    let app_ws_v1_2 = if v1_2_available {
        match admin_ws
            .issue_app_auth_token(
                IssueAppAuthenticationTokenPayload::for_installed_app_id(APP_ID_V1_2.into())
                    .expiry_seconds(0)
                    .single_use(false),
            )
            .await
        {
            Ok(token_v1_2) => {
                match AppWebsocket::connect(
                    format!("localhost:{}", app_port),
                    token_v1_2.token,
                    signer_arc.clone(),
                    Some("proofpoll".to_string()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.2 App WebSocket connected for migration reads");
                        Some(ws)
                    }
                    Err(e) => {
                        log::warn!("Could not connect v1.2 WebSocket: {}. Migration reads will be skipped.", e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("Could not issue v1.2 auth token: {}. Migration reads will be skipped.", e);
                None
            }
        }
    } else {
        None
    };

    // Connect the v1.1 AppWebsocket if available (for migration reads).
    let app_ws_v1_1 = if v1_1_available {
        match admin_ws
            .issue_app_auth_token(
                IssueAppAuthenticationTokenPayload::for_installed_app_id(APP_ID_V1_1.into())
                    .expiry_seconds(0)
                    .single_use(false),
            )
            .await
        {
            Ok(token_v1_1) => {
                match AppWebsocket::connect(
                    format!("localhost:{}", app_port),
                    token_v1_1.token,
                    signer_arc.clone(),
                    Some("proofpoll".to_string()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.1 App WebSocket connected for migration reads");
                        Some(ws)
                    }
                    Err(e) => {
                        log::warn!("Could not connect v1.1 WebSocket: {}. Migration reads will be skipped.", e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("Could not issue v1.1 auth token: {}. Migration reads will be skipped.", e);
                None
            }
        }
    } else {
        None
    };

    // Connect the v1.0 AppWebsocket if available (for legacy reads).
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
                        log::info!("v1.0 App WebSocket connected for legacy reads");
                        Some(ws)
                    }
                    Err(e) => {
                        log::warn!("Could not connect v1.0 WebSocket: {}. Legacy reads will be skipped.", e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("Could not issue v1.0 auth token: {}. Legacy reads will be skipped.", e);
                None
            }
        }
    } else {
        None
    };

    Ok((app_port, app_ws_v1_3, app_ws_v1_2, app_ws_v1_1, app_ws_v1_0))
}

/// Re-enable any disabled apps + verify cells are actually ready before
/// callers try to authorize signing credentials.
///
/// The conductor can report Enabled status while cells are still
/// initializing after a restart — calling authorize_signing_credentials too
/// early returns CellDisabled. This helper waits up to ~18 seconds with
/// periodic re-enable attempts, retrying until a probe authorize succeeds
/// or we run out of attempts.
///
/// Idempotent and safe to call multiple times. Failures here are logged but
/// don't propagate — the per-cell auth loop in the caller handles the
/// remaining cell-by-cell skip behaviour.
pub async fn ensure_apps_enabled(admin_ws: &AdminWebsocket) {
    let apps = match admin_ws.list_apps(None).await {
        Ok(a) => a,
        Err(e) => {
            log::warn!("ensure_apps_enabled: list_apps failed: {}", e);
            return;
        }
    };

    for app in &apps {
        if let Err(e) = admin_ws.enable_app(app.installed_app_id.clone()).await {
            log::warn!(
                "ensure_apps_enabled: failed to enable {}: {}",
                app.installed_app_id,
                e,
            );
        }
    }

    // Pick the active version's cell as the probe. authorize_signing_credentials
    // will return CellDisabled until cells are truly ready.
    let probe_cell = apps
        .iter()
        .find(|a| a.installed_app_id == ACTIVE_APP_ID)
        .and_then(|app| {
            app.cell_info.values().flat_map(|cells| cells.iter()).find_map(|c| {
                if let CellInfo::Provisioned(p) = c {
                    Some(p.cell_id.clone())
                } else {
                    None
                }
            })
        });

    let Some(cell_id) = probe_cell else {
        return;
    };

    for attempt in 1..=6 {
        match admin_ws
            .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                cell_id: cell_id.clone(),
                functions: None,
            })
            .await
        {
            Ok(_) => {
                if attempt > 1 {
                    log::info!(
                        "ensure_apps_enabled: cells ready after {}s wait",
                        (attempt - 1) * 3,
                    );
                } else {
                    log::info!("ensure_apps_enabled: cells ready");
                }
                return;
            }
            Err(e) => {
                let err_str = format!("{}", e);
                if err_str.contains("CellDisabled") && attempt < 6 {
                    log::info!(
                        "ensure_apps_enabled: cells not ready yet (attempt {}), waiting 3s...",
                        attempt,
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    // Re-enable all apps before retrying — sometimes the
                    // conductor needs the enable_app nudge more than once.
                    for app in &apps {
                        let _ = admin_ws.enable_app(app.installed_app_id.clone()).await;
                    }
                } else {
                    log::warn!("ensure_apps_enabled: cell readiness check failed: {}", e);
                    return;
                }
            }
        }
    }
}
