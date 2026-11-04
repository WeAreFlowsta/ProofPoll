//! ProofPoll Tauri application entry point.
//!
//! ## Module overview
//!
//! - `commands` — Tauri commands (frontend ↔ backend bridge). **Edit this for your app.**
//! - `conductor` — Holochain conductor lifecycle (start, stop, health). Reusable.
//! - `dna` — DNA installation and multi-version management. Change app IDs only.
//! - `lair` — Lair keystore management. Reusable as-is.
//! - `migration` — DNA version migration orchestration. Adapt entry types.
//! - `crypto` — Lair-based encryption for private data on public DHT.
//!
//! ## Startup sequence
//!
//! 1. Create data directory and AppState
//! 2. Spawn async: start lair → start conductor → install DNAs → setup WebSockets
//! 3. If migration needed: spawn async migration task
//! 4. If pending votes from previous run: spawn retry loop
//!
//! ## For forking developers
//!
//! Update the `invoke_handler` at the bottom to register your own Tauri commands.
//! The startup sequence and migration wiring are reusable as-is.

mod commands;
mod conductor;
mod crypto;
mod dna;
mod hc;
mod instance_guard;
mod device_seed;
mod lair;
pub mod migration;
mod process_ext;
mod seed_adopt;
mod sidecar;

use commands::AppState;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
mod profiles;
// The move to v1.5 and the node's carrier use it (in progress: the slices
// in planning/PROOFPOLL_CARRIER.md); until then only its tests do.
#[allow(dead_code)]
mod carried;
mod vault_probe;
mod webview_fix;
mod kits;
mod updater;
#[cfg(feature = "harness")]
mod harness;

/// A debug build takes `PROOFPOLL_DATA_DIR`: a test instance's own data
/// folder on every platform (the harness runs several on one machine). A
/// release build never reads it.
fn test_data_dir() -> Option<std::path::PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("PROOFPOLL_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(dir));
    }
    None
}

/// Where proofpoll.log is written: Tauri's log folder, except on macOS.
/// There Finder shows a folder whose name ends in `.app` as an application,
/// and Tauri names the log folder after the identifier (`com.proofpoll.app`):
/// the logs showed as an app called "com.proofpoll" (0.6.0-beta.1,
/// 2026-11-04). So they go to ~/Library/Logs/ProofPoll, the older ones moved
/// there once. The identifier stays: it names the data folder too.
/// Returns the target and, when the older logs were moved, a line saying so.
fn log_target(app: &tauri::App) -> (tauri_plugin_log::TargetKind, Option<String>) {
    let file_name = Some("proofpoll".to_string());
    #[cfg(target_os = "macos")]
    if let Some((old, new)) = app.path().app_log_dir().ok().and_then(|old| {
        let new = old.parent()?.join("ProofPoll");
        Some((old, new))
    }) {
        let moved = (old.is_dir() && !new.exists()).then(|| match std::fs::rename(&old, &new) {
            Ok(()) => format!("[log] the older logs moved from {:?} to {:?}", old, new),
            Err(e) => format!("[log] the older logs stay in {:?}: {}", old, e),
        });
        return (tauri_plugin_log::TargetKind::Folder { path: new, file_name }, moved);
    }
    let _ = app;
    (tauri_plugin_log::TargetKind::LogDir { file_name }, None)
}

static WEBVIEW_FIX: std::sync::OnceLock<webview_fix::Outcome> = std::sync::OnceLock::new();

