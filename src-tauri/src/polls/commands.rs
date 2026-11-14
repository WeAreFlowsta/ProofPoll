//! The poll commands: create, read, list, delete, vote.

#![allow(unused_imports)]
use crate::commands::*;
use crate::polls::*;
use holochain_types::prelude::{ActionHash, AgentPubKey};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::Mutex;

// ── Poll commands (replace with your app's commands) ──────────────────
//
// Each command follows the same pattern:
//   1. Lock the AppWebsocket from state
//   2. Parse input (convert string hashes to ActionHash)
//   3. Encode payload with ExternIO::encode()
//   4. Call call_zome() with your zome name and function
//   5. Decode the result and return a frontend-friendly type

#[tauri::command]
pub async fn create_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    title: String,
    description: String,
    options: Vec<String>,
    closes_at: Option<i64>,
    poll_type: Option<String>,
) -> Result<String, String> {
    // Tier-1 gate: signed-in install, no unlock required - but never while
    // a different identity's Vault is open. See require_identity_link.
    require_identity_link(&state).await?;

    let client = state.app_client.lock().await;
    let client = client.as_ref().ok_or("Conductor not ready")?;

    let input = CreatePollInput {
        title,
        description,
        options,
        closes_at,
        poll_type: Some(poll_type.unwrap_or_else(|| "Anonymous".to_string())),
    };
    let payload = crate::hc::encode(&input)?;
    let result = call_zome(client, POLLS_ZOME, "create_poll", payload).await?;

    let action_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(action_hash.to_string())
}

#[tauri::command]
pub async fn get_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    action_hash: String,
) -> Result<Option<PollDetail>, String> {
    let hash = ActionHash::try_from(action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    // The active version first; an older hash it has carried opens its copy.
    {
        let client = state.app_client.lock().await;
        if let Some(client) = client.as_ref() {
            let mut target = hash.clone();
            let payload = crate::hc::encode(&hash)?;
            let mut record = crate::hc::record_opt(client.line(), &call_zome(client, POLLS_ZOME, "get_poll", payload).await?)?;
            if record.is_none() {
                let mut mapped: Option<ActionHash> = crate::hc::decode(&call_zome(client, POLLS_ZOME, "get_migration_mapping", crate::hc::encode(&hash)?).await?)?;
                if mapped.is_none() {
                    // A v1.4 copy the list shows as its first record's copy.
                    let merged = state.migrated_from.lock().unwrap().as_ref().and_then(|m| {
                        m.iter().find(|(_, olds)| olds.contains(&hash.to_string())).map(|(copy, _)| copy.clone())
                    });
                    mapped = merged.and_then(|c| ActionHash::try_from(c).ok());
                }
                if let Some(copy) = mapped {
                    record = crate::hc::record_opt(client.line(), &call_zome(client, POLLS_ZOME, "get_poll", crate::hc::encode(&copy)?).await?)?;
                    target = copy;
                }
            }
            if let Some(record) = record {
                let poll: Poll = decode_entry(&record)?;
                return Ok(Some(PollDetail {
                    hash: target.to_string(),
                    author: counted_author(&record, &poll.carried_from),
                    poll,
                    dna_version: "1.5".to_string(),
                }));
            }
        }
    }

    // Then each older version, newest first.
    for (slot, version) in [
        (&state.app_client_v1_4, "1.4"),
        (&state.app_client_v1_3, "1.3"),
        (&state.app_client_v1_2, "1.2"),
        (&state.app_client_v1_1, "1.1"),
        (&state.app_client_v1_0, "1.0"),
    ] {
        let client = slot.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = crate::hc::encode(&hash.clone())?;
            let result = call_zome(client, POLLS_ZOME, "get_poll", payload).await?;
            if let Some(record) = crate::hc::record_opt(client.line(), &result)? {
                let poll: Poll = decode_entry(&record)?;
                return Ok(Some(PollDetail {
                    hash: hash.to_string(),
                    poll,
                    author: record.author.clone(),
                    dna_version: version.to_string(),
                }));
            }
        }
    }

    Ok(None)
}

