//! The test harness (cargo feature `harness`, never in a release build): a
//! localhost server whose `/dev` routes drive this instance through the same
//! commands the UI calls, for the headless matrix (`scripts/pp-matrix.mjs`).
//! On only when `PROOFPOLL_HARNESS_PORT` is set.

use axum::{
    extract::State as Ax,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Manager;

use crate::commands::{self, AppState};

pub fn spawn(app: tauri::AppHandle) {
    let Some(port) = std::env::var("PROOFPOLL_HARNESS_PORT").ok().and_then(|v| v.trim().parse::<u16>().ok()) else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let router = Router::new()
            .route("/dev/status", get(status))
            .route("/dev/op", post(op))
            .with_state(app);
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                log::warn!("[harness] /dev routes on 127.0.0.1:{}", port);
                if let Err(e) = axum::serve(listener, router).await {
                    log::error!("[harness] server stopped: {}", e);
                }
            }
            Err(e) => log::error!("[harness] could not bind 127.0.0.1:{}: {}", port, e),
        }
    });
}

type Reply = (StatusCode, Json<Value>);

fn ok(v: Value) -> Reply {
    (StatusCode::OK, Json(v))
}

fn failed(e: impl std::fmt::Display) -> Reply {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() })))
}

fn to_value<T: serde::Serialize>(r: Result<T, String>) -> Reply {
    match r {
        Ok(v) => ok(serde_json::to_value(v).unwrap_or(Value::Null)),
        Err(e) => failed(e),
    }
}

/// Resident memory of a process in MB (Linux; the matrix runs there).
fn rss_mb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let kb = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(kb / 1024)
}

async fn status(Ax(app): Ax<tauri::AppHandle>) -> Reply {
    let state = app.state::<Arc<AppState>>();
    let app_status = commands::get_app_status(state.clone());
    use crate::hc::Line;
    let pids = crate::conductor::line_pid(Line::V06).zip(crate::conductor::line_pid(Line::V07));
    let app_ports = crate::conductor::line_ports(Line::V06).map(|p| p.1).zip(crate::conductor::line_ports(Line::V07).map(|p| p.1));
    let migration = commands::get_migration_status(state.clone()).await.ok();
    let link = commands::get_identity_link(state.clone());
    let configs_with_auth = ["conductor", crate::conductor::CONDUCTOR_DIR_07]
        .iter()
        .filter(|d| {
            std::fs::read_to_string(state.data_dir.join(d).join("conductor-config.yaml"))
                .map(|c| c.contains("base64_auth_material"))
                .unwrap_or(false)
        })
        .count();
    ok(json!({
        "harness": true,
        "version": app.package_info().version.to_string(),
        "app": app_status,
        "started_offline": crate::conductor::started_offline(),
        "configs_with_auth": configs_with_auth,
        "admin_ports": [crate::conductor::admin_port(), crate::conductor::admin_port_07()],
        "app_ports": app_ports.map(|(a, b)| json!([a, b])),
        "conductor_pids": pids.map(|(a, b)| json!([a, b])),
        "rss_mb": pids.map(|(a, b)| json!({ "holochain-0.6": rss_mb(a), "holochain-0.7": rss_mb(b) })),
        "identity_link": link,
        "kits": crate::kits::status(&state.device_root),
        "migration": migration,
        "restore_marker": crate::seed_adopt::read_restore_marker(&state.data_dir),
    }))
}

/// This device's v1.4 or v1.5 chain (`dna`) as its 0.7 conductor holds it:
/// the app's status, the actions it authored, the head seq, and any seq
/// held twice (a fork).
async fn chain07(dna: &str) -> Reply {
    let app_id = if dna == "1.5" { crate::dna::APP_ID_V1_5 } else { crate::dna::APP_ID_V1_4 };
    use holochain_client_07::{AdminWebsocket, CellInfo};
    let admin = match AdminWebsocket::connect(
        format!("localhost:{}", crate::conductor::admin_port_07()),
        Some(crate::conductor::node_origin()),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return failed(format!("0.7 admin: {}", e)),
    };
    let apps = match admin.list_apps(None).await {
        Ok(a) => a,
        Err(e) => return failed(format!("list_apps: {}", e)),
    };
    let Some(app) = apps.iter().find(|a| a.installed_app_id == app_id) else {
        return failed(format!("v{} is not installed", dna));
    };
    let status = format!("{:?}", app.status);
    let Some(cell_id) = app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
        CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
        _ => None,
    }) else {
        return failed(format!("no v{} cell", dna));
    };
    let dump = match admin.dump_full_state(cell_id, None, None).await {
        Ok(d) => d,
        Err(e) => return ok(json!({ "status": status, "dump_error": e.to_string() })),
    };
    let mut per_seq: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for rec in &dump.source_chain_dump.records {
        *per_seq.entry(rec.action.action_seq()).or_insert(0) += 1;
    }
    let forked: Vec<u32> = per_seq.iter().filter(|(_, n)| **n > 1).map(|(s, _)| *s).collect();
    ok(json!({
        "status": status,
        "actions": dump.source_chain_dump.records.len(),
        "head_seq": per_seq.keys().next_back(),
        "forked_seqs": forked,
    }))
}