pub fn run() {
    // First, before anything starts a thread: on Linux with the NVIDIA driver
    // loaded, WebKitGTK 2.52 segfaults on a page change when NVIDIA's EGL
    // draws the window. The fix sets environment variables for the webview
    // only (the sidecars never inherit them).
    let _ = WEBVIEW_FIX.set(webview_fix::apply());
    let builder = tauri::Builder::default();
    // The embedded WebDriver the UI suite drives: the `e2e` feature only,
    // never a release build (scripts/check-test-features.mjs).
    #[cfg(feature = "e2e")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_dir = match test_data_dir() {
                Some(dir) => dir,
                None => app.path().app_data_dir().expect("Failed to get app data directory"),
            };
            std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");

            // Log to file (and stdout in dev) at INFO level in both dev and
            // release builds. Release-build silence would make any startup
            // failure invisible to users (and to us); the cost of a small
            // rolling log file is worth the diagnosability.
            let (log_file, log_moved) = log_target(app);
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    // The plugin's defaults are a 40 KB file that is DELETED
                    // when full. With the status line every 30 s that erased
                    // the startup block (both conductors ready, migration)
                    // within half an hour - the lines every gate's step 1
                    // asks for (beta.1, 2026-10-25). 2 MB, and the two
                    // previous files kept with a date in their name.
                    .max_file_size(2_000_000)
                    .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(2))
                    .targets([
                        tauri_plugin_log::Target::new(log_file),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    ])
                    .build(),
            )?;

            log::info!("ProofPoll starting up (v{})", env!("CARGO_PKG_VERSION"));
            if let Some(moved) = log_moved {
                log::info!("{}", moved);
            }
            log::info!("Data dir: {:?}", data_dir);

            // Identity profiles: pick (or create) the profile for the Flowsta
            // identity the Vault has unlocked right now, else the last one
            // used; a pre-profiles install is moved into a profile first.
            // Must run BEFORE AppState::new, whose key-store check wipes a
            // partially present lair.
            // Sidecars a dead earlier launch left behind (macOS kills a
            // running app whose bundle was replaced; crashes; forced quits)
            // would still hold the conductor port. Stop them before anything
            // of ours spawns. A second live ProofPoll keeps its own.
            let reaped = instance_guard::reap_orphaned_sidecars();
            if reaped > 0 {
                log::warn!("[instance] {} orphaned sidecar(s) from an earlier launch stopped", reaped);
            }

            let live_identity = tauri::async_runtime::block_on(commands::vault_live_identity())
                .and_then(|(unlocked, key)| if unlocked { key } else { None });
            let profile_root = profiles::select_profile_root(&data_dir, live_identity.as_deref());
            log::info!("Profile root: {:?}", profile_root);
            if let Some(fix) = WEBVIEW_FIX.get() {
                log::info!("[webview] {:?}", fix);
            }

            let app_state = Arc::new(AppState::new_with_device_root(profile_root, data_dir.clone()));
            let relaunched = vault_probe::take_relaunch_marker(&data_dir);
            app_state
                .relaunched_into_identity
                .store(relaunched, std::sync::atomic::Ordering::Relaxed);
            app.manage(app_state.clone());
            #[cfg(feature = "harness")]
            harness::spawn(app.handle().clone());
            // The app's own updates: a first check after 90 s, then every
            // few hours.
            updater::spawn(app.handle().clone(), app_state.device_root.clone());
            if relaunched {
                // app.restart() leaves focus with whatever was behind the
                // old window (the Vault, mostly). Come back to the front now
                // and again once the conductor is ready (below).
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }

            // SIGTERM / SIGINT (Ctrl+C, shutdown, `kill <pid>`, a package
            // upgrade) route through app.exit() so RunEvent::Exit below
            // stops the conductor and key store with the app.
            let signal_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigterm = match signal(SignalKind::terminate()) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to register SIGTERM handler: {}", e);
                            return;
                        }
                    };
                    tokio::select! {
                        _ = sigterm.recv() => log::info!("SIGTERM received, exiting"),
                        _ = tokio::signal::ctrl_c() => log::info!("SIGINT received, exiting"),
                    }
                }
                #[cfg(not(unix))]
                {
                    if tokio::signal::ctrl_c().await.is_err() {
                        log::error!("Failed to register Ctrl+C handler");
                        return;
                    }
                    log::info!("Ctrl+C received, exiting");
                }
                signal_app_handle.exit(0);
            });

            // Resolve the resource directory where the .happ bundle lives.
            // In dev mode, Tauri doesn't copy resources to target/debug/,
            // so point directly at the source resources/ folder.
            // In release, Tauri's resource_dir() returns the app's root
            // install directory; entries declared as `resources/foo` in
            // tauri.conf.json land under a `resources/` subdir there, so
            // we append that to match dev's layout.
            let resource_dir = resolve_resource_dir(app.handle(), &app_state.device_root);
            let (kit_device_root, kit_resources) = (app_state.device_root.clone(), resource_dir.clone());

            // Auto-start the conductor in the background.
            let startup_state = app_state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                {
                    let mut status = startup_state.conductor_status.lock().unwrap();
                    *status = conductor::ConductorStatus::Starting {
                        message: "Initializing...".into(),
                    };
                }

                let passphrase = startup_state.passphrase.lock().unwrap().clone();
                let data_dir = startup_state.data_dir.clone();

                let monitor_handle = app_handle.clone();
                let migration_handle = app_handle.clone();
                match conductor::start_holochain(
                    app_handle,
                    startup_state.clone(),
                    data_dir,
                    resource_dir,
                    passphrase,
                )
                .await
                {
                    Ok(result) => {
                        log::info!("Conductor started, agent: {}", result.agent_key);

                        // The Windows console-hide threads briefly steal focus
                        // away from the main ProofPoll window when they
                        // ShowWindow(SW_HIDE) the conhost windows. Re-raise our
                        // main window after the hide threads have settled
                        // (their 3 s budget runs in parallel with conductor
                        // init — by the time we reach this point they're done).
                        // No-op on non-Windows platforms.
                        // Also after "Open as this identity" on every
                        // platform: the relaunch must end up in front.
                        if cfg!(target_os = "windows")
                            || startup_state
                                .relaunched_into_identity
                                .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            use tauri::Manager;
                            if let Some(win) = monitor_handle.get_webview_window("main") {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }

                        let admin_port = result.handle.admin_port_07;
                        let app_port = result.handle.app_port_07;
                        let conductor_pid = result.handle.conductor_pid;
                        let conductor_pid_07 = result.handle.conductor_pid_07;
                        let needs_migration = result.needs_migration;
                        let v1_3_available = result.app_client_v1_3.is_some();

                        *startup_state.conductor_handle.lock().unwrap() = Some(result.handle);
                        *startup_state.agent_pub_key.lock().unwrap() = Some(result.agent_key);
                        *startup_state.agent_pub_key_07.lock().unwrap() = Some(result.agent_key_07);
                        *startup_state.app_client.lock().await = Some(result.app_client);
                        *startup_state.app_client_v1_3.lock().await = result.app_client_v1_3;
                        *startup_state.app_client_v1_2.lock().await = result.app_client_v1_2;
                        *startup_state.app_client_v1_1.lock().await = result.app_client_v1_1;
                        *startup_state.app_client_v1_0.lock().await = result.app_client_v1_0;
                        *startup_state.lair_client.lock().await = Some(result.lair_client);
                        *startup_state.conductor_status.lock().unwrap() =
                            conductor::ConductorStatus::Ready { admin_port, app_port };

                        // Each line's kit, set aside once a run.
                        kits::refresh_once(kit_device_root.clone(), kit_resources.clone());

                        // Started with no network: reconnect by itself once
                        // the rendezvous answers.
                        conductor::spawn_reconnect_when_online(
                            startup_state.clone(),
                            monitor_handle.clone(),
                        );

                        // Start background health monitor
                        conductor::spawn_health_monitor(
                            conductor_pid,
                            startup_state.clone(),
                            monitor_handle.clone(),
                        );
                        conductor::spawn_health_monitor(
                            conductor_pid_07,
                            startup_state.clone(),
                            monitor_handle,
                        );

                        // Run migration if the previous version is installed and
                        // migration hasn't completed yet.
                        let should_migrate = needs_migration || {
                            let ms = startup_state.migration_state.lock().await;
                            v1_3_available
                                && ms.status != migration::MigrationStatus::Complete
                        };
                        if should_migrate {
                            let migration_state = startup_state.clone();
                            tauri::async_runtime::spawn(async move {
                                log::info!("Starting migration to {}...", dna::ACTIVE_APP_ID);
                                match migration::run_migration(
                                    &migration_state,
                                    &migration_handle,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        log::info!("Migration completed successfully");
                                        if let Err(e) = migration::carry_private(&migration_state).await {
                                            log::warn!("Drafts and vote reasons not copied this run: {}", e);
                                        }
                                        // Start retry loop for pending votes
                                        migration::spawn_migration_retry_loop(
                                            migration_state,
                                            migration_handle,
                                        );
                                    }
                                    Err(e) => {
                                        log::error!("Migration failed: {}", e);
                                        let mut ms = migration_state.migration_state.lock().await;
                                        ms.status = migration::MigrationStatus::Error(e);
                                        ms.save(&migration_state.data_dir);
                                    }
                                }
                            });
                        } else {
                            // Installs that migrated on 0.5.0 left their 0.4.x
                            // drafts and vote reasons on v1.3: copy them once.
                            let carry_needed = {
                                let ms = startup_state.migration_state.lock().await;
                                v1_3_available && !ms.private_carried
                            };
                            if carry_needed {
                                let carry_state = startup_state.clone();
                                tauri::async_runtime::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                                    if let Err(e) = migration::carry_private(&carry_state).await {
                                        log::warn!("Drafts and vote reasons not copied this run: {}", e);
                                    }
                                });
                            }
                            // Check if there are pending votes from a previous run
                            let has_pending = {
                                let ms = startup_state.migration_state.lock().await;
                                !ms.votes_pending.is_empty()
                            };
                            if has_pending {
                                let retry_state = startup_state.clone();
                                migration::spawn_migration_retry_loop(
                                    retry_state,
                                    migration_handle,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to start conductor: {}", e);
                        let mut status = startup_state.conductor_status.lock().unwrap();
                        *status = conductor::ConductorStatus::Error { message: e };
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ── Infrastructure (keep as-is) ───────────────────────
            commands::get_app_status,
            commands::launch_vault,
            vault_probe::probe_vault,
            vault_probe::restart_app,
            vault_probe::test_vault_url,
            updater::update_status,
            updater::set_update_prefs,
            updater::dismiss_updated,
            updater::restart_to_update,
            // ── Your app commands (replace these) ─────────────────
            commands::create_poll,
            commands::get_poll,
            commands::get_all_polls,
            commands::delete_poll,
            commands::cast_vote,
            commands::get_poll_votes,
            // ── Community moderation (keep or adapt) ──────────────
            commands::flag_poll,
            commands::get_poll_flags,
            commands::remove_flag,
            commands::get_flag_threshold,
            // ── Flowsta identity linking (keep as-is) ─────────────
            commands::get_cached_profile,
            commands::save_profile_cache,
            commands::commit_identity_link,
            commands::get_linked_agents,
            commands::get_linked_agents_active,
            commands::get_my_agent_set,
            commands::get_identity_link,
            commands::revoke_identity_link,
            // ── Data export + migration (keep as-is) ──────────────
            commands::get_export_data,
            commands::get_migration_status,
            commands::abandon_pending_votes,
            // ── CAL-compliant backup + reinstall recovery ──────────
            // See the lair recovery notes
            commands::decode_record_for_export,
            commands::build_canonical_backup,
            commands::report_backup_outcome,
            commands::get_last_backup_outcome,
            // ── Agent-seed escrow: adopt / re-key (authorship survives
            //    machine death) ─────────────────────────────────────
            seed_adopt::get_seed_escrow_state,
            seed_adopt::adopt_escrowed_seed,
            seed_adopt::rekey_device_agent,
            // ── Encrypted entries (v1.3) ───────────────────────────
            commands::save_vote_rationale,
            commands::get_vote_rationale,
            commands::save_draft_poll,
            commands::get_my_drafts,
            commands::publish_draft,
            commands::delete_draft,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Every way out (window close, Quit, app.restart() for "Open as
            // this identity", the signal route above) ends here: stop the
            // conductor and key store so they never outlive the app. Linux
            // also has PR_SET_PDEATHSIG; macOS and Windows have only this
            // plus the sweep at the next launch.
            if let tauri::RunEvent::Exit = event {
                // A downloaded update installs on quit (Windows, macOS); it
                // stops the conductors cleanly itself first.
                if let Some(state) = app_handle.try_state::<Arc<AppState>>() {
                    updater::install_on_quit(app_handle, &state.device_root);
                }
                let handle = app_handle
                    .try_state::<Arc<AppState>>()
                    .and_then(|s| s.conductor_handle.lock().ok().and_then(|mut h| h.take()));
                if let Some(h) = handle {
                    h.shutdown();
                }
            }
        });
}