#[tauri::command]
pub async fn get_all_polls(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<PollListItem>, String> {
    // Step 1: polls on the active version (v1.5), one per original: a
    // carried poll is listed for the author its proof names.
    let mut polls = Vec::new();
    // Each carried poll's original (its hash) -> the poll as asked there.
    let mut firsts = std::collections::HashMap::<String, Poll>::new();
    {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let payload = crate::hc::encode(&())?;
        let result = match call_zome_within(client, POLLS_ZOME, "get_all_polls", payload.clone(), LIST_WAIT_SECS).await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("[polls] the network list did not come ({}): showing the polls this device holds", e);
                call_zome(client, POLLS_ZOME, "get_all_polls_local", payload).await?
            }
        };
        let records = crate::hc::records(client.line(), &result)?;
        for record in &records {
            if let Ok(poll) = decode_entry::<Poll>(record) {
                if let Some(o) = &poll.carried_from {
                    firsts.insert(o.original_action_hash.to_string(), poll.clone());
                }
                polls.push(PollListItem {
                    hash: record.action_hash.clone(),
                    author: counted_author(record, &poll.carried_from),
                    poll,
                    dna_version: "1.5".to_string(),
                });
            }
        }
    }

    // Step 2: every older hash some newer version maps (it is shown there),
    // and for each active poll the older hashes it came from.
    let mut migrated_hashes = std::collections::HashSet::<String>::new();
    let mut from = std::collections::HashMap::<String, Vec<String>>::new();
    {
        let client = state.app_client.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = crate::hc::encode(&())?;
            let mappings = match call_zome_within(client, POLLS_ZOME, "get_all_migration_mappings", payload.clone(), LIST_WAIT_SECS).await {
                Ok(r) => Ok(r),
                // The mappings this device holds: without them a carried
                // poll would show again from its older line.
                Err(_) => call_zome(client, POLLS_ZOME, "get_all_migration_mappings_local", payload).await,
            };
            if let Ok(r) = mappings {
                for rec in crate::hc::records(client.line(), &r).unwrap_or_default() {
                    if let Ok(entry) = decode_entry::<MigratedPollEntry>(&rec) {
                        migrated_hashes.insert(entry.old_action_hash.to_string());
                        from.entry(entry.new_action_hash.to_string()).or_default().push(entry.old_action_hash.to_string());
                    }
                }
            }
        }
    }
    // The 0.5 move's v1.4 copy of a v1.3 poll whose copy is here: shown as
    // that copy, its votes read with it, until its own author's app maps it
    // on v1.5 (only that app can vouch for an author whose two keys differ).
    // It counts when its mapping is by the copy's own author and it asks
    // the same question (title, description, options, closing time): no
    // one can fold a different poll into another's.
    {
        let client = state.app_client_v1_4.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = crate::hc::encode(&())?;
            let mut polls_14 = std::collections::HashMap::<String, (String, Poll)>::new();
            if let Ok(r) = call_zome_within(client, POLLS_ZOME, "get_all_polls", crate::hc::encode(&())?, LIST_WAIT_SECS).await {
                for rec in crate::hc::records(client.line(), &r).unwrap_or_default() {
                    if let Ok(poll) = decode_entry::<Poll>(&rec) {
                        polls_14.insert(rec.action_hash.clone(), (rec.author.clone(), poll));
                    }
                }
            }
            if let Ok(r) = call_zome_within(client, POLLS_ZOME, "get_all_migration_mappings", payload, LIST_WAIT_SECS).await {
                for rec in crate::hc::records(client.line(), &r).unwrap_or_default() {
                    let Ok(entry) = decode_entry::<MigratedPollEntry>(&rec) else { continue };
                    let (old, later) = (entry.old_action_hash.to_string(), entry.new_action_hash.to_string());
                    let (Some(first), Some((later_author, later_poll))) = (firsts.get(&old), polls_14.get(&later)) else { continue };
                    if *later_author != rec.author || !same_question(first, later_poll) {
                        continue;
                    }
                    if let Some((copy, _)) = from.iter().find(|(_, olds)| olds.contains(&old)) {
                        let copy = copy.clone();
                        migrated_hashes.insert(later.clone());
                        from.entry(copy).or_default().push(later);
                    }
                }
            }
        }
    }
    *state.migrated_from.lock().unwrap() = Some(from);

    // Each older version's own mappings (chains the dedup across all hops).
    for older_client_mutex in [&state.app_client_v1_4, &state.app_client_v1_3, &state.app_client_v1_2, &state.app_client_v1_1] {
        let client = older_client_mutex.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = crate::hc::encode(&())?;
            if let Ok(r) = call_zome_within(client, POLLS_ZOME, "get_all_migration_mappings", payload, LIST_WAIT_SECS).await {
                let records = crate::hc::records(client.line(), &r).unwrap_or_default();
                for rec in &records {
                    if let Ok(entry) = decode_entry::<MigratedPollEntry>(rec) {
                        migrated_hashes.insert(entry.old_action_hash.to_string());
                    }
                }
            }
        }
    }

    // Step 3: polls from older versions not carried or migrated yet.
    let older_versions: Vec<(&tokio::sync::Mutex<Option<crate::hc::AppClient>>, &str)> = vec![
        (&state.app_client_v1_4, "1.4"),
        (&state.app_client_v1_3, "1.3"),
        (&state.app_client_v1_2, "1.2"),
        (&state.app_client_v1_1, "1.1"),
        (&state.app_client_v1_0, "1.0"),
    ];

    for (client_mutex, version) in &older_versions {
        let client = client_mutex.lock().await;
        if let Some(client) = client.as_ref() {
            let payload = crate::hc::encode(&())?;
            match call_zome_within(client, POLLS_ZOME, "get_all_polls", payload, LIST_WAIT_SECS).await {
                Ok(result) => {
                    let records = crate::hc::records(client.line(), &result).unwrap_or_default();
                    for record in &records {
                        let hash = record.action_hash.clone();
                        if migrated_hashes.contains(&hash) {
                            continue; // shown from a newer version
                        }
                        if let Ok(poll) = decode_entry::<Poll>(record) {
                            polls.push(PollListItem {
                                hash,
                                poll,
                                author: record.author.clone(),
                                dna_version: version.to_string(),
                            });
                        }
                    }
                }
                Err(e) => log::warn!("Could not fetch {} polls (skipping): {}", version, e),
            }
        }
    }

    Ok(polls)
}

