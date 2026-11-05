//! ProofPoll coordinator zome (v1.5: v1.4 with carried records the network
//! checks).
//!
//! New in v1.5:
//!   - `carry_poll`, `carry_vote`, `carry_flag`, `carry_link`: write a record
//!     from v1.3 or v1.4 with its original record attached, once per
//!     original (a node carrying an older network calls them, and so does an
//!     app moving its own records; the integrity zome checks each proof)
//!   - `get_migration_record`, `get_carried_links`
//!   - the one-vote and one-flag rules count a carried record for its proven
//!     voter, and a person's agents include their carried links
//!   - `register_migrated_poll` is gone: a mapping comes with a carried poll
//!
//! Encrypted entries, drafts and everything else are as in v1.4.

use hdk::prelude::*;
use polls_integrity::*;

#[hdk_dependent_entry_types]
enum EntryZomes {
    Integrity(polls_integrity::EntryTypes),
}

// ── Configuration ─────────────────────────────────────────────────────

/// Minimum flags from unique agents before the UI hides a poll.
///
/// Forking developers: change this to suit your community size.
pub const FLAG_HIDE_THRESHOLD: u32 = 3;

// ── Input types ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
    /// Whether this poll shows voter identities. Defaults to Anonymous.
    pub poll_type: PollType,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Voter's display name - required for public polls, None for anonymous.
    pub display_name: Option<String>,
    /// Voter's profile picture - required for public polls, None for anonymous.
    pub profile_picture: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FlagPollInput {
    pub poll_action_hash: ActionHash,
    pub reason: FlagReason,
}

/// A record from v1.3 or v1.4 with its proof (the integrity zome's `Carried`).
#[derive(Serialize, Deserialize, Debug)]
pub struct CarryInput {
    pub carried: Carried,
}

/// A later copy of a poll (a v1.4 copy of a v1.3 poll, with its v1.4
/// mapping) and the poll's one copy here.
#[derive(Serialize, Deserialize, Debug)]
pub struct MapLaterCopyInput {
    pub copy: ActionHash,
    pub later: LaterCopy,
}

// ── Poll functions ─────────────────────────────────────────────────────

#[hdk_extern]
pub fn create_poll(input: CreatePollInput) -> ExternResult<ActionHash> {
    let now = sys_time()?.as_seconds_and_nanos().0;

    if let Some(closes_at) = input.closes_at {
        if closes_at <= now {
            return Err(wasm_error!("Poll closing time must be in the future"));
        }
    }

    let poll = Poll {
        title: input.title,
        description: input.description,
        options: input.options,
        created_at: now,
        closes_at: input.closes_at,
        poll_type: input.poll_type,
        carried_from: None,
    };

    let action_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Poll(poll)))?;

    let anchor = all_polls_anchor()?;
    create_link(anchor, action_hash.clone(), LinkTypes::AllPolls, ())?;

    Ok(action_hash)
}

#[hdk_extern]
pub fn get_poll(action_hash: ActionHash) -> ExternResult<Option<Record>> {
    get(action_hash, GetOptions::default())
}

#[hdk_extern]
pub fn get_all_polls(_: ()) -> ExternResult<Vec<Record>> {
    let anchor = all_polls_anchor()?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::AllPolls)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid poll link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}

#[hdk_extern]
pub fn delete_poll(action_hash: ActionHash) -> ExternResult<ActionHash> {
    let record = get(action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;
    let my_agent = agent_info()?.agent_initial_pubkey;
    if *record.action().author() != my_agent {
        return Err(wasm_error!("Only the poll creator can delete it"));
    }

    let anchor = all_polls_anchor()?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::AllPolls)?,
        GetStrategy::default(),
    )?;
    for link in links {
        if let Ok(target) = ActionHash::try_from(link.target) {
            if target == action_hash {
                delete_link(link.create_link_hash, GetOptions::default())?;
            }
        }
    }

    delete_entry(action_hash)
}

// ── Vote functions ─────────────────────────────────────────────────────

