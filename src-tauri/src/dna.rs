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
//! Every `AdminWebsocket::connect()` and `AppWebsocket::connect()` presents
//! `conductor::node_origin()`, the private origin made once per run that the
//! conductor config allows; nothing here is named after the app.
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
/// v1.4: the Holochain 0.7 generation of v1.3, on the 0.7.1 conductor.
pub const APP_ID_V1_4: &str = "proofpoll_v1_4";
/// v1.5: v1.4 plus records carried from v1.3 and v1.4 that the network
/// checks against their original signatures, on the same conductor.
pub const APP_ID_V1_5: &str = "proofpoll_v1_5";

const HAPP_FILE_V1_0: &str = "proofpoll_v1_0_happ.happ";
const HAPP_FILE_V1_1: &str = "proofpoll_v1_1_happ.happ";
const HAPP_FILE_V1_2: &str = "proofpoll_v1_2_happ.happ";
const HAPP_FILE_V1_3: &str = "proofpoll_v1_3_happ.happ";
const HAPP_FILE_V1_4: &str = "proofpoll_v1_4_happ.happ";
const HAPP_FILE_V1_5: &str = "proofpoll_v1_5_happ.happ";

/// The active version for all new reads and writes (on the 0.7.1 conductor).
/// v1.4 beside it is read until its records are carried (`carry_move.rs`).
pub const ACTIVE_APP_ID: &str = APP_ID_V1_5;
/// The newest version on the 0.6.1 conductor: the migration source, and the
/// home of polls whose authors have not upgraded yet.
pub const ACTIVE_APP_ID_06: &str = APP_ID_V1_3;

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
    /// True if v1.3 was installed on this start.
    pub installed_now: bool,
}