#[tauri::command]
pub async fn delete_poll(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    action_hash: String,
) -> Result<String, String> {
    require_identity_link(&state).await?;
    let hash =
        ActionHash::try_from(action_hash).map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = crate::hc::encode(&hash)?;
    let result = {
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        call_zome(client, POLLS_ZOME, "delete_poll", payload).await?
    };

    // The originals it was carried from, where this install wrote them, so
    // installs still on an older line stop showing it too. Best effort: the
    // copy here is gone either way, and its mapping keeps it from coming back.
    for old in olds_of(&state, &hash.to_string()).await {
        for slot in [&state.app_client_v1_4, &state.app_client_v1_3] {
            let older = slot.lock().await;
            let Some(older) = older.as_ref() else { continue };
            let Ok(payload) = crate::hc::encode(&old) else { continue };
            if call_zome(older, POLLS_ZOME, "delete_poll", payload).await.is_ok() {
                log::info!("[delete] its original {} deleted on an older line too", old);
            }
        }
    }

    let delete_hash: ActionHash = crate::hc::decode(&result)?;
    Ok(delete_hash.to_string())
}

#[tauri::command]
pub async fn cast_vote(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    option_index: u32,
    // "1.0", "1.1", or "1.2" — routes the vote to the correct DHT cell.
    // Obtained from dna_version on PollListItem or PollDetail.
    dna_version: String,
    // "Anonymous" or "Public" — only relevant for dna_version "1.2".
    poll_type: Option<String>,
) -> Result<String, String> {
    log::info!(
        "cast_vote: dna_version={}, poll_type={:?}, option_index={}",
        dna_version, poll_type, option_index,
    );
    // Tier-1 gate: a public vote publishes the CACHED display name/photo -
    // material this install already holds for its linked identity - so a
    // signed-in install suffices. The cache itself only ever refreshes
    // under the strict gate, and a different identity demonstrably present
    // still refuses.
    require_identity_link(&state).await?;
    let cached = load_cached_profile(&state.data_dir);
    log::info!(
        "cast_vote: identity link present, cached profile: display_name={:?}, has_picture={}",
        cached.as_ref().and_then(|p| p.display_name.as_deref()),
        cached.as_ref().and_then(|p| p.profile_picture.as_deref()).is_some(),
    );

    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;

    let action_hash: ActionHash = if dna_version == "1.0" {
        // Vote on a legacy poll — write to v1.0 DHT (no display_name field).
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name: None,
            profile_picture: None,
        };
        let payload = crate::hc::encode(&input)?;
        let client = state.app_client_v1_0.lock().await;
        let client = client.as_ref().ok_or("v1.0 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        crate::hc::decode(&result)?
    } else if dna_version == "1.1" {
        // Vote on a v1.1 poll — write to v1.1 DHT (no display_name field).
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name: None,
            profile_picture: None,
        };
        let payload = crate::hc::encode(&input)?;
        let client = state.app_client_v1_1.lock().await;
        let client = client.as_ref().ok_or("v1.1 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        crate::hc::decode(&result)?
    } else if dna_version == "1.2" {
        // Vote on a v1.2 poll — include profile for public polls.
        let (display_name, profile_picture) = if poll_type.as_deref() == Some("Public") {
            let profile = load_cached_profile(&state.data_dir);
            let dn = profile.as_ref().and_then(|p| p.display_name.clone());
            let pp = profile.as_ref().and_then(|p| p.profile_picture.clone());
            (dn, pp)
        } else {
            (None, None)
        };
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name,
            profile_picture,
        };
        let payload = crate::hc::encode(&input)?;
        let client = state.app_client_v1_2.lock().await;
        let client = client.as_ref().ok_or("v1.2 conductor not available")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        crate::hc::decode(&result)?
    } else if dna_version == "1.3" || dna_version == "1.4" {
        // Vote on a poll not carried to v1.5 yet: it lands on the poll's own
        // network, and the move carries it once the poll can come.
        let (display_name, profile_picture) = if poll_type.as_deref() == Some("Public") {
            let profile = load_cached_profile(&state.data_dir);
            let dn = profile.as_ref().and_then(|p| p.display_name.clone());
            let pp = profile.as_ref().and_then(|p| p.profile_picture.clone());
            (dn, pp)
        } else {
            (None, None)
        };
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name,
            profile_picture,
        };
        let payload = crate::hc::encode(&input)?;
        let client = client_for(&state, &dna_version).lock().await;
        let client = client.as_ref().ok_or(format!("The v{} network is not available", dna_version))?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        crate::carry_move::reopen(&state.data_dir);
        crate::hc::decode(&result)?
    } else {
        // A poll on the active network (v1.5) — include profile for public polls.
        let (display_name, profile_picture) = if poll_type.as_deref() == Some("Public") {
            let profile = load_cached_profile(&state.data_dir);
            let dn = profile.as_ref().and_then(|p| p.display_name.clone());
            let pp = profile.as_ref().and_then(|p| p.profile_picture.clone());
            (dn, pp)
        } else {
            (None, None)
        };
        let input = CastVoteInput {
            poll_action_hash: hash,
            option_index,
            display_name,
            profile_picture,
        };
        let payload = crate::hc::encode(&input)?;
        let client = state.app_client.lock().await;
        let client = client.as_ref().ok_or("Conductor not ready")?;
        let result = call_zome(client, POLLS_ZOME, "cast_vote", payload).await?;
        crate::hc::decode(&result)?
    };

    Ok(action_hash.to_string())
}