/// Whether 0.7-side code can check a v1.3 record's author with no 0.6
/// network:
/// For each app entry this install wrote on v1.3:
/// 1. the action as Holochain 0.6.1 signs it (`encode`, the bytes lair signs);
/// 2. the author's signature over those bytes;
/// 3. those bytes read back with a mirror of the 0.6 action shape built from
///    0.7 types only (0.7 changed the action shape, so a v1.5 validator has
///    no 0.6 `Action` to decode into);
/// 4. the entry hashed with 0.7's hashing, against the hash the action names.
async fn proof_check() -> Reply {
    use holochain_client::{AdminWebsocket, CellInfo};
    use holochain_types::prelude::{encode, SerializedBytes, UnsafeBytes};

    /// The 0.6 action, as far as a check needs it, in 0.7 types.
    #[derive(serde::Deserialize)]
    #[serde(tag = "type")]
    enum Action06 {
        Create(Create06),
        #[serde(other)]
        Other,
    }
    #[derive(serde::Deserialize)]
    struct Create06 {
        author: holochain_types_07::prelude::AgentPubKey,
        entry_hash: holochain_types_07::prelude::EntryHash,
    }

    let admin = match AdminWebsocket::connect(format!("localhost:{}", crate::conductor::admin_port()), Some(crate::conductor::node_origin())).await {
        Ok(a) => a,
        Err(e) => return failed(format!("0.6 admin: {}", e)),
    };
    let apps = match admin.list_apps(None).await {
        Ok(a) => a,
        Err(e) => return failed(format!("list_apps: {}", e)),
    };
    let Some(cell_id) = apps
        .iter()
        .find(|a| a.installed_app_id == crate::dna::APP_ID_V1_3)
        .and_then(|app| app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
            CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
            _ => None,
        }))
    else {
        return failed("no v1.3 cell");
    };
    let dump = match admin.dump_full_state(cell_id, None).await {
        Ok(d) => d,
        Err(e) => return failed(format!("dump_full_state: {}", e)),
    };
    let mut checked = Vec::new();
    for rec in &dump.source_chain_dump.records {
        let Some(entry) = rec.entry.as_ref().and_then(|e| e.as_app_entry()) else { continue };
        let entry_bytes = entry.as_ref().bytes().to_vec();
        let signed = match encode(&rec.action) {
            Ok(b) => b,
            Err(e) => return failed(format!("encode: {}", e)),
        };
        let author: [u8; 32] = rec.action.author().get_raw_32().try_into().unwrap_or([0u8; 32]);
        let signature_verifies = ed25519_dalek::VerifyingKey::from_bytes(&author)
            .map(|key| key.verify_strict(&signed, &ed25519_dalek::Signature::from_bytes(&rec.signature.0)).is_ok())
            .unwrap_or(false);
        let (mirror, author_matches, entry_hash_matches) = match rmp_serde::from_slice::<Action06>(&signed) {
            Ok(Action06::Create(c)) => {
                use holochain_integrity_types_07::entry::{AppEntryBytes, Entry};
                let hash = holochain_types_07::prelude::EntryHash::with_data_sync(&Entry::App(AppEntryBytes(SerializedBytes::from(UnsafeBytes::from(entry_bytes.clone())))));
                ("Create".to_string(), c.author.get_raw_32() == author, hash == c.entry_hash)
            }
            Ok(Action06::Other) => ("not a Create".to_string(), false, false),
            Err(e) => (format!("did not decode: {}", e), false, false),
        };
        checked.push(json!({
            "action_hash": rec.action_address.to_string(),
            "signature_verifies": signature_verifies,
            "mirror": mirror,
            "mirror_author_matches": author_matches,
            "entry_hash_matches": entry_hash_matches,
            "signed_bytes": signed.len(),
            "entry_bytes": entry_bytes.len(),
        }));
    }
    ok(json!({ "records": checked }))
}

