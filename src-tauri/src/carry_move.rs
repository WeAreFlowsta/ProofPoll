//! The move to v1.5: what this install wrote on v1.3 and v1.4 is written
//! again on v1.5 with its proof (the original record as its author signed
//! it), which the v1.5 network checks. Older installs, still on v1.4, see no
//! change, and nobody else has to act.
//!
//! The install's own chains are read whole from each conductor
//! (`dump_full_state`). Each carry function returns the copy already there,
//! whoever carried it, so a run that stops resumes where it was. In order:
//!   1. its agent links (`carry_link`);
//!   2. its polls, open and closed: each poll's first record by `carry_poll`;
//!      a v1.4 poll the 0.5 move made from one of its v1.3 polls is mapped to
//!      that poll's copy (`map_later_copy`), vouched for by the v1.3 key when
//!      the v1.4 key is another;
//!   3. its votes and flags (`carry_vote`, `carry_flag`), each naming its
//!      poll's mapping; a poll not carried yet is carried first from the
//!      network's record;
//!   4. its drafts and vote reasons, written again as they are (encrypted to
//!      its own key).
//! What cannot go yet (a poll whose first record is on v1.3 with no 0.6 line
//! here, or a later copy under another person's two keys) waits for a later
//! run; the readers show it from the older line meanwhile.

use crate::carried::{self, fields, Carried, CarryInput, LaterCopy, MapLaterCopyInput, Mapping, OnPoll, FLAG, LINK, MAPPING, POLL, PRIVATE, VOTE};
use crate::commands::AppState;
use crate::hc::AppClient;
use holochain_types_07::prelude::{ActionHash, AgentPubKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

const POLLS: &str = "polls";
const STATE_FILE: &str = "carry-move-v1_5.json";

/// What the move keeps between runs.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct MoveState {
    /// Nothing was left the last time it ran.
    pub complete: bool,
    /// Drafts and reasons written again (their old hashes): the one kind no
    /// carry function finds by its original.
    pub private_copied: Vec<String>,
}

impl MoveState {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(STATE_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(data_dir.join(STATE_FILE), json);
        }
    }
}

/// One app record of this install's own chain, as signed.
#[derive(Clone, Debug)]
pub struct Own {
    pub kind: (u8, u8),
    pub carried: Carried,
}

#[derive(Deserialize, Debug)]
struct Private {
    cipher: Vec<u8>,
    nonce: Vec<u8>,
    related_hash: Option<ActionHash>,
}

/// A v1.4 poll its 0.5 move made from a v1.3 poll, with that move's mapping.
#[derive(Clone, Debug)]
pub struct Later {
    pub first: ActionHash,
    pub poll: Carried,
    pub mapping: Carried,
}

/// A draft or reason to write again.
#[derive(Clone, Debug)]
pub struct PrivateCopy {
    pub old: ActionHash,
    pub cipher: Vec<u8>,
    pub nonce: Vec<u8>,
    /// For a reason: the vote it explains, on its own line.
    pub reason_for: Option<ActionHash>,
}

/// What the move does with the two chains, worked out from them alone.
#[derive(Default, Debug)]
pub struct Plan {
    pub links: Vec<Carried>,
    /// Polls carried as their first record.
    pub first: Vec<Carried>,
    pub later: Vec<Later>,
    /// Votes and flags, v1.4's first, each with its poll's hash on its line.
    pub votes: Vec<(Carried, ActionHash)>,
    pub flags: Vec<(Carried, ActionHash)>,
    pub private: Vec<PrivateCopy>,
}

