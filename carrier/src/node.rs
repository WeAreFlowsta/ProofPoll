//! One pass of the node's carrier.
//!
//! What it reads, network-wide through the older DNAs' own getters (each
//! returns records as their authors signed them): every poll on v1.3 and
//! v1.4, v1.4's poll mappings, and each poll's votes and flags. Agent links
//! and deletes have no such getter, so they come from what this node holds
//! of each older network (its conductor's state dump: every record it
//! stores as an authority, with its signature).
//!
//! What it writes on v1.5, in order, each skipped when it is there already:
//!   1. agent links (`carry_link`);
//!   2. each poll's first record (`carry_poll`): a v1.3 poll, or a v1.4 poll
//!      that is no later copy;
//!   3. later copies: a v1.4 poll its author's own move made from their v1.3
//!      poll, under the same key, mapped to that poll's copy
//!      (`map_later_copy`). One under the author's other key waits for the
//!      author's app, the one place that holds both keys to vouch;
//!   4. votes and flags, v1.4's first (`carry_vote`, `carry_flag`), one per
//!      voter and poll copy, as the v1.5 zome keeps them;
//!   5. a copy this node made of a poll or flag its author deleted is
//!      deleted too.

use crate::carried::{self, fields, Carried, CarryInput, LaterCopy, MapLaterCopyInput, Mapping, LINK};
use crate::hc::{self, AppClient, Line};
use holochain_types_07::prelude::{ActionHash, AgentPubKey};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const ROLE: &str = "proofpoll";
const POLLS: &str = "polls";
const APP_V13: &str = "proofpoll_v1_3";
const APP_V14: &str = "proofpoll_v1_4";
const APP_V15: &str = "proofpoll_v1_5";
/// DHT ops per page of a 0.7 state dump.
const DUMP_PAGE: u32 = 500;

pub struct Options {
    pub admin_06: String,
    pub admin_07: String,
    pub origin: String,
    pub dry_run: bool,
    pub max_writes: u64,
    pub pause_ms: u64,
    pub min_free_mb: u64,
}

/// What a pass saw and did: counts only.
#[derive(Serialize, Default, Debug)]
pub struct Report {
    pub dry_run: bool,
    /// The 0.6 conductor answered with v1.3 installed.
    pub v13_read: bool,
    /// The 0.7 conductor has v1.4 installed.
    pub v14_read: bool,
    /// What the older DNAs hold (records not deleted).
    pub seen: Seen,
    /// Written on v1.5 this pass (with `--dry-run`, what would be).
    pub carried: Carried5,
    /// Not carried yet: a later copy under its author's other key, or a
    /// record whose poll has no copy on v1.5 yet.
    pub waiting: u64,
    /// Refused or failed; each reason is on stderr.
    pub failed: u64,
    /// Anything was written on v1.5 this pass.
    pub wrote: bool,
    /// Why the pass stopped before the end (memory, the write limit).
    pub stopped: Option<String>,
}

#[derive(Serialize, Default, Debug)]
pub struct Seen {
    pub polls_v13: u64,
    pub polls_v14: u64,
    pub later_copies: u64,
    pub votes: u64,
    pub flags: u64,
    pub links: u64,
    pub deletes: u64,
}

#[derive(Serialize, Default, Debug)]
pub struct Carried5 {
    pub links: u64,
    pub polls: u64,
    pub later: u64,
    pub votes: u64,
    pub flags: u64,
    pub deleted: u64,
}

impl Carried5 {
    fn total(&self) -> u64 {
        self.links + self.polls + self.later + self.votes + self.flags + self.deleted
    }
}

// ── Pacing ───────────────────────────────────────────────────────────

/// The pause between calls, the memory floor and the write limit.
struct Pace<'a> {
    o: &'a Options,
    writes: u64,
}

impl Pace<'_> {
    /// Before each call: pause, then stop when memory runs low.
    async fn call(&self) -> Result<(), String> {
        tokio::time::sleep(std::time::Duration::from_millis(self.o.pause_ms)).await;
        match available_mb() {
            Some(mb) if mb < self.o.min_free_mb => Err(format!("{mb} MB available, under {} MB", self.o.min_free_mb)),
            _ => Ok(()),
        }
    }

    /// Before each write: as `call`, and stop at the write limit.
    async fn write(&mut self) -> Result<(), String> {
        if self.writes >= self.o.max_writes {
            return Err(format!("the write limit ({})", self.o.max_writes));
        }
        self.call().await?;
        self.writes += 1;
        Ok(())
    }
}