/// Test fixtures for the v1.5 DNA's carried-record validation: every app
/// record this install authored on v1.3 (0.6) and v1.4 (0.7), as signed:
/// the action's exact signed bytes, the signature and the entry (hex), with
/// the action hash, author, time (microseconds) and the entry's zome and
/// entry index. Test identities only; nothing here is a person's data.
async fn fixtures() -> Reply {
    use holochain_types::prelude::encode;
    let mut v13 = Vec::new();
    {
        use holochain_client::{AdminWebsocket, CellInfo};
        use holochain_integrity_types::EntryType;
        let admin = match AdminWebsocket::connect(format!("localhost:{}", crate::conductor::admin_port()), Some(crate::conductor::node_origin())).await {
            Ok(a) => a,
            Err(e) => return failed(format!("0.6 admin: {}", e)),
        };
        let apps = admin.list_apps(None).await.map_err(|e| e.to_string());
        let cell = apps.ok().and_then(|apps| {
            apps.iter().find(|a| a.installed_app_id == crate::dna::APP_ID_V1_3).and_then(|app| {
                app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
                    CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                    _ => None,
                })
            })
        });
        let Some(cell_id) = cell else { return failed("no v1.3 cell") };
        let dump = match admin.dump_full_state(cell_id, None).await {
            Ok(d) => d,
            Err(e) => return failed(format!("v1.3 dump: {}", e)),
        };
        for rec in &dump.source_chain_dump.records {
            let (Some(entry), Some(EntryType::App(def))) = (rec.entry.as_ref().and_then(|e| e.as_app_entry()), rec.action.entry_type()) else { continue };
            let Ok(action) = encode(&rec.action) else { continue };
            v13.push(json!({
                "dna": "1.3",
                "action_hash": rec.action_address.to_string(),
                "author": rec.action.author().to_string(),
                "timestamp_us": rec.action.timestamp().as_micros(),
                "zome_index": def.zome_index.0,
                "entry_index": def.entry_index.0,
                "action": hex::encode(&action),
                "signature": hex::encode(rec.signature.0),
                "entry": hex::encode(entry.as_ref().bytes()),
            }));
        }
    }
    let mut v14 = Vec::new();
    {
        use holochain_client_07::{AdminWebsocket, CellInfo};
        use holochain_integrity_types_07::action::EntryType;
        let admin = match AdminWebsocket::connect(format!("localhost:{}", crate::conductor::admin_port_07()), Some(crate::conductor::node_origin())).await {
            Ok(a) => a,
            Err(e) => return failed(format!("0.7 admin: {}", e)),
        };
        let apps = admin.list_apps(None).await.map_err(|e| e.to_string());
        let cell = apps.ok().and_then(|apps| {
            apps.iter().find(|a| a.installed_app_id == crate::dna::APP_ID_V1_4).and_then(|app| {
                app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
                    CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                    _ => None,
                })
            })
        });
        let Some(cell_id) = cell else { return failed("no v1.4 cell") };
        let dump = match admin.dump_full_state(cell_id, None, None).await {
            Ok(d) => d,
            Err(e) => return failed(format!("v1.4 dump: {}", e)),
        };
        for rec in &dump.source_chain_dump.records {
            let (Some(entry), Some(EntryType::App(def))) = (rec.entry.as_ref().and_then(|e| e.as_app_entry()), rec.action.entry_type()) else { continue };
            let Ok(action) = encode(&rec.action) else { continue };
            v14.push(json!({
                "dna": "1.4",
                "action_hash": rec.action_address.to_string(),
                "author": rec.action.author().to_string(),
                "timestamp_us": rec.action.timestamp().as_micros(),
                "zome_index": def.zome_index.0,
                "entry_index": def.entry_index.0,
                "action": hex::encode(&action),
                "signature": hex::encode(rec.signature.0),
                "entry": hex::encode(entry.as_ref().bytes()),
            }));
        }
    }
    ok(json!({ "v13": v13, "v14": v14 }))
}

