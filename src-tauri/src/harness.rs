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
    let (pids, app_ports) = {
        let handle = state.conductor_handle.lock().unwrap();
        match handle.as_ref() {
            Some(h) => (Some((h.conductor_pid, h.conductor_pid_07)), Some((h.app_port, h.app_port_07))),
            None => (None, None),
        }
    };
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

/// This device's v1.4 chain as its 0.7 conductor holds it: the app's status,
/// the actions it authored, the head seq, and any seq held twice (a fork).
async fn chain07() -> Reply {
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
    let Some(app) = apps.iter().find(|a| a.installed_app_id == crate::dna::APP_ID_V1_4) else {
        return failed("v1.4 is not installed");
    };
    let status = format!("{:?}", app.status);
    let Some(cell_id) = app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
        CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
        _ => None,
    }) else {
        return failed("no v1.4 cell");
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

/// The carrier's first question (`planning/PROOFPOLL_CARRIER.md`, to prove
/// 1): can 0.7-side code check a v1.3 record's author with no 0.6 network?
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
        "vote" => to_value(
            commands::cast_vote(
                state,
                s(&body, "poll"),
                body.get("option").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                opt_s(&body, "dna_version").unwrap_or_else(|| "1.4".into()),
                opt_s(&body, "poll_type"),
            )
            .await,
        ),
        "votes" => to_value(commands::get_poll_votes(state, s(&body, "poll"), opt_s(&body, "dna_version").unwrap_or_else(|| "1.4".into())).await),
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
        // As the UI asks it: from this install's own agent key.
        "agent_set" => {
            let local = commands::get_app_status(state.clone()).agent_pub_key;
            to_value(commands::get_my_agent_set(state, local).await)
        }
        // ProofPoll 0.4.x's shapes on v1.3, for the migration leg.
        "v13_poll" => to_value(commands::harness_v13_poll(&state, s(&body, "title"), s(&body, "description"), options(), closes_at).await),
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
        // The canonical backup into the pinned Vault, as the SDK's backup
        // posts it (it carries this install's escrowed seed).
        "backup" => backup(state, s(&body, "client_id")).await,
        "chain07" => chain07().await,
        "proof_check" => proof_check().await,
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
        .json(&json!({ "client_id": client_id, "app_name": "ProofPoll", "data": data, "expected_identity": expected }))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => ok(r.json().await.unwrap_or(Value::Null)),
        Ok(r) => failed(format!("backup {}: {}", r.status(), r.text().await.unwrap_or_default())),
        Err(e) => failed(e),
    }
}

/// The sign-in the identity page does through the SDK: the Vault's
/// `/link-identity` (the Vault the instance is pinned to), then the commit.
async fn link(app: &tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, client_id: String) -> Reply {
    let Some(agent) = commands::get_app_status(state.clone()).agent_pub_key else {
        return failed("no local agent yet");
    };
    let base = match crate::vault_probe::test_vault_port() {
        Some(p) => format!("http://127.0.0.1:{}", p),
        None => return failed("no PROOFPOLL_TEST_VAULT_PORT"),
    };
    let _ = app;
    let resp = reqwest::Client::new()
        .post(format!("{}/link-identity", base))
        .header("Origin", crate::vault_probe::webview_origin())
        .json(&json!({ "app_name": "ProofPoll", "client_id": client_id, "app_agent_pub_key": agent }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await;
    let v: Value = match resp {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        Ok(r) => return failed(format!("link-identity {}: {}", r.status(), r.text().await.unwrap_or_default())),
        Err(e) => return failed(e),
    };
    let (Some(key), Some(sig)) = (v.get("vault_agent_pub_key").and_then(|x| x.as_str()), v.get("vault_signature").and_then(|x| x.as_str())) else {
        return failed(format!("link-identity answered without a key and signature: {}", v));
    };
    to_value(commands::commit_identity_link(state, key.to_string(), sig.to_string()).await.map(|h| json!({ "vault_agent_pub_key": key, "link": h })))
}
