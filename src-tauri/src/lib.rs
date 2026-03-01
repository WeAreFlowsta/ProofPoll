mod commands;
mod conductor;
mod dna;
mod lair;

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

                match conductor::start_holochain(
                    app_handle,
                    data_dir,
                    resource_dir,
                    passphrase,
                )
                .await
                {
                    Ok((handle, agent_key, app_client)) => {
                        log::info!("Conductor started, agent: {}", agent_key);
                        let admin_port = handle.admin_port;
                        let app_port = handle.app_port;

                        *startup_state.conductor_handle.lock().unwrap() = Some(handle);
                        *startup_state.agent_pub_key.lock().unwrap() = Some(agent_key);
                        *startup_state.app_client.lock().await = Some(app_client);
                        *startup_state.conductor_status.lock().unwrap() =
                            conductor::ConductorStatus::Ready { admin_port, app_port };
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
            commands::get_app_status,
            commands::create_poll,
            commands::get_poll,
            commands::get_all_polls,
            commands::cast_vote,
            commands::get_poll_votes,
            commands::commit_identity_link,
            commands::get_linked_agents,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
