//! ProofPoll coordinator zome (v1.1).
//!
//! Extends v1.0 with:
//!   - Community flagging (`flag_poll`, `get_poll_flags`, `remove_flag`, `get_flag_threshold`)
//!   - Migration helpers (`register_migrated_poll`, `get_migration_mapping`, `get_all_migration_mappings`)
//!
//! ## For forking developers
//!
//! The migration functions at the bottom are reusable infrastructure.
//! When you create a v1.2, keep them — just rename "poll" to your content type.
//! The flagging functions follow the same pattern as voting (one-per-agent prevention).
//! `FLAG_HIDE_THRESHOLD` is the configurable constant for your community size.

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
/// Smaller communities may want 2; larger ones may want 5–10.
pub const FLAG_HIDE_THRESHOLD: u32 = 3;

// ── Input types ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
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

// ── Poll functions (unchanged from v1.0) ──────────────────────────────

#[hdk_extern]
pub fn create_poll(input: CreatePollInput) -> ExternResult<ActionHash> {
    let now = sys_time()?.as_seconds_and_nanos().0;

    let poll = Poll {
        title: input.title,
        description: input.description,
        options: input.options,
        created_at: now,
        closes_at: input.closes_at,
    };

    let action_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Poll(poll)))?;

    // Link from the AllPolls anchor to this poll
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
    // Verify the caller is the author
    let record = get(action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;
    let my_agent = agent_info()?.agent_initial_pubkey;
    if *record.action().author() != my_agent {
        return Err(wasm_error!("Only the poll creator can delete it"));
    }

    // Delete the AllPolls link pointing to this poll
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

    // Delete the poll entry
    delete_entry(action_hash)
}

// ── Vote functions (unchanged from v1.0) ──────────────────────────────

#[hdk_extern]
pub fn cast_vote(input: CastVoteInput) -> ExternResult<ActionHash> {
    // Fetch the poll to validate option_index
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

    // Check if poll is closed
    if let Some(closes_at) = poll.closes_at {
        let now = sys_time()?.as_seconds_and_nanos().0;
        if now > closes_at {
            return Err(wasm_error!("Poll is closed"));
        }
    }

    // Check for double-vote: look at existing PollToVotes links
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

    // Create the vote entry
    let vote = Vote {
        poll_action_hash: input.poll_action_hash.clone(),
        option_index: input.option_index,
    };

    let vote_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Vote(vote)))?;

    // Link from poll to vote
    create_link(
        input.poll_action_hash,
        vote_hash.clone(),
        LinkTypes::PollToVotes,
        (),
    )?;

    Ok(vote_hash)
}

/// Returns all vote records for a given poll.
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

// ── Flag functions (v1.1) ─────────────────────────────────────────────

/// Flag a poll for community moderation.
///
/// One flag per agent per poll, enforced via PollToFlags link author check
/// (same pattern as double-vote prevention in cast_vote).
///
/// The flag is stored on the DHT permanently — censorship resistance is
/// preserved. The UI decides whether to hide flagged polls based on the
/// configurable threshold (see `get_flag_threshold`).
#[hdk_extern]
pub fn flag_poll(input: FlagPollInput) -> ExternResult<ActionHash> {
    // Verify the poll exists
    let _poll_record = get(input.poll_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;

    // Check for double-flag: one flag per agent per poll
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

    // Link from poll to flag
    create_link(
        input.poll_action_hash,
        flag_hash.clone(),
        LinkTypes::PollToFlags,
        (),
    )?;

    Ok(flag_hash)
}

/// Returns all flag records for a given poll.
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

/// Remove your own flag from a poll.
///
/// Only the flag's author can remove it. Deletes both the entry and the
/// PollToFlags link.
#[hdk_extern]
pub fn remove_flag(flag_action_hash: ActionHash) -> ExternResult<ActionHash> {
    let record = get(flag_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Flag not found"))?;

    let my_agent = agent_info()?.agent_initial_pubkey;
    if *record.action().author() != my_agent {
        return Err(wasm_error!("Only the flag author can remove it"));
    }

    // Deserialize to get the poll hash for link cleanup
    let flag: Flag = record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize flag"))?
        .ok_or(wasm_error!("Flag entry is None"))?;

    // Delete the PollToFlags link
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

    // Delete the flag entry
    delete_entry(flag_action_hash)
}

/// Returns the flag threshold — polls with this many or more unique
/// flaggers are hidden by default in the UI.
///
/// Forking developers: change `FLAG_HIDE_THRESHOLD` to suit your community.
#[hdk_extern]
pub fn get_flag_threshold(_: ()) -> ExternResult<u32> {
    Ok(FLAG_HIDE_THRESHOLD)
}

// ── Migration functions (v1.1) ────────────────────────────────────────
//
// These functions support the v1.0 → v1.1 migration. When a user upgrades,
// their app re-creates their authored polls on the v1.1 DHT and publishes
// a MigratedPoll entry so other users can discover the old→new hash mapping
// and re-cast their votes.
//
// For developers forking this app: if you create a v1.2, copy these
// functions and update the anchor / entry types accordingly. The pattern
// is the same regardless of version numbers.

/// Register a poll that was migrated from v1.0 to v1.1.
///
/// Creates a MigratedPoll entry and links it from the migration anchor
/// so other nodes can discover it via `get_all_migration_mappings`.
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

    // Link from the migration anchor to this mapping entry
    let anchor = migration_anchor()?;
    create_link(anchor, action_hash.clone(), LinkTypes::MigrationIndex, ())?;

    Ok(action_hash)
}

/// Look up the v1.1 hash for a poll that was migrated from v1.0.
///
/// Returns `None` if no migration mapping exists yet (the poll author
/// hasn't upgraded). The caller should retry later.
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

/// Returns all migration mappings on this DHT.
///
/// Used by the migration background task to discover polls that have
/// been migrated by other users, so it can re-cast pending votes.
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
