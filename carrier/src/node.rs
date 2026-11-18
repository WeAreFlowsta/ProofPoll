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

use crate::carried::{self, fields, Carried, CarryInput, LaterCopy, MapLaterCopyInput, Mapping, OnPoll, FLAG, LINK, MAPPING, POLL, VOTE};
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
    /// Each delete decision on stderr (hashes only).
    pub verbose: bool,
    /// Where the signing credentials are kept between runs (each cell's
    /// key and cap secret, the file owner-readable only). Without it each
    /// run adds a grant to each of the node's chains.
    pub state: Option<std::path::PathBuf>,
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
    /// Signing grants added to the node's chains this run (none while the
    /// kept credentials' grants are live).
    pub grants: u64,
    /// Anything was written on v1.5 this pass.
    pub wrote: bool,
    /// Why the pass stopped before the end (memory, the write limit, a read
    /// that failed).
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
    /// Polls, mappings, votes and flags this node holds that the older
    /// DNAs' own readers did not return (their link is gone, or has not
    /// arrived). Carried all the same.
    pub unlisted: u64,
    /// v1.3 votes whose voter also has a vote on the same poll's copy that
    /// was carried from v1.4 (the 0.5.0 move cast votes again there).
    pub votes_cast_again: u64,
    /// Of those, the v1.3 votes that carry the voter's name: the v1.4 vote
    /// has none, so the name is not shown while the v1.4 one stands.
    pub named_votes_cast_again: u64,
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

// ── Signing credentials ──────────────────────────────────────────────

/// What the carrier calls on v1.3 and v1.4: getters only.
const OLDER_FUNCTIONS: &[&str] = &["get_all_polls", "get_all_migration_mappings", "get_poll_votes", "get_poll_flags"];

/// What it calls on v1.5: the carries, its own copies' removal, and reads.
const V15_FUNCTIONS: &[&str] = &[
    "carry_link",
    "carry_poll",
    "map_later_copy",
    "carry_vote",
    "carry_flag",
    "delete_poll",
    "remove_flag",
    "get_all_polls",
    "get_poll",
    "get_poll_votes",
    "get_poll_flags",
    "get_migration_mapping",
    "get_migration_record",
    "get_carried_links",
];

fn sorted(functions: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = functions.iter().map(|f| f.to_string()).collect();
    v.sort();
    v
}

/// One cell's kept credentials: the signing key and cap secret its grant
/// was made for, and the functions it lists.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Kept {
    key: String,
    secret: String,
    functions: Vec<String>,
}

impl Kept {
    fn new(key: &ed25519_dalek::SigningKey, secret: &[u8], functions: Vec<String>) -> Self {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        Kept { key: b64.encode(key.to_bytes()), secret: b64.encode(secret), functions }
    }

    fn key_and_secret(&self) -> Result<(ed25519_dalek::SigningKey, [u8; 64]), String> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let key: [u8; 32] = b64.decode(&self.key).map_err(|e| e.to_string())?.try_into().map_err(|_| "a key of the wrong length")?;
        let secret: [u8; 64] = b64.decode(&self.secret).map_err(|e| e.to_string())?.try_into().map_err(|_| "a secret of the wrong length")?;
        Ok((ed25519_dalek::SigningKey::from_bytes(&key), secret))
    }
}

/// The credentials file: one entry per cell ("<dna> <agent>").
#[derive(Serialize, Deserialize, Default)]
struct State {
    #[serde(default)]
    cells: std::collections::BTreeMap<String, Kept>,
    #[serde(skip)]
    path: Option<std::path::PathBuf>,
    #[serde(skip)]
    changed: bool,
}