/// Call the agent-linking zome (same cell) to get the agents directly linked
/// to `agent`. Local same-cell call by the cell's own agent → no cap secret.
fn linked_agents(agent: AgentPubKey) -> ExternResult<Vec<AgentPubKey>> {
    let response = call(
        CallTargetCell::Local,
        "agent_linking",
        "get_linked_agents".into(),
        None,
        agent,
    )?;
    match response {
        ZomeCallResponse::Ok(io) => io
            .decode()
            .map_err(|e| wasm_error!(format!("decode linked agents: {:?}", e))),
        other => Err(wasm_error!(format!(
            "get_linked_agents call failed: {:?}",
            other
        ))),
    }
}

/// The full set of agent keys belonging to this person: the current agent plus
/// every ProofPoll agent linked to the same Flowsta Vault identity (across the
/// user's other devices/installs). Used so a reinstalled user can't vote twice.
///
/// Two hops: `linked_agents(me)` → the Vault agent(s) I'm linked to; then
/// `linked_agents(vault)` → all the local ProofPoll agents linked to that Vault
/// identity (the agent-linking zome indexes the link from the Vault pubkey too).
///
/// Degrades gracefully: if the agent-linking zome can't be reached for any
/// reason, falls back to just the current agent (the prior per-agent behaviour)
/// rather than blocking the vote.
fn my_identity_agents(my_agent: &AgentPubKey) -> BTreeSet<AgentPubKey> {
    // Native links (the agent-linking zome) and carried ones, at both hops.
    let linked = |agent: &AgentPubKey| -> Vec<AgentPubKey> {
        let mut out = linked_agents(agent.clone()).unwrap_or_default();
        out.extend(carried_links_of(agent).unwrap_or_default());
        out
    };
    let mut set = BTreeSet::new();
    set.insert(my_agent.clone());
    for hub in linked(my_agent) {
        for s in linked(&hub) {
            set.insert(s);
        }
        set.insert(hub);
    }
    set
}

/// Who a vote or flag link counts for: the voter its tag names (a carried
/// record's, checked by integrity), else the link's author.
fn counted_for(link: &Link) -> AgentPubKey {
    AgentPubKey::try_from_raw_39(link.tag.0.clone()).unwrap_or_else(|_| link.author.clone())
}

/// The tag a carried vote's or flag's link carries: its original voter.
fn voter_tag(c: &Carried) -> LinkTag {
    LinkTag::new(c.original_author.get_raw_39().to_vec())
}

#[hdk_extern]
pub fn cast_vote(input: CastVoteInput) -> ExternResult<ActionHash> {
    let poll_record = get(input.poll_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;

    let poll: Poll = poll_record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize poll"))?
        .ok_or(wasm_error!("Poll entry is None"))?;

    if input.option_index as usize >= poll.options.len() {
        return Err(wasm_error!("Invalid option index"));
    }

    if let Some(closes_at) = poll.closes_at {
        let now = sys_time()?.as_seconds_and_nanos().0;
        if now > closes_at {
            return Err(wasm_error!("Poll is closed"));
        }
    }

    // Require identity fields on public polls
    if poll.poll_type == PollType::Public && input.display_name.is_none() {
        return Err(wasm_error!("Display name is required for public polls"));
    }

    // Check for double-vote - across ALL of this person's agents, not just the
    // current one. ProofPoll mints a fresh agent key per install, so a
    // per-agent check would let a reinstalled (or multi-device) user vote
    // again. We dedup against the user's full linked-agent set instead.
    let my_agent = agent_info()?.agent_initial_pubkey;
    let my_agents = my_identity_agents(&my_agent);
    let existing_links = get_links(
        LinkQuery::try_new(input.poll_action_hash.clone(), LinkTypes::PollToVotes)?,
        GetStrategy::default(),
    )?;
    for link in &existing_links {
        if my_agents.contains(&counted_for(link)) {
            return Err(wasm_error!("You have already voted on this poll"));
        }
    }

    let vote = Vote {
        poll_action_hash: input.poll_action_hash.clone(),
        option_index: input.option_index,
        display_name: input.display_name,
        profile_picture: input.profile_picture,
        carried_from: None,
    };

    let vote_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Vote(vote)))?;

    create_link(
        input.poll_action_hash,
        vote_hash.clone(),
        LinkTypes::PollToVotes,
        (),
    )?;

    Ok(vote_hash)
}

