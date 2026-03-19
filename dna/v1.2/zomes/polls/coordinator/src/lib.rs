//! ProofPoll coordinator zome (v1.2).
//!
//! Extends v1.1 with:
//!   - `poll_type` field on `CreatePollInput` (Anonymous | Public)
//!   - `display_name` and `profile_picture` on `CastVoteInput`
//!     (populated from the voter's local profile cache for public polls)
//!
//! Everything else (flagging, migration) is carried forward unchanged.

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
    /// Voter's display name — required for public polls, None for anonymous.
    pub display_name: Option<String>,
    /// Voter's profile picture — required for public polls, None for anonymous.
    pub profile_picture: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FlagPollInput {
    pub poll_action_hash: ActionHash,
    pub reason: FlagReason,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RegisterMigratedPollInput {
    pub old_action_hash: ActionHash,
    pub new_action_hash: ActionHash,
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

    // Check for double-vote
    let my_agent = agent_info()?.agent_initial_pubkey;
    let existing_links = get_links(
        LinkQuery::try_new(input.poll_action_hash.clone(), LinkTypes::PollToVotes)?,
        GetStrategy::default(),
    )?;
    for link in &existing_links {
        if link.author == my_agent {
            return Err(wasm_error!("You have already voted on this poll"));
        }
    }

    let vote = Vote {
        poll_action_hash: input.poll_action_hash.clone(),
        option_index: input.option_index,
        display_name: input.display_name,
        profile_picture: input.profile_picture,
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
        if link.author == my_agent {
            return Err(wasm_error!("You have already flagged this poll"));
        }
    }

    let now = sys_time()?.as_seconds_and_nanos().0;
    let flag = Flag {
        poll_action_hash: input.poll_action_hash.clone(),
        reason: input.reason,
        created_at: now,
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

// ── Migration functions (v1.1 → v1.2) ────────────────────────────────

#[hdk_extern]
pub fn register_migrated_poll(input: RegisterMigratedPollInput) -> ExternResult<ActionHash> {
    if input.old_action_hash == input.new_action_hash {
        return Err(wasm_error!("Old and new action hashes must be different"));
    }

    let now = sys_time()?.as_seconds_and_nanos().0;
    let migrated = MigratedPoll {
        old_action_hash: input.old_action_hash,
        new_action_hash: input.new_action_hash,
        migrated_at: now,
    };

    let action_hash = create_entry(&EntryZomes::Integrity(EntryTypes::MigratedPoll(migrated)))?;

    let anchor = migration_anchor()?;
    create_link(anchor, action_hash.clone(), LinkTypes::MigrationIndex, ())?;

    Ok(action_hash)
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