impl State {
    /// The kept file, or an empty state (no file yet, none asked for, or
    /// one that does not read: then new grants replace it).
    fn load(path: Option<&std::path::Path>) -> State {
        let Some(path) = path else { return State::default() };
        let mut state = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes).unwrap_or_else(|e| {
                eprintln!("carrier: the credentials file does not read ({e}); new grants replace it");
                State::default()
            }),
            Err(_) => State::default(),
        };
        state.path = Some(path.to_path_buf());
        state
    }

    fn keep(&mut self, cell: String, kept: Kept) {
        if self.path.is_some() {
            self.cells.insert(cell, kept);
            self.changed = true;
        }
    }

    /// Written whole to a file only its owner reads, then moved into place.
    fn save(&self) -> Result<(), String> {
        let (Some(path), true) = (&self.path, self.changed) else { return Ok(()) };
        let tmp = path.with_extension("tmp");
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut f = open.open(&tmp).map_err(|e| format!("the credentials file: {e}"))?;
        std::io::Write::write_all(&mut f, &bytes).map_err(|e| format!("the credentials file: {e}"))?;
        f.sync_all().map_err(|e| format!("the credentials file: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("the credentials file: {e}"))
    }
}

/// Whether a listed grant (secrets removed) is assigned to `key`: the
/// assignees sit under `access` on 0.6 and `constraint` on 0.7, both
/// private fields, so they are read from the grant's serialized form.
fn assigned_to<G: Serialize, K: Serialize>(grant: &G, key: &K) -> bool {
    let (Ok(g), Ok(k)) = (serde_json::to_value(grant), serde_json::to_value(key)) else { return false };
    g.get("access")
        .or_else(|| g.get("constraint"))
        .and_then(|a| a.get("assignees"))
        .and_then(|a| a.as_array())
        .is_some_and(|a| a.contains(&k))
}

/// A cell's signing credentials, the same on both lines: the kept ones
/// while their grant is live and lists today's functions; otherwise a new
/// grant (the kept one revoked first when it is live with other functions).
macro_rules! credentials {
    ($name:ident, $client:ident, $types:ident) => {
        async fn $name(
            admin: &$client::AdminWebsocket,
            app_id: &str,
            cell: &$client::CellId,
            functions: &[&str],
            state: &mut State,
            grants: &mut u64,
        ) -> Result<$client::SigningCredentials, String> {
            use $client::{AuthorizeSigningCredentialsPayload, GrantedFunctions, SigningCredentials};
            use $types::prelude::{AgentPubKey, CapSecret, FunctionName, ZomeName};
            let id = format!("{} {}", cell.dna_hash(), cell.agent_pubkey());
            let wanted = sorted(functions);
            if let Some(kept) = state.cells.get(&id).cloned() {
                match kept.key_and_secret() {
                    Ok((keypair, secret)) => {
                        let signing_agent_key = AgentPubKey::from_raw_32(keypair.verifying_key().as_bytes().to_vec());
                        // Every grant, revoked ones too, filtered below: asked
                        // without them, Holochain 0.6.1 lists none at all.
                        let listed = admin
                            .list_capability_grants(app_id.to_string(), true)
                            .await
                            .map_err(|e| format!("{app_id} grants: {e}"))?;
                        let live = listed
                            .0
                            .iter()
                            .filter(|(c, _)| c == cell)
                            .flat_map(|(_, g)| g.iter())
                            .find(|g| g.revoked_at.is_none() && assigned_to(&g.cap_grant, &signing_agent_key));
                        match live {
                            Some(_) if kept.functions == wanted => {
                                return Ok(SigningCredentials { signing_agent_key, keypair, cap_secret: CapSecret::from(secret) });
                            }
                            Some(g) => {
                                admin
                                    .revoke_zome_call_capability(cell.clone(), g.action_hash.clone())
                                    .await
                                    .map_err(|e| format!("{app_id} revoke the old grant: {e}"))?;
                            }
                            None => eprintln!("carrier: the kept grant on {app_id} is gone; a new one"),
                        }
                    }
                    Err(e) => eprintln!("carrier: the kept credentials for {app_id} do not read ({e}); a new grant"),
                }
            }
            let listed = GrantedFunctions::Listed(functions.iter().map(|f| (ZomeName::from(POLLS), FunctionName::from(*f))).collect());
            let creds = admin
                .authorize_signing_credentials(AuthorizeSigningCredentialsPayload { cell_id: cell.clone(), functions: Some(listed) })
                .await
                .map_err(|e| format!("{app_id} signing credentials: {e}"))?;
            *grants += 1;
            state.keep(id, Kept::new(&creds.keypair, creds.cap_secret.as_ref(), wanted));
            Ok(creds)
        }
    };
}

credentials!(credentials_06, holochain_client, holochain_types);
credentials!(credentials_07, holochain_client_07, holochain_types_07);

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

async fn connect_06(o: &Options, state: &mut State, grants: &mut u64) -> Result<Option<Line06>, String> {
    use holochain_client::{AdminWebsocket, AllowedOrigins, AppWebsocket, CellInfo, ClientAgentSigner, IssueAppAuthenticationTokenPayload};
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
    let creds = credentials_06(&admin, APP_V13, &cell, OLDER_FUNCTIONS, state, grants).await?;
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

async fn connect_07(o: &Options, state: &mut State, grants: &mut u64) -> Result<Line07, String> {
    use holochain_client_07::{AdminWebsocket, AllowedOrigins, AppWebsocket, CellInfo, ClientAgentSigner, IssueAppAuthenticationTokenPayload};
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
    signer.add_credentials(cell15.clone(), credentials_07(&admin, APP_V15, &cell15, V15_FUNCTIONS, state, grants).await?);
    if let Some(cell) = &cell14 {
        signer.add_credentials(cell.clone(), credentials_07(&admin, APP_V14, cell, OLDER_FUNCTIONS, state, grants).await?);
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
/// hashes of what was deleted there BY ITS OWN AUTHOR.
#[derive(Default)]
struct Held {
    links: Vec<Carried>,
    deleted: HashSet<ActionHash>,
    /// Every poll, mapping, vote and flag this node holds, by kind. The
    /// older DNAs' readers follow links, and those DNAs accept a link
    /// delete from anyone, so a record whose link someone else deleted is
    /// missing from a reader's answer; it is still here, signed.
    records: Vec<((u8, u8), Carried)>,
}

impl Held {
    /// `got` (a reader's answer) with the records of `kind` held here that
    /// it left out. Returns how many were added.
    fn add_to(&self, got: &mut Vec<Carried>, kind: (u8, u8), on_poll: Option<&ActionHash>) -> u64 {
        let mut have: HashSet<ActionHash> = got.iter().map(|c| c.original_action_hash.clone()).collect();
        let mut added = 0;
        for (k, c) in &self.records {
            if *k != kind || have.contains(&c.original_action_hash) {
                continue;
            }
            if let Some(poll) = on_poll {
                if fields::<OnPoll>(c).map(|f| &f.poll_action_hash != poll).unwrap_or(true) {
                    continue;
                }
            }
            have.insert(c.original_action_hash.clone());
            got.push(c.clone());
            added += 1;
        }
        added
    }

    /// After the reads: drop what its own author deleted.
    fn settle(&mut self, deleted: HashSet<ActionHash>) {
        self.links.retain(|c| !deleted.contains(&c.original_action_hash));
        self.records.retain(|(_, c)| !deleted.contains(&c.original_action_hash));
        self.deleted = deleted;
    }
}

/// The kinds read from what the node holds, beside agent links.
const HELD_KINDS: [(u8, u8); 4] = [POLL, VOTE, FLAG, MAPPING];

/// The deletes seen on an older network, with who wrote each record and who
/// wrote each delete. The older DNAs check no author on a delete, so anyone
/// could write one for another person's poll, vote, flag or link; only a
/// delete by the record's own author counts here. A delete of a record this
/// node does not hold (its author is not known) does not count either: the
/// copy stays, which loses nothing.
#[derive(Default)]
struct Deletes {
    authors: HashMap<ActionHash, AgentPubKey>,
    deletes: Vec<(ActionHash, AgentPubKey)>,
}

impl Deletes {
    fn wrote(&mut self, record: ActionHash, author: AgentPubKey) {
        self.authors.insert(record, author);
    }
    fn deleted(&mut self, record: ActionHash, by: AgentPubKey) {
        self.deletes.push((record, by));
    }
    /// The records deleted by their own author.
    fn by_author(&self) -> HashSet<ActionHash> {
        self.deletes.iter().filter(|(record, by)| self.authors.get(record) == Some(by)).map(|(record, _)| record.clone()).collect()
    }
}

async fn held_06(l: &Line06) -> Result<Held, String> {
    use holochain_types::dht_op::{ChainOp, DhtOp};
    use holochain_types::prelude::{Action, EntryType, RecordEntry};
    let dump = l.admin.dump_full_state(l.cell.clone(), None).await.map_err(|e| format!("v1.3 dump: {e}"))?;
    let mut held = Held::default();
    let mut deletes = Deletes::default();
    let mut seen = HashSet::new();
    for op in dump.integration_dump.integrated {
        let DhtOp::ChainOp(op) = op else { continue };
        let ChainOp::StoreRecord(signature, action, entry) = *op else { continue };
        let author = AgentPubKey::from_raw_39(action.author().get_raw_39().to_vec());
        if let Action::Delete(d) = &action {
            deletes.deleted(ActionHash::from_raw_39(d.deletes_address.get_raw_39().to_vec()), author);
            continue;
        }
        let encoded = holochain_types::prelude::encode(&action).map_err(|e| format!("encode a v1.3 action: {e}"))?;
        deletes.wrote(carried::action_hash_of(&encoded), author);
        let (Some(EntryType::App(def)), RecordEntry::Present(entry)) = (action.entry_type(), &entry) else { continue };
        let kind = (def.zome_index.0, def.entry_index.0);
        // A poll, mapping, vote or flag is its create record (the older
        // DNAs never update one).
        let wanted = kind == LINK || (HELD_KINDS.contains(&kind) && matches!(action, Action::Create(_)));
        if !wanted {
            continue;
        }
        let Some(bytes) = entry.as_app_entry() else { continue };
        let c = carried::from_v13(&action, &signature, bytes.as_ref().bytes(), None)?;
        if seen.insert(c.original_action_hash.clone()) {
            if kind == LINK {
                held.links.push(c);
            } else {
                held.records.push((kind, c));
            }
        }
    }
    held.settle(deletes.by_author());
    Ok(held)
}

async fn held_07(l: &Line07, pace: &Pace<'_>) -> Result<Held, String> {
    use holochain_types_07::op::{ChainOp, DhtOp, OpEntry};
    use holochain_types_07::prelude::{ActionData, EntryType};
    let mut held = Held::default();
    let Some((cell, _)) = &l.v14 else { return Ok(held) };
    let mut deletes = Deletes::default();
    let mut seen = HashSet::new();
    let mut cursor = None;
    loop {
        pace.call().await?;
        let dump = l.admin.dump_full_state(cell.clone(), cursor.take(), Some(DUMP_PAGE)).await.map_err(|e| format!("v1.4 dump: {e}"))?;
        for op in dump.integration_dump.integrated {
            let DhtOp::ChainOp(op) = op else { continue };
            match *op {
                ChainOp::CreateRecord(signed, entry) => {
                    let action = signed.data();
                    let encoded = holochain_types_07::prelude::encode(action).map_err(|e| format!("encode a v1.4 action: {e}"))?;
                    deletes.wrote(carried::action_hash_of(&encoded), action.author().clone());
                    let OpEntry::Present(entry) = entry else { continue };
                    let Some(EntryType::App(def)) = action.entry_type() else { continue };
                    let kind = (def.zome_index.0, def.entry_index.0);
                    if kind != LINK && !HELD_KINDS.contains(&kind) {
                        continue;
                    }
                    let Some(bytes) = entry.as_app_entry() else { continue };
                    let c = carried::from_v14(action, signed.signature(), bytes.as_ref().bytes(), None)?;
                    if seen.insert(c.original_action_hash.clone()) {
                        if kind == LINK {
                            held.links.push(c);
                        } else {
                            held.records.push((kind, c));
                        }
                    }
                }
                ChainOp::DeleteRecord(signed) | ChainOp::DeleteEntry(signed) => {
                    if let ActionData::Delete(d) = &signed.data().data {
                        deletes.deleted(d.deletes_address.clone(), signed.data().author().clone());
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
    held.settle(deletes.by_author());
    Ok(held)
}

/// A vote's name, when its voter gave one (public polls only).
#[derive(Deserialize, Debug)]
struct Named {
    #[serde(default)]
    display_name: Option<String>,
}

/// Which keys are one person, from the agent links this node holds: two
/// keys are one person when they are the same key, linked to each other, or
/// linked to a key in common (an install's key on each network is linked to
/// the same identity).
#[derive(Default)]
struct People {
    linked: HashMap<String, HashSet<String>>,
}

impl People {
    fn link(&mut self, a: &str, b: &str) {
        self.linked.entry(a.to_string()).or_default().insert(b.to_string());
        self.linked.entry(b.to_string()).or_default().insert(a.to_string());
    }
    fn same(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        match (self.linked.get(a), self.linked.get(b)) {
            (Some(la), Some(lb)) => la.contains(b) || la.iter().any(|k| lb.contains(k)),
            (Some(la), None) => la.contains(b),
            (None, Some(lb)) => lb.contains(a),
            (None, None) => false,
        }
    }
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
    /// The original authors of the votes or flags here carried from v1.4.
    from_v14: Vec<String>,
    /// Carried records: original hash → (record hash, writer).
    carried: Vec<(ActionHash, String, String)>,
}

async fn on_copy(v15: &AppClient, f: &str, copy: &ActionHash) -> Result<OnCopy, String> {
    let records = hc::records(Line::V07, &call(v15, f, copy).await?)?;
    let mut out = OnCopy { by: HashSet::new(), from_v14: Vec::new(), carried: Vec::new() };
    for r in records {
        match r.entry_as::<OnV15>().ok().and_then(|e| e.carried_from) {
            Some(c) => {
                if c.original_dna == "1.4" {
                    out.from_v14.push(c.original_author.to_string());
                }
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
    let mut state = State::load(o.state.as_deref());
    let connected = async {
        let l07 = connect_07(o, &mut state, &mut r.grants).await?;
        let l06 = connect_06(o, &mut state, &mut r.grants).await?;
        Ok::<_, String>((l07, l06))
    }
    .await;
    // Kept even when a connection failed after a grant was made. A file
    // that cannot be written does not stop the pass: the next run adds
    // grants again, which its report's `grants` shows.
    if let Err(e) = state.save() {
        eprintln!("carrier: the credentials were not kept ({e})");
    }
    let (l07, l06) = connected?;
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
    let mut polls13 = match l06 {
        Some(l) => {
            pace.call().await?;
            carried::from_records("1.3", &call(&l.v13, "get_all_polls", &()).await?)?
        }
        None => Vec::new(),
    };
    let (mut polls14, mut maps14) = match &l07.v14 {
        Some((_, c)) => {
            pace.call().await?;
            let polls = carried::from_records("1.4", &call(c, "get_all_polls", &()).await?)?;
            pace.call().await?;
            (polls, carried::from_records("1.4", &call(c, "get_all_migration_mappings", &()).await?)?)
        }
        None => (Vec::new(), Vec::new()),
    };
    // What the readers left out and this node holds (see `Held::records`).
    r.seen.unlisted += held13.add_to(&mut polls13, POLL, None);
    r.seen.unlisted += held14.add_to(&mut polls14, POLL, None);
    r.seen.unlisted += held14.add_to(&mut maps14, MAPPING, None);
    r.seen.polls_v13 = polls13.len() as u64;
    r.seen.polls_v14 = polls14.len() as u64;

    // 1. Agent links.
    let links: Vec<&Carried> = held14.links.iter().chain(held13.links.iter()).collect();
    r.seen.links = links.len() as u64;
    let mut linked: HashMap<String, HashSet<String>> = HashMap::new();
    let mut people = People::default();
    for c in links {
        let Some(pair) = fields::<IsSamePerson>(c) else { continue };
        let a = pair.agent_a.to_string();
        people.link(&a, &pair.agent_b.to_string());
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
            let mut records = carried::from_records(&p.original_dna, &call(client, getter, &p.original_action_hash).await?)?;
            let held = if p.original_dna == "1.3" { &held13 } else { &held14 };
            r.seen.unlisted += held.add_to(&mut records, if getter == "get_poll_votes" { VOTE } else { FLAG }, Some(&p.original_action_hash));
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
            if o.verbose && !deleted.is_empty() {
                let here = &on[&key];
                eprintln!("carrier: {getter} on copy {copy}: {} carried, {} by this node", here.carried.len(), here.carried.iter().filter(|(_, _, w)| *w == me).count());
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
                // A v1.3 vote whose voter has a vote here carried from v1.4:
                // counted, with whether the v1.3 one carries a name.
                if getter == "get_poll_votes" && c.original_dna == "1.3" {
                    let voter = c.original_author.to_string();
                    if here.from_v14.iter().any(|k| people.same(k, &voter)) {
                        r.seen.votes_cast_again += 1;
                        if fields::<Named>(&c).and_then(|n| n.display_name).map(|n| !n.trim().is_empty()).unwrap_or(false) {
                            r.seen.named_votes_cast_again += 1;
                        }
                    }
                }
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
                if c.original_dna == "1.4" {
                    here.from_v14.push(c.original_author.to_string());
                }
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
                    if o.verbose && deleted.contains(original) {
                        eprintln!("carrier: removed flag {original}: copy {hash}, made here {}", *writer == me);
                    }
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
        let Some(copy) = copy_of(v15, old).await? else {
            if o.verbose {
                eprintln!("carrier: deleted {old}: no copy here");
            }
            continue;
        };
        if live.is_none() {
            pace.call().await?;
            live = Some(hc::records(Line::V07, &call(v15, "get_all_polls", &()).await?)?);
        }
        let copy_s = copy.to_string();
        let listed = live.as_ref().expect("read above").iter().find(|p| p.action_hash == copy_s);
        let mine = listed.is_some_and(|p| p.author == me);
        if o.verbose {
            eprintln!("carrier: deleted poll {old}: copy {copy_s}, listed {}, made here {mine}", listed.is_some());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two keys are one person when linked to each other or to one identity.
    #[test]
    fn keys_linked_to_one_identity_are_one_person() {
        let mut p = People::default();
        p.link("key on v1.3", "identity");
        p.link("key on v1.4", "identity");
        p.link("someone else", "another identity");
        assert!(p.same("key on v1.3", "key on v1.3"));
        assert!(p.same("key on v1.3", "identity"));
        assert!(p.same("key on v1.3", "key on v1.4"), "both linked to the same identity");
        assert!(!p.same("key on v1.3", "someone else"));
        assert!(!p.same("unlinked", "key on v1.3"));
        assert!(p.same("unlinked", "unlinked"));
    }

    fn made(n: u8, poll: Option<u8>) -> Carried {
        #[derive(Serialize)]
        struct Entry {
            poll_action_hash: ActionHash,
        }
        let hash = |n: u8| ActionHash::from_raw_36(vec![n; 36]);
        Carried {
            original_dna: "1.4".into(),
            original_action_hash: hash(n),
            original_author: AgentPubKey::from_raw_36(vec![n; 36]),
            original_timestamp: holochain_types_07::prelude::Timestamp::from_micros(0),
            original_action: Vec::new(),
            original_signature: holochain_types_07::prelude::Signature([0; 64]),
            original_entry: poll.map(|p| rmp_serde::to_vec_named(&Entry { poll_action_hash: hash(p) }).unwrap()).unwrap_or_default(),
            poll_mapping: None,
        }
    }

    /// A record this node holds is carried even when the older DNA's reader
    /// left it out (anyone could delete its link there), once, and a vote
    /// only on its own poll. What its author deleted stays out.
    #[test]
    fn what_the_node_holds_is_added_to_what_the_readers_return() {
        let hash = |n: u8| ActionHash::from_raw_36(vec![n; 36]);
        let mut held = Held::default();
        held.records.push((POLL, made(1, None)));
        held.records.push((POLL, made(2, None)));
        held.records.push((VOTE, made(10, Some(1))));
        held.records.push((VOTE, made(11, Some(2))));
        held.records.push((VOTE, made(12, Some(1))));
        held.settle([hash(12)].into_iter().collect());

        // The reader returned poll 1 only: poll 2 is added, poll 1 is not doubled.
        let mut polls = vec![made(1, None)];
        assert_eq!(held.add_to(&mut polls, POLL, None), 1);
        assert_eq!(polls.iter().map(|c| c.original_action_hash.clone()).collect::<Vec<_>>(), vec![hash(1), hash(2)]);

        // Poll 1's votes: the reader returned none. Vote 10 is added; vote
        // 11 is on another poll; vote 12 was deleted by its author.
        let mut votes = Vec::new();
        assert_eq!(held.add_to(&mut votes, VOTE, Some(&hash(1))), 1);
        assert_eq!(votes[0].original_action_hash, hash(10));
        // Asked again, nothing more is added.
        assert_eq!(held.add_to(&mut votes, VOTE, Some(&hash(1))), 0);
    }

    /// Only a record's own author can have it left out or its copy removed:
    /// the older DNAs let anyone write a delete for anyone's record.
    #[test]
    fn only_the_authors_own_delete_counts() {
        let agent = |n: u8| AgentPubKey::from_raw_36(vec![n; 36]);
        let record = |n: u8| ActionHash::from_raw_36(vec![n; 36]);
        let (author, stranger) = (agent(1), agent(2));
        let mut d = Deletes::default();
        d.wrote(record(10), author.clone());
        d.wrote(record(11), author.clone());
        d.wrote(record(12), author.clone());
        d.deleted(record(10), author.clone()); // its author's own delete
        d.deleted(record(11), stranger.clone()); // a stranger's delete
        d.deleted(record(12), stranger.clone()); // both: the author's counts
        d.deleted(record(12), author.clone());
        d.deleted(record(13), stranger.clone()); // of a record not held here
        let counted = d.by_author();
        assert!(counted.contains(&record(10)));
        assert!(!counted.contains(&record(11)), "a stranger's delete of another person's record is ignored");
        assert!(counted.contains(&record(12)));
        assert!(!counted.contains(&record(13)), "the author is not known: the copy stays");
        assert_eq!(counted.len(), 2);
    }

    /// Every zome function this file names is in a grant's list, so no
    /// call is refused for a function the grant leaves out.
    #[test]
    fn every_function_called_is_granted() {
        let full = include_str!("node.rs");
        let src = &full[..full.find("#[cfg(test)]").unwrap()];
        let granted: HashSet<&str> = V15_FUNCTIONS.iter().chain(OLDER_FUNCTIONS).copied().collect();
        let prefixes = ["get_", "carry_", "map_", "delete_", "remove_"];
        // Each `"name"` token: a quote, lowercase letters and underscores, a quote.
        let mut named = HashSet::new();
        let bytes = src.as_bytes();
        for (i, _) in src.match_indices('"') {
            let rest = &src[i + 1..];
            let len = rest.bytes().take_while(|b| b.is_ascii_lowercase() || *b == b'_').count();
            let name = &rest[..len];
            let closed = bytes.get(i + 1 + len) == Some(&b'"');
            if closed && prefixes.iter().any(|p| name.starts_with(p) && name.len() > p.len()) {
                named.insert(name);
            }
        }
        assert!(named.len() >= 14, "the source scan found {named:?}");
        for f in &named {
            assert!(granted.contains(f), "{f} is called but in no grant");
        }
    }

    /// The older DNAs are only read.
    #[test]
    fn the_older_grant_is_getters_only() {
        assert!(OLDER_FUNCTIONS.iter().all(|f| f.starts_with("get_")));
    }

    /// Kept credentials read back to the same key and secret.
    #[test]
    fn kept_credentials_round_trip() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let secret = [9u8; 64];
        let kept = Kept::new(&key, &secret, sorted(&["b", "a"]));
        let json = serde_json::to_string(&kept).unwrap();
        let back: Kept = serde_json::from_str(&json).unwrap();
        let (key2, secret2) = back.key_and_secret().unwrap();
        assert_eq!(key2.to_bytes(), key.to_bytes());
        assert_eq!(secret2, secret);
        assert_eq!(back.functions, vec!["a".to_string(), "b".to_string()]);
    }

    /// A grant's assignees are found under either line's field name.
    #[test]
    fn assignees_found_on_both_shapes() {
        #[derive(Serialize)]
        struct Access {
            access_type: String,
            assignees: Option<Vec<u8>>,
        }
        #[derive(Serialize)]
        struct G06 {
            access: Access,
        }
        #[derive(Serialize)]
        struct G07 {
            constraint: Access,
        }
        let a = || Access { access_type: "assigned".into(), assignees: Some(vec![1, 2]) };
        assert!(assigned_to(&G06 { access: a() }, &1u8));
        assert!(assigned_to(&G07 { constraint: a() }, &2u8));
        assert!(!assigned_to(&G07 { constraint: a() }, &3u8));
    }

    /// The file is written owner-readable only, and read back.
    #[test]
    fn state_file_is_private() {
        let dir = std::env::temp_dir().join(format!("carrier-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        let mut s = State::load(Some(&path));
        s.keep("dna agent".into(), Kept::new(&ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]), &[2u8; 64], vec![]));
        s.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let back = State::load(Some(&path));
        assert_eq!(back.cells.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