#[hdk_extern]
pub fn get_poll_votes(poll_action_hash: ActionHash) -> ExternResult<Vec<Record>> {
    let links = get_links(
        LinkQuery::try_new(poll_action_hash, LinkTypes::PollToVotes)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid vote link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}

// ── Flag functions (unchanged from v1.1) ──────────────────────────────

#[hdk_extern]
pub fn flag_poll(input: FlagPollInput) -> ExternResult<ActionHash> {
    let _poll_record = get(input.poll_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;

    let my_agent = agent_info()?.agent_initial_pubkey;
    let existing_flags = get_links(
        LinkQuery::try_new(input.poll_action_hash.clone(), LinkTypes::PollToFlags)?,
        GetStrategy::default(),
    )?;
    for link in &existing_flags {
        if counted_for(link) == my_agent {
            return Err(wasm_error!("You have already flagged this poll"));
        }
    }

    let now = sys_time()?.as_seconds_and_nanos().0;
    let flag = Flag {
        poll_action_hash: input.poll_action_hash.clone(),
        reason: input.reason,
        created_at: now,
        carried_from: None,
    };

    let flag_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Flag(flag)))?;

    create_link(
        input.poll_action_hash,
        flag_hash.clone(),
        LinkTypes::PollToFlags,
        (),
    )?;

    Ok(flag_hash)
}

#[hdk_extern]
pub fn get_poll_flags(poll_action_hash: ActionHash) -> ExternResult<Vec<Record>> {
    let links = get_links(
        LinkQuery::try_new(poll_action_hash, LinkTypes::PollToFlags)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid flag link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}

#[hdk_extern]
pub fn remove_flag(flag_action_hash: ActionHash) -> ExternResult<ActionHash> {
    let record = get(flag_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Flag not found"))?;

    let my_agent = agent_info()?.agent_initial_pubkey;
    if *record.action().author() != my_agent {
        return Err(wasm_error!("Only the flag author can remove it"));
    }

    let flag: Flag = record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize flag"))?
        .ok_or(wasm_error!("Flag entry is None"))?;

    let links = get_links(
        LinkQuery::try_new(flag.poll_action_hash, LinkTypes::PollToFlags)?,
        GetStrategy::default(),
    )?;
    for link in links {
        if let Ok(target) = ActionHash::try_from(link.target) {
            if target == flag_action_hash {
                delete_link(link.create_link_hash, GetOptions::default())?;
            }
        }
    }

    delete_entry(flag_action_hash)
}

#[hdk_extern]
pub fn get_flag_threshold(_: ()) -> ExternResult<u32> {
    Ok(FLAG_HIDE_THRESHOLD)
}

// ── Encrypted entry functions (v1.3) ──────────────────────────────────

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateEncryptedEntryInput {
    pub cipher: Vec<u8>,
    pub nonce: Vec<u8>,
    /// How to link this entry. "vote_rationale" or "draft_poll".
    /// NOT stored on the DHT - only used for routing.
    pub link_as: String,
    pub related_hash: Option<ActionHash>,
}

/// Store an encrypted entry on the DHT with appropriate links.
#[hdk_extern]
pub fn create_encrypted_entry(input: CreateEncryptedEntryInput) -> ExternResult<ActionHash> {
    let entry = EncryptedEntry {
        cipher: input.cipher,
        nonce: input.nonce,
        entry_type_hint: "private".to_string(),
        related_hash: input.related_hash.clone(),
    };
    let action_hash = create_entry(&EntryZomes::Integrity(EntryTypes::EncryptedEntry(entry)))?;

    match input.link_as.as_str() {
        "vote_rationale" => {
            if let Some(vote_hash) = input.related_hash {
                create_link(vote_hash, action_hash.clone(), LinkTypes::VoteToRationale, ())?;
            }
        }
        "draft_poll" => {
            let agent = agent_info()?.agent_initial_pubkey;
            let anchor = agent_drafts_anchor(&agent)?;
            create_link(anchor, action_hash.clone(), LinkTypes::AgentDrafts, ())?;
        }
        _ => {}
    }

    Ok(action_hash)
}

/// Get the encrypted rationale for a vote (if one exists).
#[hdk_extern]
pub fn get_vote_rationale(vote_action_hash: ActionHash) -> ExternResult<Option<Record>> {
    let links = get_links(
        LinkQuery::try_new(vote_action_hash, LinkTypes::VoteToRationale)?,
        GetStrategy::default(),
    )?;
    if let Some(link) = links.first() {
        let hash = ActionHash::try_from(link.target.clone())
            .map_err(|_| wasm_error!("Invalid rationale link target"))?;
        return get(hash, GetOptions::default());
    }
    Ok(None)
}

/// Get all encrypted draft polls for the current agent.
#[hdk_extern]
pub fn get_my_drafts(_: ()) -> ExternResult<Vec<Record>> {
    let agent = agent_info()?.agent_initial_pubkey;
    let anchor = agent_drafts_anchor(&agent)?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::AgentDrafts)?,
        GetStrategy::default(),
    )?;
    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid draft link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }
    Ok(records)
}