/// A v1.5 getter's records as v1.5 alone holds them (no older line read):
/// each with its writer, the author its proof names when it was carried, and
/// its entry's fields.
async fn v15_records(state: &AppState, f: &str, hash: Option<String>) -> Reply {
    #[derive(serde::Deserialize)]
    struct Carried {
        #[serde(default)]
        carried_from: Option<crate::carried::Carried>,
    }
    /// The fields a test reads, whichever kind the record is.
    #[derive(serde::Deserialize, serde::Serialize)]
    struct Fields {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        option_index: Option<u32>,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        reason: Option<Value>,
    }
    let Some(client) = state.app_client.lock().await.clone() else { return failed("the v1.5 client is not connected") };
    let payload = match hash {
        Some(h) => crate::hc::action_hash(&h).and_then(|h| crate::hc::encode(&h)),
        None => crate::hc::encode(&()),
    };
    let records = match payload {
        Ok(p) => client.call_zome(crate::names::ROLE_NAME, "polls", f, p).await.and_then(|b| crate::hc::records(client.line(), &b)),
        Err(e) => Err(e),
    };
    match records {
        Ok(records) => ok(json!(records
            .iter()
            .map(|r| {
                let from = r.entry_as::<Carried>().ok().and_then(|c| c.carried_from);
                json!({
                    "hash": r.action_hash,
                    "writer": r.author,
                    "carried_author": from.as_ref().map(|c| c.original_author.to_string()),
                    "carried_dna": from.as_ref().map(|c| c.original_dna.clone()),
                    "carried_hash": from.as_ref().map(|c| c.original_action_hash.to_string()),
                    "fields": r.entry_as::<Fields>().ok(),
                })
            })
            .collect::<Vec<_>>())),
        Err(e) => failed(e),
    }
}