/// The v1.3 poll a v1.4 poll was migrated from: from the map `get_all_polls`
/// keeps, read once from the active line when it is not loaded yet.
/// The client of the network a poll of `dna_version` lives on ("1.5" and
/// unknown versions: the active one).
pub(crate) fn client_for<'a>(state: &'a AppState, dna_version: &str) -> &'a tokio::sync::Mutex<Option<crate::hc::AppClient>> {
    match dna_version {
        "1.0" => &state.app_client_v1_0,
        "1.1" => &state.app_client_v1_1,
        "1.2" => &state.app_client_v1_2,
        "1.3" => &state.app_client_v1_3,
        "1.4" => &state.app_client_v1_4,
        _ => &state.app_client,
    }
}

/// A poll's flag records on one network (on the active one, what this
/// device holds when the network does not answer in time).
pub(crate) async fn flag_records(
    slot: &tokio::sync::Mutex<Option<crate::hc::AppClient>>,
    poll: &ActionHash,
    active: bool,
) -> Result<Vec<crate::hc::RecordView>, String> {
    let client = slot.lock().await;
    let client = client.as_ref().ok_or("This poll's network is not available")?;
    let payload = crate::hc::encode(poll)?;
    let result = if active {
        read_or_local(client, "get_poll_flags", payload).await?
    } else {
        call_zome(client, POLLS_ZOME, "get_poll_flags", payload).await?
    };
    crate::hc::records(client.line(), &result)
}