/// The plan for this install's own records (`v13` None when it has no 0.6
/// line). A v1.4 poll is a later copy when its own v1.4 mapping names it:
/// the 0.5 move wrote one for each v1.3 poll it copied. When the v1.3 chain
/// is here without that poll (deleted there since), the v1.4 copy is the
/// poll's first record now. Drafts and reasons the 0.5 move copied to v1.4
/// are on both chains; each ciphertext goes once.
pub fn plan(v13: Option<&[Own]>, v14: &[Own]) -> Plan {
    let has_v13 = v13.is_some();
    let v13 = v13.unwrap_or(&[]);
    let of = |kind: (u8, u8), records: &[Own]| -> Vec<Own> { records.iter().filter(|r| r.kind == kind).cloned().collect() };
    let mut p = Plan::default();
    let later_of: Vec<(ActionHash, Own)> = of(MAPPING, v14)
        .into_iter()
        .filter_map(|m| fields::<Mapping>(&m.carried).map(|f| (f.new_action_hash, Own { kind: m.kind, carried: m.carried.clone() })))
        .collect();
    for r in of(LINK, v14).into_iter().chain(of(LINK, v13)) {
        p.links.push(r.carried);
    }
    p.first.extend(of(POLL, v13).into_iter().map(|r| r.carried));
    for r in of(POLL, v14) {
        let later = later_of.iter().find(|(new, _)| *new == r.carried.original_action_hash).map(|(_, m)| {
            let first = fields::<Mapping>(&m.carried).map(|f| f.old_action_hash).expect("decoded above");
            Later { first, poll: r.carried.clone(), mapping: m.carried.clone() }
        });
        match later {
            Some(l) if has_v13 && !v13.iter().any(|x| x.kind == POLL && x.carried.original_action_hash == l.first) => p.first.push(r.carried),
            Some(l) => p.later.push(l),
            None => p.first.push(r.carried),
        }
    }
    for (kind, out) in [(VOTE, &mut p.votes), (FLAG, &mut p.flags)] {
        for r in of(kind, v14).into_iter().chain(of(kind, v13)) {
            if let Some(f) = fields::<OnPoll>(&r.carried) {
                out.push((r.carried, f.poll_action_hash));
            }
        }
    }
    let mut seen = HashSet::new();
    for r in of(PRIVATE, v14).into_iter().chain(of(PRIVATE, v13)) {
        if let Some(f) = fields::<Private>(&r.carried) {
            if seen.insert((f.cipher.clone(), f.nonce.clone())) {
                p.private.push(PrivateCopy { old: r.carried.original_action_hash, cipher: f.cipher, nonce: f.nonce, reason_for: f.related_hash });
            }
        }
    }
    p
}

/// What a run did.
#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub links: usize,
    pub polls: usize,
    pub later: usize,
    pub votes: usize,
    pub flags: usize,
    pub private: usize,
    /// Left for a later run: their poll is not here yet and cannot come yet.
    pub waiting: usize,
    pub failed: usize,
}

async fn call<I: Serialize + std::fmt::Debug>(client: &AppClient, f: &str, input: &I) -> Result<Vec<u8>, String> {
    crate::commands::call_zome(client, POLLS, f, crate::hc::encode(input)?).await
}

async fn call_hash<I: Serialize + std::fmt::Debug>(client: &AppClient, f: &str, input: &I) -> Result<ActionHash, String> {
    crate::hc::decode(&call(client, f, input).await?)
}

/// The copy on v1.5 an older hash opens.
async fn copy_of(dest: &AppClient, old: &ActionHash) -> Result<Option<ActionHash>, String> {
    crate::hc::decode(&call(dest, "get_migration_mapping", old).await?)
}

/// The mapping record a carried vote or flag on `old` names.
async fn mapping_record(dest: &AppClient, old: &ActionHash) -> Result<Option<ActionHash>, String> {
    crate::hc::decode(&call(dest, "get_migration_record", old).await?)
}