/// `MemAvailable` from /proc/meminfo; None where there is none.
fn available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
}

// ── Connections ──────────────────────────────────────────────────────

/// The 0.6 conductor: v1.3.
struct Line06 {
    admin: holochain_client::AdminWebsocket,
    cell: holochain_types::prelude::CellId,
    v13: AppClient,
}

/// The 0.7 conductor: v1.4 (when installed) and v1.5.
struct Line07 {
    admin: holochain_client_07::AdminWebsocket,
    v14: Option<(holochain_types_07::prelude::CellId, AppClient)>,
    v15: AppClient,
    /// The node's key on v1.5: the author of what it carries.
    me: AgentPubKey,
}

/// The host part of an admin address: app interfaces are on the same host.
fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(h, _)| h)
}

async fn connect_06(o: &Options) -> Result<Option<Line06>, String> {
    use holochain_client::{
        AdminWebsocket, AllowedOrigins, AppWebsocket, AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner,
        IssueAppAuthenticationTokenPayload,
    };
    let admin = match AdminWebsocket::connect(o.admin_06.clone(), Some(o.origin.clone())).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("carrier: the 0.6 conductor does not answer ({e}); v1.3 is not read this pass");
            return Ok(None);
        }
    };
    let apps = admin.list_apps(None).await.map_err(|e| format!("0.6 list_apps: {e}"))?;
    let Some(cell) = apps.iter().find(|a| a.installed_app_id == APP_V13).and_then(|a| {
        a.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
            CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
            _ => None,
        })
    }) else {
        eprintln!("carrier: v1.3 is not installed on the 0.6 conductor");
        return Ok(None);
    };
    let interfaces = admin.list_app_interfaces().await.map_err(|e| format!("0.6 list_app_interfaces: {e}"))?;
    let port = match interfaces.iter().find(|i| i.installed_app_id.is_none() && matches!(&i.allowed_origins, AllowedOrigins::Origins(s) if s.contains(&o.origin))) {
        Some(i) => i.port,
        None => admin
            .attach_app_interface(0, None, AllowedOrigins::Origins(HashSet::from([o.origin.clone()])), None)
            .await
            .map_err(|e| format!("0.6 attach_app_interface: {e}"))?,
    };
    let signer = ClientAgentSigner::default();
    let creds = admin
        .authorize_signing_credentials(AuthorizeSigningCredentialsPayload { cell_id: cell.clone(), functions: None })
        .await
        .map_err(|e| format!("0.6 signing credentials: {e}"))?;
    signer.add_credentials(cell.clone(), creds);
    let signer: std::sync::Arc<dyn holochain_client::AgentSigner + Send + Sync> = signer.into();
    let token = admin
        .issue_app_auth_token(IssueAppAuthenticationTokenPayload::for_installed_app_id(APP_V13.into()).expiry_seconds(0).single_use(false))
        .await
        .map_err(|e| format!("0.6 auth token: {e}"))?;
    let ws = AppWebsocket::connect(format!("{}:{}", host_of(&o.admin_06), port), token.token, signer, Some(o.origin.clone()))
        .await
        .map_err(|e| format!("v1.3 app websocket: {e}"))?;
    Ok(Some(Line06 { admin, cell, v13: AppClient::V06(ws) }))
}