/// Delete an encrypted entry (author-only) and clean up its links.
/// Uses `related_hash` to determine link type:
///   Some(hash) → vote rationale (linked from vote via VoteToRationale)
///   None → draft poll (linked from agent anchor via AgentDrafts)
#[hdk_extern]
pub fn delete_encrypted_entry(action_hash: ActionHash) -> ExternResult<ActionHash> {
    let record = get(action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Encrypted entry not found"))?;
    let my_agent = agent_info()?.agent_initial_pubkey;
    if *record.action().author() != my_agent {
        return Err(wasm_error!("Only the author can delete encrypted entries"));
    }

    let ee: EncryptedEntry = record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize encrypted entry"))?
        .ok_or(wasm_error!("Entry is None"))?;

    if let Some(vote_hash) = ee.related_hash {
        // Vote rationale - clean up VoteToRationale link
        let links = get_links(
            LinkQuery::try_new(vote_hash, LinkTypes::VoteToRationale)?,
            GetStrategy::default(),
        )?;
        for link in links {
            if let Ok(target) = ActionHash::try_from(link.target) {
                if target == action_hash {
                    delete_link(link.create_link_hash, GetOptions::default())?;
                }
            }
        }
    } else {
        // Draft poll - clean up AgentDrafts link
        let anchor = agent_drafts_anchor(&my_agent)?;
        let links = get_links(
            LinkQuery::try_new(anchor, LinkTypes::AgentDrafts)?,
            GetStrategy::default(),
        )?;
        for link in links {
            if let Ok(target) = ActionHash::try_from(link.target) {
                if target == action_hash {
                    delete_link(link.create_link_hash, GetOptions::default())?;
                }
            }
        }
    }

    delete_entry(action_hash)
}

// ── Carrying records from older DNAs ─────────────────────────────────

/// Carry a poll with its original record: the copy, its AllPolls link and
/// the mapping from its old hash. Once per original: a poll already mapped
/// returns that copy.
#[hdk_extern]
pub fn carry_poll(input: CarryInput) -> ExternResult<ActionHash> {
    let c = input.carried;
    if let Some(existing) = get_migration_mapping(c.original_action_hash.clone())? {
        return Ok(existing);
    }
    let f: PollFields = original_fields(&c).map_err(|e| wasm_error!(e))?;
    let old_action_hash = c.original_action_hash.clone();
    let poll = Poll {
        title: f.title,
        description: f.description,
        options: f.options,
        created_at: f.created_at,
        closes_at: f.closes_at,
        poll_type: f.poll_type,
        carried_from: Some(c),
    };
    let poll_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Poll(poll)))?;
    create_link(all_polls_anchor()?, poll_hash.clone(), LinkTypes::AllPolls, ())?;
    let mapping = MigratedPoll {
        old_action_hash,
        new_action_hash: poll_hash.clone(),
        migrated_at: sys_time()?.as_seconds_and_nanos().0,
        via: None,
    };
    let mapping_hash = create_entry(&EntryZomes::Integrity(EntryTypes::MigratedPoll(mapping)))?;
    create_link(migration_anchor()?, mapping_hash, LinkTypes::MigrationIndex, ())?;
    Ok(poll_hash)
}