fn s(body: &Value, key: &str) -> String {
    body.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn opt_s(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

async fn op(Ax(app): Ax<tauri::AppHandle>, Json(body): Json<Value>) -> Reply {
    let state = app.state::<Arc<AppState>>();
    let options = || -> Vec<String> {
        body.get("options")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let closes_at = body.get("closes_at").and_then(|v| v.as_i64());
    match s(&body, "op").as_str() {
        "create_poll" => to_value(
            commands::create_poll(state, s(&body, "title"), s(&body, "description"), options(), closes_at, opt_s(&body, "poll_type")).await,
        ),
        "polls" => to_value(commands::get_all_polls(state).await),
        "poll" => to_value(commands::get_poll(state, s(&body, "poll")).await),
        "vote" => to_value(
            commands::cast_vote(
                state,
                s(&body, "poll"),
                body.get("option").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                opt_s(&body, "dna_version").unwrap_or_else(|| "1.5".into()),
                opt_s(&body, "poll_type"),
            )
            .await,
        ),
        "votes" => to_value(commands::get_poll_votes(state, s(&body, "poll"), opt_s(&body, "dna_version").unwrap_or_else(|| "1.5".into())).await),
        "save_draft" => to_value(
            commands::save_draft_poll(state, s(&body, "title"), s(&body, "description"), options(), closes_at, opt_s(&body, "poll_type")).await,
        ),
        "drafts" => to_value(commands::get_my_drafts(state).await),
        "publish_draft" => to_value(commands::publish_draft(state, s(&body, "draft")).await),
        "save_rationale" => to_value(commands::save_vote_rationale(state, s(&body, "vote"), s(&body, "text")).await),
        "rationale" => to_value(commands::get_vote_rationale(state, s(&body, "vote")).await),
        "identity" => ok(serde_json::to_value(commands::get_identity_link(state)).unwrap_or(Value::Null)),
        "migration" => to_value(commands::get_migration_status(state).await),
        "link" => link(&app, state, s(&body, "client_id")).await,
        // A sign-in as 0.4.x made it: the link on v1.3 only.
        "v13_link" => v13_link(state, s(&body, "client_id")).await,
        // The layout's check: v1.5 links the key the active network runs
        // as to the signed-in identity (own or carried link).
        "linked_active" => {
            let st = commands::get_app_status(state.clone());
            match st.agent_pub_key_07.or(st.agent_pub_key).map(holochain_types::prelude::AgentPubKey::try_from) {
                Some(Ok(key)) => to_value(commands::identity_linked_on_active_inner(state.inner(), &key).await),
                Some(Err(e)) => failed(format!("{:?}", e)),
                None => failed("no local agent yet"),
            }
        }
        // As the UI asks it: from this install's own agent key.
        "agent_set" => {
            let local = commands::get_app_status(state.clone()).agent_pub_key;
            to_value(commands::get_my_agent_set(state, local).await)
        }
        // ProofPoll 0.4.x's shapes on v1.3, for the migration leg.
        "v13_poll" => to_value(commands::harness_v13_poll(&state, s(&body, "title"), s(&body, "description"), options(), closes_at, opt_s(&body, "poll_type")).await),
        "v13_flag" => to_value(commands::harness_v13_flag(&state, s(&body, "poll"), opt_s(&body, "reason").unwrap_or_else(|| "Spam".into())).await),
        "v13_delete" => to_value(commands::harness_v13_remove(&state, "delete_poll", s(&body, "poll")).await),
        "v13_unflag" => to_value(commands::harness_v13_remove(&state, "remove_flag", s(&body, "flag")).await),
        "v15_records" => v15_records(&state, &s(&body, "fn"), opt_s(&body, "hash")).await,
        // This start's admin ports and origin, so a test can run the node's
        // carrier against this instance's conductors.
        "node_admin" => ok(json!({
            "admin_06": crate::conductor::admin_port(),
            "admin_07": crate::conductor::admin_port_07(),
            "origin": crate::conductor::node_origin(),
        })),
        "v13_draft" => {
            let draft = json!({
                "title": s(&body, "title"),
                "description": s(&body, "description"),
                "options": options(),
                "closes_at": closes_at,
                "poll_type": "Anonymous",
            });
            to_value(commands::harness_v13_encrypted(&state, "draft_poll", None, draft.to_string().into_bytes()).await)
        }
        "v13_rationale" => to_value(
            commands::harness_v13_encrypted(&state, "vote_rationale", Some(s(&body, "vote")), s(&body, "text").into_bytes()).await,
        ),
        // The migration again, as a start does it: from NotStarted, then the
        // drafts and reasons.
        "migrate" => {
            {
                let mut ms = state.migration_state.lock().await;
                *ms = crate::migration::MigrationState::default();
                ms.save(&state.data_dir);
            }
            let arc = state.inner().clone();
            if let Err(e) = crate::migration::run_migration(&arc, &app).await {
                return failed(format!("migration: {}", e));
            }
            let carried = crate::migration::carry_private(&arc).await;
            let after = serde_json::to_value(&*arc.migration_state.lock().await).unwrap_or(Value::Null);
            to_value(carried.map(|(drafts, reasons)| json!({ "drafts": drafts, "reasons": reasons, "state": after })))
        }
        // One run of the move to v1.5 (carry_move.rs), as if left open.
        "move15" => {
            let arc = state.inner().clone();
            crate::carry_move::reopen(&arc.data_dir);
            to_value(crate::carry_move::run(&arc).await.map(|r| json!({ "report": r, "complete": crate::carry_move::MoveState::load(&arc.data_dir).complete })))
        }
        // The canonical backup into the pinned Vault, as the SDK's backup
        // posts it (it carries this install's escrowed seed).
        "backup" => backup(state, s(&body, "client_id")).await,
        "chain07" => chain07(&opt_s(&body, "dna").unwrap_or_else(|| "1.4".into())).await,
        // The start screen's "Continue without them" during a restore that
        // came back shorter than the backup.
        "restore_try_again" => {
            crate::dna::RESTORE_TRY_AGAIN.store(true, std::sync::atomic::Ordering::SeqCst);
            ok(json!({ "asked": true }))
        }
        "restore_go_on" => {
            crate::dna::RESTORE_ACCEPT_SHORT.store(true, std::sync::atomic::Ordering::SeqCst);
            ok(json!({ "accepted": true }))
        }
        "proof_check" => proof_check().await,
        "v13_vote" => to_value(
            commands::harness_v13_vote(&state, s(&body, "poll"), body.get("option").and_then(|v| v.as_u64()).unwrap_or(0) as u32, opt_s(&body, "display_name")).await,
        ),
        "fixtures" => fixtures().await,
        // What the Vault backup holds for this app: the escrowed seed or none.
        "escrow" => {
            use crate::seed_adopt::EscrowProbe;
            let probe = crate::seed_adopt::VaultEscrowProbe { client_id: s(&body, "client_id") };
            match probe.escrowed_seed().await {
                Ok(crate::seed_adopt::EscrowedSeed::Seed(_)) => ok(json!({ "seed": true })),
                Ok(crate::seed_adopt::EscrowedSeed::NoBackup) => ok(json!({ "seed": false, "backup": "none" })),
                Ok(crate::seed_adopt::EscrowedSeed::DataOnly) => ok(json!({ "seed": false, "backup": "data only" })),
                Err(e) => failed(e),
            }
        }
        // The machine-death restore: adopt the seed escrowed in the Vault
        // backup. A debug build exits after it (the script relaunches).
        "adopt" => {
            use crate::seed_adopt::EscrowProbe;
            let probe = crate::seed_adopt::VaultEscrowProbe { client_id: s(&body, "client_id") };
            let hex = match probe.escrowed_seed().await {
                Ok(crate::seed_adopt::EscrowedSeed::Seed(hex)) => hex,
                Ok(_) => return failed("no escrowed seed in the Vault backup"),
                Err(e) => return failed(e),
            };
            to_value(crate::seed_adopt::adopt_escrowed_seed(app.clone(), state, hex, true, Some(s(&body, "client_id"))).await.map(|_| json!({ "adopted": true })))
        }
        other => failed(format!("unknown op {:?}", other)),
    }
}

async fn backup(state: tauri::State<'_, Arc<AppState>>, client_id: String) -> Reply {
    let Some(port) = crate::vault_probe::test_vault_port() else {
        return failed("no PROOFPOLL_TEST_VAULT_PORT");
    };
    let expected = commands::get_identity_link(state.clone()).map(|l| l.vault_agent_pub_key);
    let data = match commands::build_canonical_backup(state, Some(client_id.clone())).await {
        Ok(d) => d,
        Err(e) => return failed(format!("build_canonical_backup: {}", e)),
    };
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/backup", port))
        .header("Origin", crate::vault_probe::webview_origin())
        .json(&json!({ "client_id": client_id, "app_name": crate::names::APP_NAME, "data": data, "expected_identity": expected }))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => ok(r.json().await.unwrap_or(Value::Null)),
        Ok(r) => failed(format!("backup {}: {}", r.status(), r.text().await.unwrap_or_default())),
        Err(e) => failed(e),
    }
}

/// The Vault's signature for this install's key, as `/link-identity` gives
/// it to the SDK: (identity key, signature).
async fn vault_signs_link(agent: &str, client_id: &str) -> Result<(String, String), String> {
    let base = match crate::vault_probe::test_vault_port() {
        Some(p) => format!("http://127.0.0.1:{}", p),
        None => return Err("no PROOFPOLL_TEST_VAULT_PORT".into()),
    };
    let r = reqwest::Client::new()
        .post(format!("{}/link-identity", base))
        .header("Origin", crate::vault_probe::webview_origin())
        .json(&json!({ "app_name": crate::names::APP_NAME, "client_id": client_id, "app_agent_pub_key": agent }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("link-identity {}: {}", r.status(), r.text().await.unwrap_or_default()));
    }
    let v: Value = r.json().await.unwrap_or(Value::Null);
    match (v.get("vault_agent_pub_key").and_then(|x| x.as_str()), v.get("vault_signature").and_then(|x| x.as_str())) {
        (Some(key), Some(sig)) => Ok((key.to_string(), sig.to_string())),
        _ => Err(format!("link-identity answered without a key and signature: {}", v)),
    }
}

/// A sign-in as ProofPoll 0.4.x made it: the Vault signs this install's
/// key and the link is written on v1.3 (and saved here), nowhere else.
async fn v13_link(state: tauri::State<'_, Arc<AppState>>, client_id: String) -> Reply {
    let Some(agent) = commands::get_app_status(state.clone()).agent_pub_key else {
        return failed("no local agent yet");
    };
    match vault_signs_link(&agent, &client_id).await {
        Ok((key, sig)) => to_value(commands::harness_v13_link(&state, key.clone(), sig).await.map(|h| json!({ "vault_agent_pub_key": key, "link": h }))),
        Err(e) => failed(e),
    }
}

/// The sign-in the identity page does through the SDK: the Vault's
/// `/link-identity` (the Vault the instance is pinned to), then the commit.
async fn link(app: &tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, client_id: String) -> Reply {
    let Some(agent) = commands::get_app_status(state.clone()).agent_pub_key else {
        return failed("no local agent yet");
    };
    let _ = app;
    match vault_signs_link(&agent, &client_id).await {
        Ok((key, sig)) => to_value(commands::commit_identity_link(state, key.clone(), sig).await.map(|h| json!({ "vault_agent_pub_key": key, "link": h }))),
        Err(e) => failed(e),
    }
}