async fn connect_07(o: &Options) -> Result<Line07, String> {
    use holochain_client_07::{
        AdminWebsocket, AllowedOrigins, AppWebsocket, AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner,
        IssueAppAuthenticationTokenPayload,
    };
    let admin = AdminWebsocket::connect(o.admin_07.clone(), Some(o.origin.clone()))
        .await
        .map_err(|e| format!("the 0.7 conductor does not answer: {e}"))?;
    let apps = admin.list_apps(None).await.map_err(|e| format!("0.7 list_apps: {e}"))?;
    let cell_of = |app_id: &str| {
        apps.iter().find(|a| a.installed_app_id == app_id).and_then(|a| {
            a.cell_info.values().flat_map(|c| c.iter()).find_map(|c| match c {
                CellInfo::Provisioned(p) => Some(p.cell_id.clone()),
                _ => None,
            })
        })
    };
    let cell15 = cell_of(APP_V15).ok_or("v1.5 is not installed on the 0.7 conductor")?;
    let cell14 = cell_of(APP_V14);
    let interfaces = admin.list_app_interfaces().await.map_err(|e| format!("0.7 list_app_interfaces: {e}"))?;
    let port = match interfaces.iter().find(|i| i.installed_app_id.is_none() && matches!(&i.allowed_origins, AllowedOrigins::Origins(s) if s.contains(&o.origin))) {
        Some(i) => i.port,
        None => admin
            .attach_app_interface(0, None, AllowedOrigins::Origins(HashSet::from([o.origin.clone()])), None)
            .await
            .map_err(|e| format!("0.7 attach_app_interface: {e}"))?,
    };
    let signer = ClientAgentSigner::default();
    for cell in std::iter::once(&cell15).chain(cell14.iter()) {
        let creds = admin
            .authorize_signing_credentials(AuthorizeSigningCredentialsPayload { cell_id: cell.clone(), functions: None })
            .await
            .map_err(|e| format!("0.7 signing credentials: {e}"))?;
        signer.add_credentials(cell.clone(), creds);
    }
    let signer: std::sync::Arc<dyn holochain_client_07::AgentSigner + Send + Sync> = signer.into();
    let connect = |app_id: &'static str| {
        let (admin, signer) = (&admin, signer.clone());
        let (host, origin) = (host_of(&o.admin_07), o.origin.clone());
        async move {
            let token = admin
                .issue_app_auth_token(IssueAppAuthenticationTokenPayload::for_installed_app_id(app_id.into()).expiry_seconds(0).single_use(false))
                .await
                .map_err(|e| format!("{app_id} auth token: {e}"))?;
            let ws = AppWebsocket::connect(format!("{host}:{port}"), token.token, signer, Some(origin))
                .await
                .map_err(|e| format!("{app_id} app websocket: {e}"))?;
            Ok::<_, String>(AppClient::V07(ws))
        }
    };
    let v15 = connect(APP_V15).await?;
    let v14 = match cell14 {
        Some(cell) => Some((cell, connect(APP_V14).await?)),
        None => None,
    };
    let me = cell15.agent_pubkey().clone();
    Ok(Line07 { admin, v14, v15, me })
}

// ── Reading ──────────────────────────────────────────────────────────

async fn call<I: Serialize + std::fmt::Debug>(client: &AppClient, f: &str, input: &I) -> Result<Vec<u8>, String> {
    client.call_zome(ROLE, POLLS, f, hc::encode(input)?).await
}

/// The copy on v1.5 of the poll `old` (on its own DNA), when there is one.
async fn copy_of(v15: &AppClient, old: &ActionHash) -> Result<Option<ActionHash>, String> {
    hc::decode(&call(v15, "get_migration_mapping", old).await?)
}

/// What this node holds of an older network: its agent links, and the
/// hashes of what was deleted there.
#[derive(Default)]
struct Held {
    links: Vec<Carried>,
    deleted: HashSet<ActionHash>,
}

async fn held_06(l: &Line06) -> Result<Held, String> {
    use holochain_types::dht_op::{ChainOp, DhtOp};
    use holochain_types::prelude::{Action, EntryType, RecordEntry};
    let dump = l.admin.dump_full_state(l.cell.clone(), None).await.map_err(|e| format!("v1.3 dump: {e}"))?;
    let mut held = Held::default();
    let mut seen = HashSet::new();
    for op in dump.integration_dump.integrated {
        let DhtOp::ChainOp(op) = op else { continue };
        let ChainOp::StoreRecord(signature, action, entry) = *op else { continue };
        if let Action::Delete(d) = &action {
            held.deleted.insert(ActionHash::from_raw_39(d.deletes_address.get_raw_39().to_vec()));
            continue;
        }
        let (Some(EntryType::App(def)), RecordEntry::Present(entry)) = (action.entry_type(), &entry) else { continue };
        if (def.zome_index.0, def.entry_index.0) != LINK {
            continue;
        }
        let Some(bytes) = entry.as_app_entry() else { continue };
        let c = carried::from_v13(&action, &signature, bytes.as_ref().bytes(), None)?;
        if seen.insert(c.original_action_hash.clone()) {
            held.links.push(c);
        }
    }
    let deleted = held.deleted.clone();
    held.links.retain(|c| !deleted.contains(&c.original_action_hash));
    Ok(held)
}