/// The older hashes a poll on the active network came from, from the map
/// `get_all_polls` keeps (read once from the active line when not loaded).
pub(crate) async fn olds_of(state: &AppState, new_hash: &str) -> Vec<ActionHash> {
    let cached = state.migrated_from.lock().unwrap().as_ref().map(|m| m.get(new_hash).cloned().unwrap_or_default());
    let olds = match cached {
        Some(found) => found,
        None => {
            let client = state.app_client.lock().await;
            let Some(client) = client.as_ref() else { return Vec::new() };
            let Ok(payload) = crate::hc::encode(&()) else { return Vec::new() };
            let Ok(r) = read_or_local(client, "get_all_migration_mappings", payload).await else { return Vec::new() };
            let mut from = std::collections::HashMap::<String, Vec<String>>::new();
            for rec in crate::hc::records(client.line(), &r).unwrap_or_default() {
                if let Ok(entry) = decode_entry::<MigratedPollEntry>(&rec) {
                    from.entry(entry.new_action_hash.to_string()).or_default().push(entry.old_action_hash.to_string());
                }
            }
            let found = from.get(new_hash).cloned().unwrap_or_default();
            *state.migrated_from.lock().unwrap() = Some(from);
            found
        }
    };
    olds.into_iter().filter_map(|o| ActionHash::try_from(o).ok()).collect()
}

/// The records `fn_name` (votes or flags) returns for `poll` on every older
/// line that may hold some under one of its older hashes (v1.4, v1.3), each
/// read waiting at most [`LIST_WAIT_SECS`].
pub(crate) async fn on_older_lines(state: &AppState, olds: &[ActionHash], fn_name: &str) -> Vec<crate::hc::RecordView> {
    let mut out = Vec::new();
    for slot in [&state.app_client_v1_4, &state.app_client_v1_3] {
        let client = slot.lock().await;
        let Some(client) = client.as_ref() else { continue };
        for old in olds {
            let Ok(payload) = crate::hc::encode(old) else { continue };
            match call_zome_within(client, POLLS_ZOME, fn_name, payload, LIST_WAIT_SECS).await {
                Ok(r) => out.extend(crate::hc::records(client.line(), &r).unwrap_or_default()),
                Err(e) => log::debug!("[{}] an older copy was not read: {}", fn_name, e),
            }
        }
    }
    out
}

/// Each voter key's linked identities (the Vault keys it links to on any
/// network), kept ten minutes: a person's keys share their identity.
pub(crate) static IDENTITIES: Mutex<Option<std::collections::HashMap<String, (std::time::Instant, Vec<String>)>>> = Mutex::new(None);

pub(crate) async fn identities_of(state: &std::sync::Arc<AppState>, key: &str) -> Vec<String> {
    let cached = IDENTITIES.lock().unwrap().as_ref().and_then(|m| m.get(key).cloned());
    if let Some((at, ids)) = &cached {
        if at.elapsed() < std::time::Duration::from_secs(600) {
            return ids.clone();
        }
    }
    let Ok(agent) = AgentPubKey::try_from(key.to_string()) else { return Vec::new() };
    let lookup = async {
        let mut ids: Vec<String> = Vec::new();
        for network in LINK_NETWORKS {
            for h in hubs_on(state, &agent, network).await {
                let h = h.to_string();
                if !ids.contains(&h) {
                    ids.push(h);
                }
            }
        }
        ids
    };
    // At most LIST_WAIT_SECS, as the reads it counts: then the last answer
    // kept, or the key alone, and asked again next time.
    let Ok(ids) = tokio::time::timeout(std::time::Duration::from_secs(LIST_WAIT_SECS), lookup).await else {
        log::debug!("[identity] {}'s linked identities did not come in {} s", key, LIST_WAIT_SECS);
        return cached.map(|(_, ids)| ids).unwrap_or_default();
    };
    IDENTITIES.lock().unwrap().get_or_insert_with(Default::default).insert(key.to_string(), (std::time::Instant::now(), ids.clone()));
    ids
}