/// Map a later copy's hash to the poll's one copy here (`copy`, carried from
/// the poll's first record), so votes cast on the later copy land on the
/// same copy as the first record's. Once per later copy: a hash already
/// mapped returns its copy.
#[hdk_extern]
pub fn map_later_copy(input: MapLaterCopyInput) -> ExternResult<ActionHash> {
    let old_action_hash = input.later.poll.original_action_hash.clone();
    if let Some(existing) = get_migration_mapping(old_action_hash.clone())? {
        return Ok(existing);
    }
    let mapping = MigratedPoll {
        old_action_hash,
        new_action_hash: input.copy.clone(),
        migrated_at: sys_time()?.as_seconds_and_nanos().0,
        via: Some(input.later),
    };
    let mapping_hash = create_entry(&EntryZomes::Integrity(EntryTypes::MigratedPoll(mapping)))?;
    create_link(migration_anchor()?, mapping_hash, LinkTypes::MigrationIndex, ())?;
    Ok(input.copy)
}

/// The poll a carried vote or flag is filed under: the new hash of the
/// mapping it names.
fn mapped_poll(c: &Carried) -> ExternResult<ActionHash> {
    let mapping_hash = c.poll_mapping.clone().ok_or(wasm_error!("A carried vote or flag names its poll's mapping"))?;
    let record = get(mapping_hash, GetOptions::default())?.ok_or(wasm_error!("The poll mapping is not found"))?;
    let mapping: MigratedPoll = record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize MigratedPoll"))?
        .ok_or(wasm_error!("The poll mapping is not a MigratedPoll"))?;
    Ok(mapping.new_action_hash)
}

/// The record a link of `link_type` from `poll` already files for `voter`.
fn filed_for(poll: &ActionHash, link_type: LinkTypes, voter: &AgentPubKey) -> ExternResult<Option<ActionHash>> {
    let links = get_links(LinkQuery::try_new(poll.clone(), link_type)?, GetStrategy::default())?;
    Ok(links
        .iter()
        .find(|l| &counted_for(l) == voter)
        .and_then(|l| ActionHash::try_from(l.target.clone()).ok()))
}

/// Carry a vote onto its poll's copy, tagged with its original voter. Once
/// per voter and poll: a vote already there for them (carried, or cast
/// again under the same key) is returned instead.
#[hdk_extern]
pub fn carry_vote(input: CarryInput) -> ExternResult<ActionHash> {
    let c = input.carried;
    let poll = mapped_poll(&c)?;
    if let Some(existing) = filed_for(&poll, LinkTypes::PollToVotes, &c.original_author)? {
        return Ok(existing);
    }
    let f: VoteFields = original_fields(&c).map_err(|e| wasm_error!(e))?;
    let tag = voter_tag(&c);
    let vote = Vote {
        poll_action_hash: poll.clone(),
        option_index: f.option_index,
        display_name: f.display_name,
        profile_picture: f.profile_picture,
        carried_from: Some(c),
    };
    let vote_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Vote(vote)))?;
    create_link(poll, vote_hash.clone(), LinkTypes::PollToVotes, tag)?;
    Ok(vote_hash)
}

/// Carry a flag as a vote is carried.
#[hdk_extern]
pub fn carry_flag(input: CarryInput) -> ExternResult<ActionHash> {
    let c = input.carried;
    let poll = mapped_poll(&c)?;
    if let Some(existing) = filed_for(&poll, LinkTypes::PollToFlags, &c.original_author)? {
        return Ok(existing);
    }
    let f: FlagFields = original_fields(&c).map_err(|e| wasm_error!(e))?;
    let tag = voter_tag(&c);
    let flag = Flag {
        poll_action_hash: poll.clone(),
        reason: f.reason,
        created_at: f.created_at,
        carried_from: Some(c),
    };
    let flag_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Flag(flag)))?;
    create_link(poll, flag_hash.clone(), LinkTypes::PollToFlags, tag)?;
    Ok(flag_hash)
}

