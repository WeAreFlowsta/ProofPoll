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
mod lair;
pub mod migration;

use commands::AppState;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data directory");
            std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");

            log::info!("ProofPoll starting up...");
            log::info!("Data dir: {:?}", data_dir);

            let app_state = Arc::new(AppState::new(data_dir));
            app.manage(app_state.clone());

            // Resolve the resource directory where the .happ bundle lives.
            // In dev mode, Tauri doesn't copy resources to target/debug/,
            // so point directly at the source resources/ folder.
            #[cfg(debug_assertions)]
            let resource_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources");
            #[cfg(not(debug_assertions))]
            let resource_dir = app
                .path()
                .resource_dir()
                .expect("Failed to get resource directory");

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
                    data_dir,
                    resource_dir,
                    passphrase,
                )
                .await
                {
                    Ok(result) => {
                        log::info!("Conductor started, agent: {}", result.agent_key);
                        let admin_port = result.handle.admin_port;
                        let app_port = result.handle.app_port;
                        let conductor_pid = result.handle.conductor_pid;
                        let needs_migration = result.needs_migration;
                        let v1_2_available = result.app_client_v1_2.is_some();

                        *startup_state.conductor_handle.lock().unwrap() = Some(result.handle);
                        *startup_state.agent_pub_key.lock().unwrap() = Some(result.agent_key);
                        *startup_state.app_client.lock().await = Some(result.app_client);
                        *startup_state.app_client_v1_2.lock().await = result.app_client_v1_2;
                        *startup_state.app_client_v1_1.lock().await = result.app_client_v1_1;
                        *startup_state.app_client_v1_0.lock().await = result.app_client_v1_0;
                        *startup_state.lair_client.lock().await = Some(result.lair_client);
                        *startup_state.conductor_status.lock().unwrap() =
                            conductor::ConductorStatus::Ready { admin_port, app_port };

                        // Start background health monitor
                        conductor::spawn_health_monitor(
                            conductor_pid,
                            startup_state.clone(),
                            monitor_handle,
                        );

                        // Run migration if the previous version is installed and
                        // migration hasn't completed yet.
                        let should_migrate = needs_migration || {
                            let ms = startup_state.migration_state.lock().await;
                            v1_2_available
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
            commands::get_identity_link,
            commands::revoke_identity_link,
            // ── Data export + migration (keep as-is) ──────────────
            commands::get_export_data,
            commands::get_migration_status,
            commands::abandon_pending_votes,
            // ── Encrypted entries (v1.3) ───────────────────────────
            commands::save_vote_rationale,
            commands::get_vote_rationale,
            commands::save_draft_poll,
            commands::get_my_drafts,
            commands::publish_draft,
            commands::delete_draft,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