async fn held_07(l: &Line07, pace: &Pace<'_>) -> Result<Held, String> {
    use holochain_types_07::op::{ChainOp, DhtOp, OpEntry};
    use holochain_types_07::prelude::{ActionData, EntryType};
    let mut held = Held::default();
    let Some((cell, _)) = &l.v14 else { return Ok(held) };
    let mut seen = HashSet::new();
    let mut cursor = None;
    loop {
        pace.call().await?;
        let dump = l.admin.dump_full_state(cell.clone(), cursor.take(), Some(DUMP_PAGE)).await.map_err(|e| format!("v1.4 dump: {e}"))?;
        for op in dump.integration_dump.integrated {
            let DhtOp::ChainOp(op) = op else { continue };
            match *op {
                ChainOp::CreateRecord(signed, OpEntry::Present(entry)) => {
                    let action = signed.data();
                    let Some(EntryType::App(def)) = action.entry_type() else { continue };
                    if (def.zome_index.0, def.entry_index.0) != LINK {
                        continue;
                    }
                    let Some(bytes) = entry.as_app_entry() else { continue };
                    let c = carried::from_v14(action, signed.signature(), bytes.as_ref().bytes(), None)?;
                    if seen.insert(c.original_action_hash.clone()) {
                        held.links.push(c);
                    }
                }
                ChainOp::DeleteRecord(signed) | ChainOp::DeleteEntry(signed) => {
                    if let ActionData::Delete(d) = &signed.data().data {
                        held.deleted.insert(d.deletes_address.clone());
                    }
                }
                _ => {}
            }
        }
        match dump.integration_dump.dht_ops_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let deleted = held.deleted.clone();
    held.links.retain(|c| !deleted.contains(&c.original_action_hash));
    Ok(held)
}

/// An agent link's two agents.
#[derive(Deserialize, Debug)]
struct IsSamePerson {
    agent_a: AgentPubKey,
    agent_b: AgentPubKey,
}

/// A v1.5 poll, vote or flag as far as the pass reads it.
#[derive(Deserialize, Debug)]
struct OnV15 {
    #[serde(default)]
    carried_from: Option<Carried>,
}

/// A poll's records on v1.5 by whom they count for: a carried one for the
/// author its proof names, any other for its writer.
struct OnCopy {
    by: HashSet<String>,
    /// Carried records: original hash → (record hash, writer).
    carried: Vec<(ActionHash, String, String)>,
}

async fn on_copy(v15: &AppClient, f: &str, copy: &ActionHash) -> Result<OnCopy, String> {
    let records = hc::records(Line::V07, &call(v15, f, copy).await?)?;
    let mut out = OnCopy { by: HashSet::new(), carried: Vec::new() };
    for r in records {
        match r.entry_as::<OnV15>().ok().and_then(|e| e.carried_from) {
            Some(c) => {
                out.by.insert(c.original_author.to_string());
                out.carried.push((c.original_action_hash, r.action_hash.clone(), r.author.clone()));
            }
            None => {
                out.by.insert(r.author.clone());
            }
        }
    }
    Ok(out)
}

// ── The pass ─────────────────────────────────────────────────────────

pub async fn run(o: &Options) -> Result<Report, String> {
    let mut r = Report { dry_run: o.dry_run, ..Default::default() };
    let l07 = connect_07(o).await?;
    let l06 = connect_06(o).await?;
    r.v13_read = l06.is_some();
    r.v14_read = l07.v14.is_some();
    let mut pace = Pace { o, writes: 0 };
    if let Err(stop) = pass(o, l06.as_ref(), &l07, &mut pace, &mut r).await {
        r.stopped = Some(stop);
    }
    r.wrote = !o.dry_run && r.carried.total() > 0;
    Ok(r)
}

/// Count a record that could not go: its reason on stderr, by kind only.
fn failed(r: &mut Report, what: &str, e: String) {
    eprintln!("carrier: {what} not carried: {e}");
    r.failed += 1;
}

/// Ok(()) at the end; Err(why) when it stopped early (memory, the write
/// limit) or a read it cannot go on without failed.
async fn pass(o: &Options, l06: Option<&Line06>, l07: &Line07, pace: &mut Pace<'_>, r: &mut Report) -> Result<(), String> {
    let v15 = &l07.v15;
    let me = l07.me.to_string();

    // Reads.
    let held13 = match l06 {
        Some(l) => {
            pace.call().await?;
            held_06(l).await?
        }
        None => Held::default(),
    };
    let held14 = held_07(l07, pace).await?;
    let deleted: HashSet<ActionHash> = held13.deleted.union(&held14.deleted).cloned().collect();
    r.seen.deletes = deleted.len() as u64;
    let polls13 = match l06 {
        Some(l) => {
            pace.call().await?;
            carried::from_records("1.3", &call(&l.v13, "get_all_polls", &()).await?)?
        }
        None => Vec::new(),
    };
    let (polls14, maps14) = match &l07.v14 {
        Some((_, c)) => {
            pace.call().await?;
            let polls = carried::from_records("1.4", &call(c, "get_all_polls", &()).await?)?;
            pace.call().await?;
            (polls, carried::from_records("1.4", &call(c, "get_all_migration_mappings", &()).await?)?)
        }
        None => (Vec::new(), Vec::new()),
    };
    r.seen.polls_v13 = polls13.len() as u64;
    r.seen.polls_v14 = polls14.len() as u64;

    // 1. Agent links.
    let links: Vec<&Carried> = held14.links.iter().chain(held13.links.iter()).collect();
    r.seen.links = links.len() as u64;
    let mut linked: HashMap<String, HashSet<String>> = HashMap::new();
    for c in links {
        let Some(pair) = fields::<IsSamePerson>(c) else { continue };
        let a = pair.agent_a.to_string();
        if !linked.contains_key(&a) {
            pace.call().await?;
            let agents: Vec<AgentPubKey> = hc::decode(&call(v15, "get_carried_links", &pair.agent_a).await?)?;
            linked.insert(a.clone(), agents.iter().map(|k| k.to_string()).collect());
        }
        if linked[&a].contains(&pair.agent_b.to_string()) {
            continue;
        }
        if let Err(e) = carried::check(c) {
            failed(r, "an agent link", e);
            continue;
        }
        if !o.dry_run {
            pace.write().await?;
            if let Err(e) = call(v15, "carry_link", &CarryInput { carried: c.clone() }).await {
                failed(r, "an agent link", e);
                continue;
            }
        }
        linked.get_mut(&a).map(|s| s.insert(pair.agent_b.to_string()));
        r.carried.links += 1;
    }

    // Which v1.4 polls are later copies: those a mapping by their own
    // author names as the new copy of a v1.3 poll.
    let later_of: HashMap<ActionHash, (Carried, ActionHash)> = maps14
        .iter()
        .filter_map(|m| fields::<Mapping>(m).map(|f| (f.new_action_hash, (m.clone(), f.old_action_hash))))
        .collect();
    let mut later = Vec::new();
    let mut first: Vec<&Carried> = polls13.iter().collect();
    for p in &polls14 {
        match later_of.get(&p.original_action_hash) {
            Some((m, old)) if m.original_author == p.original_author => later.push((p, m, old.clone())),
            _ => first.push(p),
        }
    }
    r.seen.later_copies = later.len() as u64;

    // 2. First records.
    for p in first {
        pace.call().await?;
        if copy_of(v15, &p.original_action_hash).await?.is_some() {
            continue;
        }
        if let Err(e) = carried::check(p) {
            failed(r, "a poll", e);
            continue;
        }
        if !o.dry_run {
            pace.write().await?;
            if let Err(e) = call(v15, "carry_poll", &CarryInput { carried: p.clone() }).await {
                failed(r, "a poll", e);
                continue;
            }
        }
        r.carried.polls += 1;
    }

    // 3. Later copies under their first record's key.
    for (p, mapping, old) in later {
        pace.call().await?;
        if copy_of(v15, &p.original_action_hash).await?.is_some() {
            continue;
        }
        let Some(copy) = copy_of(v15, &old).await? else {
            if deleted.contains(&old) {
                // Its first record was deleted on v1.3: the later copy is
                // the poll's first record now.
                if let Err(e) = carried::check(p) {
                    failed(r, "a poll", e);
                    continue;
                }
                if !o.dry_run {
                    pace.write().await?;
                    if let Err(e) = call(v15, "carry_poll", &CarryInput { carried: p.clone() }).await {
                        failed(r, "a poll", e);
                        continue;
                    }
                }
                r.carried.polls += 1;
            } else {
                r.waiting += 1;
            }
            continue;
        };
        // The first record: from v1.3, or as its copy carries it.
        let first = match polls13.iter().find(|q| q.original_action_hash == old) {
            Some(q) => Some(q.clone()),
            None => {
                pace.call().await?;
                hc::record_opt(Line::V07, &call(v15, "get_poll", &copy).await?)?.and_then(|rec| rec.entry_as::<OnV15>().ok()).and_then(|e| e.carried_from)
            }
        };
        let Some(first) = first else {
            r.waiting += 1;
            continue;
        };
        if first.original_author != p.original_author {
            r.waiting += 1;
            continue;
        }
        let lc = LaterCopy { poll: p.clone(), mapping: mapping.clone(), vouch: None };
        if let Err(e) = carried::check_later_copy(&first, &lc) {
            failed(r, "a later copy of a poll", e);
            continue;
        }
        if !o.dry_run {
            pace.write().await?;
            if let Err(e) = call(v15, "map_later_copy", &MapLaterCopyInput { copy, later: lc }).await {
                failed(r, "a later copy of a poll", e);
                continue;
            }
        }
        r.carried.later += 1;
    }

    // 4. Votes and flags, v1.4's first; 5. deletes mirrored.
    let mut polls: Vec<(&AppClient, &Carried)> = Vec::new();
    if let Some((_, c)) = &l07.v14 {
        polls.extend(polls14.iter().map(|p| (c, p)));
    }
    if let Some(l) = l06 {
        polls.extend(polls13.iter().map(|p| (&l.v13, p)));
    }
    let mut on: HashMap<(ActionHash, &str), OnCopy> = HashMap::new();
    for (getter, carry, remove, what) in [("get_poll_votes", "carry_vote", None, "a vote"), ("get_poll_flags", "carry_flag", Some("remove_flag"), "a flag")] {
        for (client, p) in &polls {
            pace.call().await?;
            let records = carried::from_records(&p.original_dna, &call(client, getter, &p.original_action_hash).await?)?;
            if getter == "get_poll_votes" {
                r.seen.votes += records.len() as u64;
            } else {
                r.seen.flags += records.len() as u64;
            }
            // With none left here, the copy is still read when something was
            // deleted: a carried flag whose original was removed goes too.
            if records.is_empty() && (remove.is_none() || deleted.is_empty()) {
                continue;
            }
            pace.call().await?;
            let Some(copy) = copy_of(v15, &p.original_action_hash).await? else {
                r.waiting += records.len() as u64;
                continue;
            };
            let key = (copy.clone(), getter);
            if !on.contains_key(&key) {
                pace.call().await?;
                on.insert(key.clone(), on_copy(v15, getter, &copy).await?);
            }
            if records.is_empty() {
                continue;
            }
            pace.call().await?;
            let mapping: Option<ActionHash> = hc::decode(&call(v15, "get_migration_record", &p.original_action_hash).await?)?;
            let Some(mapping) = mapping else {
                r.waiting += records.len() as u64;
                continue;
            };
            for c in records {
                let here = on.get_mut(&key).expect("read above");
                if here.by.contains(&c.original_author.to_string()) {
                    continue;
                }
                let mut c = c;
                c.poll_mapping = Some(mapping.clone());
                if let Err(e) = carried::check(&c) {
                    failed(r, what, e);
                    continue;
                }
                if !o.dry_run {
                    pace.write().await?;
                    if let Err(e) = call(v15, carry, &CarryInput { carried: c.clone() }).await {
                        failed(r, what, e);
                        continue;
                    }
                }
                here.by.insert(c.original_author.to_string());
                if getter == "get_poll_votes" {
                    r.carried.votes += 1;
                } else {
                    r.carried.flags += 1;
                }
            }
        }
        // A flag this node carried whose original its author removed.
        if let Some(remove) = remove {
            for ((_, g), here) in &on {
                if *g != getter {
                    continue;
                }
                for (original, hash, writer) in &here.carried {
                    if *writer != me || !deleted.contains(original) {
                        continue;
                    }
                    if !o.dry_run {
                        pace.write().await?;
                        let h = ActionHash::from_raw_39(hc::hash_bytes(hash)?);
                        if let Err(e) = call(v15, remove, &h).await {
                            failed(r, "a removed flag", e);
                            continue;
                        }
                    }
                    r.carried.deleted += 1;
                }
            }
        }
    }

    // A poll this node carried whose original its author deleted.
    let mut live: Option<Vec<hc::RecordView>> = None;
    for old in &deleted {
        pace.call().await?;
        let Some(copy) = copy_of(v15, old).await? else { continue };
        if live.is_none() {
            pace.call().await?;
            live = Some(hc::records(Line::V07, &call(v15, "get_all_polls", &()).await?)?);
        }
        let copy_s = copy.to_string();
        let mine = live.as_ref().expect("read above").iter().any(|p| p.action_hash == copy_s && p.author == me);
        if !mine {
            continue;
        }
        if !o.dry_run {
            pace.write().await?;
            if let Err(e) = call(v15, "delete_poll", &copy).await {
                failed(r, "a deleted poll", e);
                continue;
            }
        }
        r.carried.deleted += 1;
    }
    Ok(())
}