/// One record per person: keys linked to one identity are one person (two
/// installs, or one install's two keys), and the first record of each is
/// kept (the active network's come first). `voter` is whom a record counts
/// for.
pub(crate) async fn once_per_person(
    state: &std::sync::Arc<AppState>,
    records: Vec<crate::hc::RecordView>,
    voter: impl Fn(&crate::hc::RecordView) -> String,
) -> Vec<crate::hc::RecordView> {
    // Each distinct voter's identities, looked up side by side.
    let keys: Vec<String> = records.iter().map(&voter).collect();
    let mut lookups = tokio::task::JoinSet::new();
    for key in keys.iter().cloned().collect::<std::collections::HashSet<_>>() {
        let state = state.clone();
        lookups.spawn(async move {
            let ids = identities_of(&state, &key).await;
            (key, ids)
        });
    }
    let mut identities = std::collections::HashMap::<String, Vec<String>>::new();
    while let Some(Ok((key, ids))) = lookups.join_next().await {
        identities.insert(key, ids);
    }
    let mut seen = std::collections::HashSet::<String>::new();
    let mut out = Vec::new();
    for (rec, key) in records.into_iter().zip(keys) {
        let mut person = identities.get(&key).cloned().unwrap_or_default();
        person.push(key);
        if person.iter().any(|p| seen.contains(p)) {
            continue;
        }
        seen.extend(person);
        out.push(rec);
    }
    out
}

#[tauri::command]
pub async fn get_poll_votes(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    poll_action_hash: String,
    // The poll's dna_version (from PollListItem or PollDetail): votes are
    // read from its network.
    dna_version: String,
) -> Result<Vec<VoteData>, String> {
    let hash = ActionHash::try_from(poll_action_hash)
        .map_err(|e| format!("Invalid action hash: {:?}", e))?;
    let payload = crate::hc::encode(&hash)?;
    let active = !matches!(dna_version.as_str(), "1.0" | "1.1" | "1.2" | "1.3" | "1.4");

    let mut records: Vec<crate::hc::RecordView> = {
        let client = client_for(&state, &dna_version).lock().await;
        let client = client.as_ref().ok_or(format!("The v{} network is not available", dna_version))?;
        let result = if active {
            read_or_local(client, "get_poll_votes", payload).await?
        } else {
            call_zome(client, POLLS_ZOME, "get_poll_votes", payload).await?
        };
        crate::hc::records(client.line(), &result)?
    };
    // A poll on the active network: add the votes still on an older line
    // under one of its older hashes (not carried yet), by anyone with no
    // vote here. A carried vote counts for the voter its proof names.
    if active {
        let olds = olds_of(&state, &hash.to_string()).await;
        if !olds.is_empty() {
            let mut here: std::collections::HashSet<String> = records
                .iter()
                .map(|r| counted_author(r, &decode_entry::<Vote>(r).ok().and_then(|v| v.carried_from)))
                .collect();
            for rec in on_older_lines(&state, &olds, "get_poll_votes").await {
                if here.insert(rec.author.clone()) {
                    records.push(rec);
                }
            }
        }
    }
    // Once per person.
    let records = once_per_person(state.inner(), records, |r| counted_author(r, &decode_entry::<Vote>(r).ok().and_then(|v| v.carried_from))).await;

    let mut votes = Vec::new();
    for record in &records {
        let vote: Vote = decode_entry(record)?;
        votes.push(VoteData {
            vote: VoteResponse {
                hash: record.action_hash.clone(),
                poll_action_hash: vote.poll_action_hash.to_string(),
                option_index: vote.option_index,
            },
            author: counted_author(record, &vote.carried_from),
            display_name: vote.display_name,
            profile_picture: vote.profile_picture,
        });
    }
    Ok(votes)
}