/// Where the bundled resources (.happ) live. `resource_dir()` can fail
/// outright on some Windows boxes (custom-drive install, AV interference -
/// the same field failure Vault and Your Own AI hit and fixed). A panic
/// here killed the app at startup with no window; fall back to the
/// exe-relative resources dir, then the data dir - loudly, so the log
/// names the real problem.
#[cfg(debug_assertions)]
pub(crate) fn resolve_resource_dir(
    _app_handle: &tauri::AppHandle,
    _data_dir: &std::path::Path,
) -> std::path::PathBuf {
    // Dev mode: Tauri doesn't copy resources to target/debug/, point at the
    // source resources/ folder.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources")
}

#[cfg(not(debug_assertions))]
pub(crate) fn resolve_resource_dir(
    app_handle: &tauri::AppHandle,
    data_dir: &std::path::Path,
) -> std::path::PathBuf {
    use tauri::Manager;
    match app_handle.path().resource_dir() {
        Ok(d) => return d.join("resources"),
        Err(e) => log::warn!(
            "[resources] resource_dir() failed: {} - trying exe-relative resources",
            e
        ),
    }
    match std::env::current_exe() {
        Ok(exe) => {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("resources");
                if candidate.is_dir() {
                    log::warn!("[resources] using exe-relative resources at {:?}", candidate);
                    return candidate;
                }
                log::warn!(
                    "[resources] exe-relative resources missing at {:?}",
                    candidate
                );
            }
        }
        Err(e) => log::warn!("[resources] current_exe() failed: {}", e),
    }
    log::warn!(
        "[resources] falling back to data dir {:?} - bundled DNAs will only be found if copied there",
        data_dir
    );
    data_dir.to_path_buf()
}