/// This install's own chain on v1.3 (0.6), as signed; None with no 0.6 line.
async fn own_v13(state: &AppState) -> Result<Option<Vec<Own>>, String> {
    use holochain_types::prelude::Action;
    let Some(dump) = crate::commands::dump_chain_06(state).await? else { return Ok(None) };
    let deleted: HashSet<String> = dump
        .records
        .iter()
        .filter_map(|r| match &r.action {
            Action::Delete(d) => Some(d.deletes_address.to_string()),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for rec in &dump.records {
        let (Some(entry), Some(holochain_types::prelude::EntryType::App(def))) = (rec.entry.as_ref().and_then(|e| e.as_app_entry()), rec.action.entry_type()) else { continue };
        if deleted.contains(&rec.action_address.to_string()) {
            continue;
        }
        let carried = carried::from_v13(&rec.action, &rec.signature, entry.as_ref().bytes(), None)?;
        out.push(Own { kind: (def.zome_index.0, def.entry_index.0), carried });
    }
    Ok(Some(out))
}

/// This install's own chain on v1.4 (0.7), as signed.
async fn own_v14() -> Result<Vec<Own>, String> {
    use holochain_client_07::{AdminWebsocket, CellInfo};
    use holochain_types_07::prelude::{ActionData, EntryType};
    let admin = AdminWebsocket::connect(format!("localhost:{}", crate::conductor::admin_port_07()), Some(crate::conductor::node_origin()))
        .await
        .map_err(|e| format!("0.7 admin: {}", e))?;
    let apps = admin.list_apps(None).await.map_err(|e| format!("list_apps: {}", e))?;
    let Some(cell_id) = apps.iter().find(|a| a.installed_app_id == crate::dna::APP_ID_V1_4).and_then(|app| {
        app.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
            CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
            _ => None,
        })
    }) else {
        return Ok(Vec::new());
    };
    let dump = admin.dump_full_state(cell_id, None, None).await.map_err(|e| format!("v1.4 dump: {}", e))?;
    let records = &dump.source_chain_dump.records;
    let deleted: HashSet<String> = records
        .iter()
        .filter_map(|r| match &r.action.data {
            ActionData::Delete(d) => Some(d.deletes_address.to_string()),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for rec in records {
        let (Some(entry), Some(EntryType::App(def))) = (rec.entry.as_ref().and_then(|e| e.as_app_entry()), rec.action.entry_type()) else { continue };
        if deleted.contains(&rec.action_address.to_string()) {
            continue;
        }
        let carried = carried::from_v14(&rec.action, &rec.signature, entry.as_ref().bytes(), None)?;
        out.push(Own { kind: (def.zome_index.0, def.entry_index.0), carried });
    }
    Ok(out)
}

/// The 0.6 key's signature vouching that `later` is also this install's
/// (`carried::key_vouch_payload`), by the 0.6 lair.
async fn vouch_for(state: &AppState, later: &AgentPubKey) -> Result<Signature, String> {
    use lair_keystore_api::prelude::BinDataSized;
    let key_06 = state.agent_pub_key.lock().unwrap().clone().ok_or("Agent key not available")?;
    let key_06 = crate::commands::parse_agent_pub_key_string(&key_06)?;
    let raw: [u8; 32] = key_06.get_raw_32().try_into().map_err(|_| "an agent key is 32 bytes".to_string())?;
    let lair = state.lair_client.lock().await;
    let lair = lair.as_ref().ok_or("The 0.6 keystore is not connected")?;
    let sig = lair
        .sign_by_pub_key(BinDataSized(Arc::new(raw)), None, carried::key_vouch_payload(later).into())
        .await
        .map_err(|e| format!("sign the vouch: {}", e))?;
    Ok(Signature(*sig.0))
}

/// Map a later copy to its first record's copy here. False when that copy
/// is not here yet.
async fn map_later(state: &AppState, dest: &AppClient, first: Option<&Carried>, later: &Later) -> Result<bool, String> {
    let Some(copy) = copy_of(dest, &later.first).await? else { return Ok(false) };
    let first_author = match first {
        Some(f) => f.original_author.clone(),
        None => crate::commands::parse_agent_pub_key_string(&state.agent_pub_key.lock().unwrap().clone().ok_or("Agent key not available")?)
            .map(|k| AgentPubKey::from_raw_39(k.get_raw_39().to_vec()))?,
    };
    let vouch = if later.poll.original_author != first_author { Some(vouch_for(state, &later.poll.original_author).await?) } else { None };
    let lc = LaterCopy { poll: later.poll.clone(), mapping: later.mapping.clone(), vouch };
    if let Some(f) = first {
        carried::check_later_copy(f, &lc)?;
    }
    call_hash(dest, "map_later_copy", &MapLaterCopyInput { copy, later: lc }).await?;
    Ok(true)
}

/// The client of an older line, cloned so no lock is held across calls.
async fn client_of(state: &AppState, dna: &str) -> Option<AppClient> {
    match dna {
        "1.3" => state.app_client_v1_3.lock().await.clone(),
        "1.4" => state.app_client_v1_4.lock().await.clone(),
        _ => None,
    }
}

/// Make sure the poll `old` (on `dna`) has its copy here, carrying it from
/// the network's record when nobody has yet. False when it cannot be done
/// now: the line is not running, or the poll is a later copy whose first
/// record cannot be reached or whose author's keys differ.
async fn ensure_carried(state: &AppState, dest: &AppClient, dna: &str, old: &ActionHash) -> Result<bool, String> {
    if copy_of(dest, old).await?.is_some() {
        return Ok(true);
    }
    let Some(source) = client_of(state, dna).await else { return Ok(false) };
    let Some(poll) = carried::from_record(dna, &call(&source, "get_poll", old).await?)? else { return Ok(false) };
    if dna == "1.4" {
        // A later copy: its own author's v1.4 mapping names it.
        let maps = carried::from_records("1.4", &call(&source, "get_all_migration_mappings", &()).await?)?;
        let later = maps.into_iter().find(|m| {
            m.original_author == poll.original_author && fields::<Mapping>(m).is_some_and(|f| f.new_action_hash == *old)
        });
        if let Some(mapping) = later {
            let first = fields::<Mapping>(&mapping).map(|f| f.old_action_hash).expect("matched above");
            if !Box::pin(ensure_carried(state, dest, "1.3", &first)).await? {
                return Ok(false);
            }
            let first_record = match client_of(state, "1.3").await {
                Some(c) => carried::from_record("1.3", &call(&c, "get_poll", &first).await?)?,
                None => None,
            };
            let Some(first_record) = first_record else { return Ok(false) };
            // Another person's two keys: only they can vouch.
            if first_record.original_author != poll.original_author {
                return Ok(false);
            }
            return map_later(state, dest, Some(&first_record), &Later { first, poll, mapping }).await;
        }
    }
    carried::check(&poll)?;
    call_hash(dest, "carry_poll", &CarryInput { carried: poll }).await?;
    Ok(true)
}

/// One run of the move. Writes under the same gate as every write.
pub async fn run(state: &Arc<AppState>) -> Result<Report, String> {
    let mut ms = MoveState::load(&state.data_dir);
    let mut report = Report::default();
    if ms.complete {
        return Ok(report);
    }
    let v13 = own_v13(state).await?;
    let v14 = own_v14().await?;
    let plan = plan(v13.as_deref(), &v14);
    let dest = state.app_client.lock().await.clone().ok_or("The v1.5 client is not connected")?;
    let dest = &dest;

    let count = |r: Result<bool, String>, done: &mut usize, report_waiting: &mut usize, report_failed: &mut usize, what: &str| match r {
        Ok(true) => *done += 1,
        Ok(false) => *report_waiting += 1,
        Err(e) => {
            log::warn!("[move 1.5] {} not carried this run: {}", what, e);
            *report_failed += 1;
        }
    };

    for c in &plan.links {
        let r = match carried::check(c) {
            Ok(_) => call_hash(dest, "carry_link", &CarryInput { carried: c.clone() }).await.map(|_| true),
            Err(e) => Err(e),
        };
        count(r, &mut report.links, &mut report.waiting, &mut report.failed, "an agent link");
    }
    for c in &plan.first {
        let r = match carried::check(c) {
            Ok(_) => call_hash(dest, "carry_poll", &CarryInput { carried: c.clone() }).await.map(|_| true),
            Err(e) => Err(e),
        };
        count(r, &mut report.polls, &mut report.waiting, &mut report.failed, "a poll");
    }
    for l in &plan.later {
        // Its first record, carried above; with no 0.6 line now, mapped once
        // that poll's copy is here.
        let first = v13.as_deref().and_then(|v| v.iter().find(|r| r.carried.original_action_hash == l.first)).map(|r| r.carried.clone());
        let r = map_later(state, dest, first.as_ref(), l).await;
        count(r, &mut report.later, &mut report.waiting, &mut report.failed, "a later copy of a poll");
    }

    // A v1.3 vote on a poll whose copy already has this install's v1.4 vote
    // (the 0.5 move cast it again there) stays behind: one vote per poll.
    let mut voted: HashSet<ActionHash> = HashSet::new();
    for (kind, items) in [("carry_vote", &plan.votes), ("carry_flag", &plan.flags)] {
        voted.clear();
        for (c, poll) in items {
            let r: Result<bool, String> = async {
                if !ensure_carried(state, dest, &c.original_dna, poll).await? {
                    return Ok(false);
                }
                let (Some(mapping), Some(copy)) = (mapping_record(dest, poll).await?, copy_of(dest, poll).await?) else { return Ok(false) };
                if c.original_dna == "1.3" && voted.contains(&copy) {
                    return Ok(true);
                }
                let mut c = c.clone();
                c.poll_mapping = Some(mapping);
                carried::check(&c)?;
                call_hash(dest, kind, &CarryInput { carried: c }).await?;
                voted.insert(copy);
                Ok(true)
            }
            .await;
            let (done, what) = if kind == "carry_vote" { (&mut report.votes, "a vote") } else { (&mut report.flags, "a flag") };
            count(r, done, &mut report.waiting, &mut report.failed, what);
        }
    }

    // Drafts, and reasons under the carried vote they explain.
    for p in &plan.private {
        if ms.private_copied.contains(&p.old.to_string()) {
            continue;
        }
        let r: Result<bool, String> = async {
            let related = match &p.reason_for {
                None => None,
                Some(vote) => {
                    let Some((c, poll)) = plan.votes.iter().find(|(c, _)| &c.original_action_hash == vote) else { return Ok(true) };
                    let Some(copy) = copy_of(dest, poll).await? else { return Ok(false) };
                    let votes = call(dest, "get_poll_votes", &copy).await?;
                    let here = crate::hc::records(dest.line(), &votes)?;
                    let carried_vote = here.iter().find(|v| {
                        v.entry_as::<CarriedVote>().ok().and_then(|e| e.carried_from).is_some_and(|f| f.original_action_hash == c.original_action_hash)
                    });
                    // A v1.3 vote left behind for its v1.4 one: that one's
                    // reason was copied with it by the 0.5 move.
                    let Some(v) = carried_vote else { return Ok(true) };
                    Some(ActionHash::from_raw_39(crate::hc::hash_bytes(&v.action_hash)?))
                }
            };
            let input = CreateEncryptedEntryInput {
                cipher: p.cipher.clone(),
                nonce: p.nonce.clone(),
                link_as: if related.is_some() { "vote_rationale".into() } else { "draft_poll".into() },
                related_hash: related,
            };
            crate::commands::call_zome(dest, POLLS, "create_encrypted_entry", crate::hc::encode(&input)?).await?;
            ms.private_copied.push(p.old.to_string());
            ms.save(&state.data_dir);
            Ok(true)
        }
        .await;
        count(r, &mut report.private, &mut report.waiting, &mut report.failed, "a draft or reason");
    }

    ms.complete = report.waiting == 0 && report.failed == 0;
    ms.save(&state.data_dir);
    log::info!("[move 1.5] {:?}", report);
    Ok(report)
}

/// The v1.5 `create_encrypted_entry` input.
#[derive(Serialize, Debug)]
struct CreateEncryptedEntryInput {
    cipher: Vec<u8>,
    nonce: Vec<u8>,
    link_as: String,
    related_hash: Option<ActionHash>,
}

/// A v1.5 vote as far as the move reads it.
#[derive(Deserialize, Debug)]
struct CarriedVote {
    #[serde(default)]
    carried_from: Option<Carried>,
}

/// Run the move now and again every ten minutes until nothing is left,
/// under the same gate as every write (a different identity present: wait).
pub fn spawn(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        // The cells settle for a moment after startup.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        loop {
            match crate::commands::require_identity_link(&state).await {
                Ok(_) => {}
                Err(e) if e == crate::commands::ERR_NOT_LINKED => {}
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    continue;
                }
            }
            match run(&state).await {
                Ok(_) if MoveState::load(&state.data_dir).complete => break,
                Ok(_) => {}
                Err(e) => log::warn!("[move 1.5] this run stopped: {}", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
}

/// After a write on an older line (a vote on a poll that could not come yet):
/// the move runs again at the next launch.
pub fn reopen(data_dir: &Path) {
    let mut ms = MoveState::load(data_dir);
    if ms.complete {
        ms.complete = false;
        ms.save(data_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holochain_types_07::prelude::Timestamp;

    fn rec(kind: (u8, u8), byte: u8, entry: Vec<u8>) -> Own {
        Own {
            kind,
            carried: Carried {
                original_dna: "1.4".into(),
                original_action_hash: ActionHash::from_raw_32(vec![byte; 32]),
                original_author: AgentPubKey::from_raw_32(vec![1; 32]),
                original_timestamp: Timestamp::from_micros(1),
                original_action: vec![],
                original_signature: Signature([0; 64]),
                original_entry: entry,
                poll_mapping: None,
            },
        }
    }

    fn h(byte: u8) -> ActionHash {
        ActionHash::from_raw_32(vec![byte; 32])
    }

    #[derive(Serialize, Debug)]
    struct MappingOut {
        old_action_hash: ActionHash,
        new_action_hash: ActionHash,
        migrated_at: i64,
        carried_from: Option<()>,
    }

    #[derive(Serialize, Debug)]
    struct VoteOut {
        poll_action_hash: ActionHash,
        option_index: u32,
    }

    #[derive(Serialize, Debug)]
    struct PrivateOut {
        cipher: Vec<u8>,
        nonce: Vec<u8>,
        entry_type_hint: String,
        related_hash: Option<ActionHash>,
    }

    fn enc<T: Serialize>(v: &T) -> Vec<u8> {
        rmp_serde::to_vec_named(v).unwrap()
    }

    /// The 0.5 move's v1.4 copy of a v1.3 poll is mapped, not carried as a
    /// first record; a v1.4 poll born there is carried; v1.4's votes come
    /// before v1.3's; a draft the 0.5 move copied goes once.
    #[test]
    fn the_plan_maps_later_copies_and_takes_each_draft_once() {
        let poll13 = rec(POLL, 10, vec![0x80]);
        let vote13 = rec(VOTE, 11, enc(&VoteOut { poll_action_hash: h(10), option_index: 0 }));
        let draft13 = rec(PRIVATE, 12, enc(&PrivateOut { cipher: vec![7; 8], nonce: vec![1; 24], entry_type_hint: "private".into(), related_hash: None }));
        let copy14 = rec(POLL, 20, vec![0x80]);
        let map14 = rec(MAPPING, 21, enc(&MappingOut { old_action_hash: h(10), new_action_hash: h(20), migrated_at: 1, carried_from: None }));
        let born14 = rec(POLL, 22, vec![0x80]);
        let vote14 = rec(VOTE, 23, enc(&VoteOut { poll_action_hash: h(20), option_index: 0 }));
        let draft14 = rec(PRIVATE, 24, enc(&PrivateOut { cipher: vec![7; 8], nonce: vec![1; 24], entry_type_hint: "private".into(), related_hash: None }));
        let reason14 = rec(PRIVATE, 25, enc(&PrivateOut { cipher: vec![9; 8], nonce: vec![2; 24], entry_type_hint: "private".into(), related_hash: Some(h(23)) }));
        let v14 = [copy14, map14, born14, vote14, draft14, reason14];
        let v13 = [poll13, vote13, draft13];
        let p = plan(Some(&v13[..]), &v14);
        let hashes = |v: &[Carried]| v.iter().map(|c| c.original_action_hash.clone()).collect::<Vec<_>>();
        assert_eq!(hashes(&p.first), vec![h(10), h(22)], "the v1.3 poll and the poll born on v1.4");
        assert_eq!(p.later.len(), 1);
        assert_eq!((p.later[0].first.clone(), p.later[0].poll.original_action_hash.clone()), (h(10), h(20)));
        assert_eq!(p.votes.iter().map(|(c, poll)| (c.original_action_hash.clone(), poll.clone())).collect::<Vec<_>>(), vec![(h(23), h(20)), (h(11), h(10))]);
        assert_eq!(p.private.iter().map(|x| (x.old.clone(), x.reason_for.clone())).collect::<Vec<_>>(), vec![(h(24), None), (h(25), Some(h(23)))]);

        // No 0.6 line: the later copy waits for its first record's copy.
        let p = plan(None, &v14);
        assert_eq!((hashes(&p.first), p.later.len()), (vec![h(22)], 1));
        // The 0.6 line without that poll (deleted there): the v1.4 copy is
        // the first record now.
        let p = plan(Some(&[][..]), &v14);
        assert_eq!((hashes(&p.first), p.later.len()), (vec![h(20), h(22)], 0));
    }

    #[test]
    fn the_state_survives_a_reload_and_reopens() {
        let dir = std::env::temp_dir().join(format!("proofpoll-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(MoveState::load(&dir), MoveState::default());
        MoveState { complete: true, private_copied: vec!["x".into()] }.save(&dir);
        assert!(MoveState::load(&dir).complete);
        reopen(&dir);
        assert_eq!(MoveState::load(&dir), MoveState { complete: false, private_copied: vec!["x".into()] });
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