/// Carry an agent link, filed under both of its agents. Once per original.
#[hdk_extern]
pub fn carry_link(input: CarryInput) -> ExternResult<ActionHash> {
    let c = input.carried;
    let entry: IsSamePerson = original_fields(&c).map_err(|e| wasm_error!(e))?;
    for (existing, carried) in carried_links_from(&entry.agent_a)? {
        if carried.original_action_hash == c.original_action_hash {
            return Ok(existing);
        }
    }
    let (agent_a, agent_b) = (entry.agent_a, entry.agent_b);
    let link_hash = create_entry(&EntryZomes::Integrity(EntryTypes::CarriedLink(CarriedLink { carried: c })))?;
    create_link(agent_a, link_hash.clone(), LinkTypes::AgentToCarriedLink, ())?;
    create_link(agent_b, link_hash.clone(), LinkTypes::AgentToCarriedLink, ())?;
    Ok(link_hash)
}

/// The carried links filed under `agent`, with their records' hashes.
fn carried_links_from(agent: &AgentPubKey) -> ExternResult<Vec<(ActionHash, Carried)>> {
    let links = get_links(LinkQuery::try_new(agent.clone(), LinkTypes::AgentToCarriedLink)?, GetStrategy::default())?;
    let mut out = Vec::new();
    for link in links {
        let Ok(hash) = ActionHash::try_from(link.target) else { continue };
        if let Some(record) = get(hash.clone(), GetOptions::default())? {
            if let Ok(Some(CarriedLink { carried })) = record.entry().to_app_option::<CarriedLink>() {
                out.push((hash, carried));
            }
        }
    }
    Ok(out)
}

/// The agents `agent` is linked to by carried links.
fn carried_links_of(agent: &AgentPubKey) -> ExternResult<Vec<AgentPubKey>> {
    let mut out = Vec::new();
    for (_, carried) in carried_links_from(agent)? {
        if let Ok(entry) = original_fields::<IsSamePerson>(&carried) {
            let other = if &entry.agent_a == agent { entry.agent_b } else { entry.agent_a };
            out.push(other);
        }
    }
    Ok(out)
}

#[hdk_extern]
pub fn get_carried_links(agent: AgentPubKey) -> ExternResult<Vec<AgentPubKey>> {
    carried_links_of(&agent)
}

/// The mapping record for a poll's old hash: what a carried vote or flag
/// names in `poll_mapping`.
#[hdk_extern]
pub fn get_migration_record(old_action_hash: ActionHash) -> ExternResult<Option<ActionHash>> {
    let links = get_links(LinkQuery::try_new(migration_anchor()?, LinkTypes::MigrationIndex)?, GetStrategy::default())?;
    for link in links {
        let Ok(hash) = ActionHash::try_from(link.target) else { continue };
        if let Some(record) = get(hash.clone(), GetOptions::default())? {
            if let Ok(Some(m)) = record.entry().to_app_option::<MigratedPoll>() {
                if m.old_action_hash == old_action_hash {
                    return Ok(Some(hash));
                }
            }
        }
    }
    Ok(None)
}

#[hdk_extern]
pub fn get_migration_mapping(old_action_hash: ActionHash) -> ExternResult<Option<ActionHash>> {
    let anchor = migration_anchor()?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::MigrationIndex)?,
        GetStrategy::default(),
    )?;

    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid migration link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            let migrated: MigratedPoll = record
                .entry()
                .to_app_option()
                .map_err(|_| wasm_error!("Could not deserialize MigratedPoll"))?
                .ok_or(wasm_error!("MigratedPoll entry is None"))?;

            if migrated.old_action_hash == old_action_hash {
                return Ok(Some(migrated.new_action_hash));
            }
        }
    }

    Ok(None)
}

#[hdk_extern]
pub fn get_all_migration_mappings(_: ()) -> ExternResult<Vec<Record>> {
    let anchor = migration_anchor()?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::MigrationIndex)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid migration link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}