/// Install ProofPoll DNAs, handling upgrades across all versions.
///
/// - Fresh install: installs v1.3 only.
/// - Upgrade from v1.2: installs v1.3 alongside v1.2, flags migration needed.
/// - Already current: re-enables v1.3, keeps older versions for migration reads.
pub async fn install_dnas(
    admin_port: u16,
    resource_dir: &Path,
    lair_client: &lair_keystore_api::prelude::LairClient,
    data_dir: &Path,
) -> Result<InstallResult, String> {
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some(crate::conductor::node_origin()),
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
                // Never uninstall a cell that can hold data: it deletes this
                // device's chain (its polls, votes, drafts and reasons).
                // Enable it again; if that fails, stop with nothing changed.
                log::warn!("v1.3 DNA disabled, enabling it again...");
                admin_ws
                    .enable_app(APP_ID_V1_3.to_string())
                    .await
                    .map_err(|e| format!("ProofPoll v1.3 is disabled and could not be enabled again ({}). Nothing was changed.", e))?;
                v1_3_installed = true;
                v1_3_agent_key = Some(app.agent_pub_key.clone());
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
        let agent_key = match v1_2_agent_key.as_ref()
            .or(v1_1_agent_key.as_ref())
            .or(v1_0_agent_key.as_ref())
            .cloned()
        {
            Some(k) => Some(k),
            // Brand-new install: the agent seed originates app-side so the
            // canonical backup can escrow it - the export carries the means
            // of authorship, and a restore resumes the SAME agent.
            None => Some(
                crate::device_seed::ensure_device_agent(lair_client, data_dir).await?,
            ),
        };

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
        installed_now: !v1_3_installed,
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
) -> Result<(u16, Option<crate::hc::AppClient>, Option<crate::hc::AppClient>, Option<crate::hc::AppClient>, Option<crate::hc::AppClient>), String> {
    use crate::hc::AppClient;
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("Failed to connect to admin WebSocket: {}", e))?;

    let app_port = admin_ws
        .attach_app_interface(0, None, AllowedOrigins::Origins(std::collections::HashSet::from([crate::conductor::node_origin()])), None)
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
            IssueAppAuthenticationTokenPayload::for_installed_app_id(ACTIVE_APP_ID_06.into())
                .expiry_seconds(0)
                .single_use(false),
        )
        .await
        .map_err(|e| format!("Failed to issue v1.3 auth token: {}", e))?;

    let app_ws_v1_3 = AppWebsocket::connect(
        format!("localhost:{}", app_port),
        token_v1_3.token,
        signer_arc.clone(),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map(AppClient::V06)
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
                    Some(crate::conductor::node_origin()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.2 App WebSocket connected for migration reads");
                        Some(AppClient::V06(ws))
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
                    Some(crate::conductor::node_origin()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.1 App WebSocket connected for migration reads");
                        Some(AppClient::V06(ws))
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
                    Some(crate::conductor::node_origin()),
                )
                .await
                {
                    Ok(ws) => {
                        log::info!("v1.0 App WebSocket connected for legacy reads");
                        Some(AppClient::V06(ws))
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

    Ok((app_port, Some(app_ws_v1_3), app_ws_v1_2, app_ws_v1_1, app_ws_v1_0))
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
        .find(|a| a.installed_app_id == ACTIVE_APP_ID_06)
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


// ── The Holochain 0.7 line (DNA v1.4, and v1.5 beside it) ────────────

/// One app on the 0.7 conductor, with the version a backup's cell names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct App07 {
    pub app_id: &'static str,
    happ: &'static str,
    pub dna_version: &'static str,
}

/// The apps on the 0.7 conductor, in install order: v1.4 (the active one),
/// then v1.5 (carried records; installed beside v1.4, not called yet).
pub const APPS_07: [App07; 2] = [
    App07 { app_id: APP_ID_V1_4, happ: HAPP_FILE_V1_4, dna_version: "1.4" },
    App07 { app_id: APP_ID_V1_5, happ: HAPP_FILE_V1_5, dna_version: "1.5" },
];

/// What `install_dnas_07` did with one app on this start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install07 {
    /// Installed before; it is enabled again.
    Present,
    /// Installed now, with a new chain.
    Installed,
    /// Rebuilding this key's existing chain from the network (installed now
    /// with `restore_from_dht`, or a restore still running from an earlier
    /// start); `wait_for_restore_07` finishes it.
    Restoring,
}

/// What `install_dnas_07` did with each of `APPS_07`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installs07(pub Vec<(App07, Install07)>);

impl Installs07 {
    /// The apps still rebuilding their chain from the network.
    pub fn restoring(&self) -> Vec<App07> {
        self.0.iter().filter(|(_, i)| *i == Install07::Restoring).map(|(a, _)| *a).collect()
    }

    /// Whether anything was installed on this start (a restore included).
    pub fn any_new(&self) -> bool {
        self.0.iter().any(|(_, i)| *i != Install07::Present)
    }
}

/// Install `APPS_07` on the 0.7.1 conductor under the same agent key the 0.6
/// conductor uses (the key lives in the shared lair; the DNA hashes differ,
/// so one key on two conductors is allowed). Re-enables an installed app.
///
/// `restore`: the versions whose chain this key already has on the network
/// (an adopted key, from its backup's cells); those install without genesis
/// and rebuild that chain from peers. A fresh genesis under such a key starts
/// a second chain: in 0.7.1 it shares one store with the old chain arriving
/// from peers, and the device's writes fail or fork the chain. An installed
/// v1.5 has a chain even before the app writes to it (its genesis).
pub async fn install_dnas_07(
    admin_port: u16,
    resource_dir: &Path,
    agent_key_raw39: &[u8],
    restore: &[String],
) -> Result<Installs07, String> {
    use holochain_client_07::AdminWebsocket;

    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("Failed to connect to the 0.7 admin WebSocket: {}", e))?;
    let existing = admin_ws
        .list_apps(None)
        .await
        .map_err(|e| format!("Failed to list apps (0.7): {}", e))?;
    let mut done = Vec::new();
    for app in APPS_07 {
        let restore = restore.iter().any(|v| v == app.dna_version);
        done.push((app, install_one_07(&admin_ws, &existing, resource_dir, agent_key_raw39, app, restore).await?));
    }
    Ok(Installs07(done))
}

async fn install_one_07(
    admin_ws: &holochain_client_07::AdminWebsocket,
    existing: &[holochain_client_07::AppInfo],
    resource_dir: &Path,
    agent_key_raw39: &[u8],
    app: App07,
    restore: bool,
) -> Result<Install07, String> {
    use holochain_client_07::InstallAppPayload;
    use holochain_types_07::app::{AppBundleSource, AppStatus};
    use holochain_types_07::prelude::AgentPubKey;

    let v = app.dna_version;
    if let Some(info) = existing.iter().find(|a| a.installed_app_id == app.app_id) {
        match &info.status {
            // The conductor resumes a restore by itself after a restart.
            AppStatus::AwaitingRestore => {
                log::info!("ProofPoll v{} is still restoring its chain from the network (0.7)", v);
                return Ok(Install07::Restoring);
            }
            AppStatus::Unrecoverable(_, reason) => {
                return Err(unrecoverable_message(&format!("{:?}", reason)));
            }
            // A restore that finished while ProofPoll was closed: the chain
            // is there and not yet enabled. `wait_for_restore_07` compares
            // it with the backup before enabling it.
            AppStatus::Disabled(_) if restore => {
                log::info!("ProofPoll v{} finished restoring while ProofPoll was closed (0.7)", v);
                return Ok(Install07::Restoring);
            }
            _ => {}
        }
        admin_ws
            .enable_app(app.app_id.to_string())
            .await
            .map_err(|e| format!("Failed to re-enable v{}: {}", v, e))?;
        log::info!("ProofPoll v{} DNA present and enabled (0.7)", v);
        return Ok(Install07::Present);
    }
    let happ_path = resource_dir.join(app.happ);
    if !happ_path.exists() {
        return Err(format!("ProofPoll v{} hApp bundle not found at {:?}", v, happ_path));
    }
    log::info!(
        "Installing ProofPoll v{} DNA from {:?} (0.7{})...",
        v,
        happ_path,
        if restore { ", restoring this key's chain from the network" } else { "" }
    );
    let payload = InstallAppPayload {
        source: AppBundleSource::Path(happ_path),
        agent_key: Some(AgentPubKey::from_raw_39(agent_key_raw39.to_vec())),
        installed_app_id: Some(app.app_id.to_string()),
        network_seed: None,
        roles_settings: None,
        ignore_genesis_failure: false,
        restore_from_dht: restore,
    };
    admin_ws
        .install_app(payload)
        .await
        .map_err(|e| format!("Failed to install v{} DNA: {}", v, e))?;
    if restore {
        return Ok(Install07::Restoring);
    }
    admin_ws
        .enable_app(app.app_id.to_string())
        .await
        .map_err(|e| format!("Failed to enable v{} DNA: {}", v, e))?;
    log::info!("ProofPoll v{} DNA installed and enabled (0.7)", v);
    Ok(Install07::Installed)
}

/// What a restore that can never finish says (a validated chain-integrity
/// warrant against the key: Holochain's terminal `Unrecoverable`).
fn unrecoverable_message(reason: &str) -> String {
    format!(
        "This key's history on the network cannot be restored: the network holds a \
         warrant against it ({}). Your published polls and votes are unchanged. Use a new \
         key on this device instead (Identity, Backups and recovery).",
        reason
    )
}

/// Set when the person chose to go on with a restore that is missing the
/// old device's last records (the start screen's button). Read by
/// `wait_for_restore_07`; never cleared, the choice holds for this start.
pub static RESTORE_ACCEPT_SHORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by the start screen's "Try again" while a short restore is set aside;
/// taken (and cleared) by the start's wait in `conductor.rs`.
pub static RESTORE_TRY_AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a finished restore came back with (the library's type): `Whole`
/// is enabled; `Short` is not, the network's copy ending at `have` and the
/// backup at `want` (the peers a restore asks agreed on a head from before
/// the old device's last writes reached them).
pub use flowsta_hc_lines::restore::Restored;

/// Wait for `app`'s restore to finish, then enable it. Holochain retries by
/// itself (no peers, too few peers agreeing on the chain head, a gap), so
/// there is no time limit here: `progress` hears the seconds waited every
/// few seconds, for the start screen.
///
/// `backup_head`: the highest seq the backup holds for this version. A
/// restore that ends before it is not enabled (`Restored::Short`) unless the
/// person chose to go on: a chain enabled there would be written on from a
/// point the old device already wrote past.
pub async fn wait_for_restore_07(
    admin_port: u16,
    app: App07,
    backup_head: Option<u32>,
    progress: impl Fn(u64),
) -> Result<Restored, String> {
    use flowsta_hc_lines::restore::{finish, RestoreError};
    let admin = admin_07(admin_port).await?;
    let accept = RESTORE_ACCEPT_SHORT.load(std::sync::atomic::Ordering::SeqCst);
    match finish(&admin, app.app_id, backup_head, accept, |waited| progress(waited.as_secs())).await {
        Ok(restored) => Ok(restored),
        Err(RestoreError::Unrecoverable(reason)) => Err(unrecoverable_message(&reason)),
        Err(RestoreError::NotInstalled(_)) => Err(format!("v{} is not installed (0.7)", app.dna_version)),
        Err(e) => Err(format!("v{}: {}", app.dna_version, e)),
    }
}

/// The library's admin client on the 0.7 conductor.
async fn admin_07(admin_port: u16) -> Result<flowsta_hc_lines::AdminClient, String> {
    flowsta_hc_lines::AdminClient::connect(crate::hc::Line::V07, admin_port, &crate::conductor::node_origin())
        .await
        .map_err(|e| format!("Failed to connect to the 0.7 admin WebSocket: {}", e))
}

/// Set a short restore aside: remove what was restored, which also takes
/// this device off the network under the key. While it restores, a device
/// announces itself as the key's agent, and the peers then follow it instead
/// of the old device (one address per key): the old device's gossip rounds
/// time out and its last records never leave it (matrix 2026-11-13, twenty
/// minutes). Away from the network, the old device can hand them over.
pub async fn set_short_restore_aside_07(admin_port: u16, app: App07) -> Result<(), String> {
    let admin = admin_07(admin_port).await?;
    flowsta_hc_lines::restore::set_aside(&admin, app.app_id, crate::names::INSTALLED_APP_PREFIX)
        .await
        .map_err(|e| format!("Failed to remove the short restore of v{}: {}", app.dna_version, e))?;
    log::info!("[restore] v{}: the short restore is set aside and this device is off the network under the key", app.dna_version);
    Ok(())
}

/// Ask the network again for `app`'s chain after a short restore was set aside.
pub async fn ask_again_07(
    admin_port: u16,
    resource_dir: &Path,
    agent_key_raw39: &[u8],
    app: App07,
) -> Result<(), String> {
    use holochain_client_07::AdminWebsocket;
    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("Failed to connect to the 0.7 admin WebSocket: {}", e))?;
    match install_one_07(&admin_ws, &[], resource_dir, agent_key_raw39, app, true).await? {
        Install07::Restoring => Ok(()),
        other => Err(format!("v{} did not start restoring again ({:?})", app.dna_version, other)),
    }
}

/// Attach the 0.7 conductor's app interface, authorize signing for the v1.4
/// cell and connect the v1.4 app websocket.
/// The 0.7 app interface: the port, the active (v1.5) client, and the v1.4
/// client when v1.4 is installed.
pub async fn setup_app_interface_07(admin_port: u16) -> Result<(u16, crate::hc::AppClient, Option<crate::hc::AppClient>), String> {
    use holochain_client_07::{
        AdminWebsocket, AllowedOrigins, AppStatusFilter, AppWebsocket,
        AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner,
        IssueAppAuthenticationTokenPayload,
    };

    let admin_ws = AdminWebsocket::connect(
        format!("localhost:{}", admin_port),
        Some(crate::conductor::node_origin()),
    )
    .await
    .map_err(|e| format!("Failed to connect to the 0.7 admin WebSocket: {}", e))?;
    let app_port = admin_ws
        .attach_app_interface(0, None, AllowedOrigins::Origins(std::collections::HashSet::from([crate::conductor::node_origin()])), None)
        .await
        .map_err(|e| format!("Failed to attach the 0.7 app interface: {}", e))?;
    log::info!("0.7 app interface attached on port {}", app_port);

    let signer = ClientAgentSigner::default();
    let apps = admin_ws
        .list_apps(Some(AppStatusFilter::Enabled))
        .await
        .map_err(|e| format!("Failed to list apps (0.7): {}", e))?;
    for app in &apps {
        for cells in app.cell_info.values() {
            for cell in cells {
                if let CellInfo::Provisioned(provisioned) = cell {
                    let cell_id = provisioned.cell_id.clone();
                    // Cells can take a few seconds after install; CellNotRunning
                    // is 0.7's "not yet".
                    let mut authorized = false;
                    for attempt in 1..=6 {
                        match admin_ws
                            .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                                cell_id: cell_id.clone(),
                                functions: None,
                            })
                            .await
                        {
                            Ok(creds) => {
                                signer.add_credentials(cell_id.clone(), creds);
                                authorized = true;
                                break;
                            }
                            Err(e) => {
                                let s = e.to_string();
                                if (s.contains("CellNotRunning") || s.contains("CellDisabled")) && attempt < 6 {
                                    let _ = admin_ws.enable_app(app.installed_app_id.clone()).await;
                                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                } else {
                                    log::warn!("Could not authorize signing for a cell in {} (0.7): {}", app.installed_app_id, s);
                                    break;
                                }
                            }
                        }
                    }
                    if authorized {
                        log::info!("Signing credentials authorized for cell in {} (0.7)", app.installed_app_id);
                    }
                }
            }
        }
    }
    let signer_arc: std::sync::Arc<dyn holochain_client_07::AgentSigner + Send + Sync> = signer.into();
    // One app websocket per app: a token names one installed app.
    let connect = |app_id: &'static str| {
        let (admin_ws, signer) = (&admin_ws, signer_arc.clone());
        async move {
            let token = admin_ws
                .issue_app_auth_token(
                    IssueAppAuthenticationTokenPayload::for_installed_app_id(app_id.into())
                        .expiry_seconds(0)
                        .single_use(false),
                )
                .await
                .map_err(|e| format!("Failed to issue the {} auth token: {}", app_id, e))?;
            let ws = AppWebsocket::connect(format!("localhost:{}", app_port), token.token, signer, Some(crate::conductor::node_origin()))
                .await
                .map_err(|e| format!("Failed to connect the {} app WebSocket: {}", app_id, e))?;
            log::info!("{} App WebSocket connected on port {} (0.7)", app_id, app_port);
            Ok::<_, String>(crate::hc::AppClient::V07(ws))
        }
    };
    let active = connect(ACTIVE_APP_ID).await?;
    let v1_4 = if apps.iter().any(|a| a.installed_app_id == APP_ID_V1_4) {
        match connect(APP_ID_V1_4).await {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("v1.4 is not read this start: {}", e);
                None
            }
        }
    } else {
        None
    };
    Ok((app_port, active, v1_4))
}

#[cfg(test)]
mod restore_tests {
    use flowsta_hc_lines::restore::short_by as restore_is_short;

    #[test]
    fn a_restore_is_short_only_when_it_ends_before_the_backup() {
        // The 11-12 matrix miss: the network agreed on seq 37, the backup held to 39.
        assert_eq!(restore_is_short(Some(37), Some(39)), Some((37, 39)));
        // As far as the backup, or further (the old device wrote after its backup).
        assert_eq!(restore_is_short(Some(39), Some(39)), None);
        assert_eq!(restore_is_short(Some(44), Some(39)), None);
        // A marker from before the per-version heads: nothing to compare.
        assert_eq!(restore_is_short(Some(3), None), None);
        // An empty chain against a backup with records; against one with none.
        assert_eq!(restore_is_short(None, Some(5)), Some((0, 5)));
        assert_eq!(restore_is_short(None, Some(0)), None);
    }
}
