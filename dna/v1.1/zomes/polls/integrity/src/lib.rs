//! ProofPoll integrity zome (v1.1).
//!
//! Extends v1.0 with community flagging and migration support.
//! New entry types: `Flag`, `MigratedPoll`. New link types: `PollToFlags`, `MigrationIndex`.
//!
//! ## For forking developers
//!
//! When creating your own v1.2:
//!   1. Copy this entire directory to `dna/v1.2/`
//!   2. Add your new entry types to the `EntryTypes` enum
//!   3. Add new link types to the `LinkTypes` enum
//!   4. Keep `MigratedPoll` and `MigrationIndex` — they power the migration system
//!   5. Update `network_seed` in `dna.yaml` to create a new DHT

use hdi::prelude::*;

// ── Entry types (v1.0 originals) ──────────────────────────────────────

/// A poll with a question and multiple options.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
}

/// A vote on a specific poll option.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
}

// ── Entry types (v1.1 additions) ──────────────────────────────────────

/// A flag on a poll, indicating community concern.
///
/// Flags are stored on the DHT — the data is never deleted (censorship
/// resistance). The UI hides polls that exceed a configurable flag
/// threshold, but any user can toggle "show flagged content" to see them.
///
/// One flag per agent per poll, enforced in the coordinator.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Flag {
    pub poll_action_hash: ActionHash,
    pub reason: FlagReason,
    pub created_at: i64,
}

/// Why a poll was flagged.
///
/// Forking developers: add or rename variants to suit your community.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum FlagReason {
    Spam,
    Misleading,
    OffTopic,
    Inappropriate,
}

/// Records a poll that was migrated from v1.0 to v1.1.
///
/// This entry is published on the v1.1 DHT so that other users can
/// discover the old-to-new hash mapping and re-cast their votes.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct MigratedPoll {
    /// The poll's ActionHash on the v1.0 DHT.
    pub old_action_hash: ActionHash,
    /// The poll's ActionHash on the v1.1 DHT (after re-creation).
    pub new_action_hash: ActionHash,
    /// Unix timestamp when the migration happened.
    pub migrated_at: i64,
}

// ── Entry type enum ───────────────────────────────────────────────────

#[hdk_entry_types]
#[unit_enum(UnitEntryTypes)]
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EntryTypes {
    Poll(Poll),
    Vote(Vote),
    Flag(Flag),
    MigratedPoll(MigratedPoll),
}

// ── Link types ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[hdk_link_types]
pub enum LinkTypes {
    /// From a well-known anchor hash to each Poll's action hash.
    AllPolls,
    /// From a Poll's action hash to each Vote's action hash.
    PollToVotes,
    /// From a Poll's action hash to each Flag's action hash.
    PollToFlags,
    /// From the migration anchor to each MigratedPoll's action hash.
    MigrationIndex,
}

// ── Anchors ───────────────────────────────────────────────────────────

/// Returns a deterministic hash to use as the base for AllPolls links.
pub fn all_polls_anchor() -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title: "ALL_POLLS_ANCHOR".to_string(),
        description: String::new(),
        options: vec![],
        created_at: 0,
        closes_at: None,
    })
}

/// Returns a deterministic hash to use as the base for MigrationIndex links.
///
/// Any node on the v1.1 DHT can discover all migration mappings by
/// calling `get_links` on this anchor with `LinkTypes::MigrationIndex`.
pub fn migration_anchor() -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title: "MIGRATION_ANCHOR".to_string(),
        description: String::new(),
        options: vec![],
        created_at: 0,
        closes_at: None,
    })
}

// ── Validation ────────────────────────────────────────────────────────

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::StoreEntry(store_entry) => match store_entry {
            OpEntry::CreateEntry { app_entry, .. } | OpEntry::UpdateEntry { app_entry, .. } => {
                match app_entry {
                    EntryTypes::Poll(poll) => validate_poll(&poll),
                    EntryTypes::Vote(vote) => validate_vote(&vote),
                    EntryTypes::Flag(flag) => validate_flag(&flag),
                    EntryTypes::MigratedPoll(_) => {
                        // MigratedPoll entries are validated by structure alone.
                        // The coordinator ensures old_action_hash != new_action_hash.
                        Ok(ValidateCallbackResult::Valid)
                    }
                }
            }
            _ => Ok(ValidateCallbackResult::Valid),
        },
        FlatOp::RegisterCreateLink {
            link_type,
            base_address,
            target_address: _,
            tag: _,
            action: _,
        } => match link_type {
            LinkTypes::AllPolls => {
                let anchor = all_polls_anchor()?;
                if base_address != AnyLinkableHash::from(anchor) {
                    return Ok(ValidateCallbackResult::Invalid(
                        "AllPolls link must originate from the polls anchor".to_string(),
                    ));
                }
                Ok(ValidateCallbackResult::Valid)
            }
            LinkTypes::PollToVotes => Ok(ValidateCallbackResult::Valid),
            LinkTypes::PollToFlags => Ok(ValidateCallbackResult::Valid),
            LinkTypes::MigrationIndex => {
                let anchor = migration_anchor()?;
                if base_address != AnyLinkableHash::from(anchor) {
                    return Ok(ValidateCallbackResult::Invalid(
                        "MigrationIndex link must originate from the migration anchor".to_string(),
                    ));
                }
                Ok(ValidateCallbackResult::Valid)
            }
        },
        FlatOp::RegisterDeleteLink { .. } => Ok(ValidateCallbackResult::Valid),
        _ => Ok(ValidateCallbackResult::Valid),
    }
}

fn validate_poll(poll: &Poll) -> ExternResult<ValidateCallbackResult> {
    if poll.title.trim().is_empty() {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll title cannot be empty".to_string(),
        ));
    }
    if poll.options.len() < 2 {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll must have at least 2 options".to_string(),
        ));
    }
    if poll.options.len() > 10 {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll cannot have more than 10 options".to_string(),
        ));
    }
    for opt in &poll.options {
        if opt.trim().is_empty() {
            return Ok(ValidateCallbackResult::Invalid(
                "Poll options cannot be empty".to_string(),
            ));
        }
    }
    Ok(ValidateCallbackResult::Valid)
}

fn validate_vote(vote: &Vote) -> ExternResult<ValidateCallbackResult> {
    // We can't validate option_index against the poll here because we'd need
    // to fetch the poll entry, which isn't available during pure validation.
    // The coordinator enforces this check at call time.
    let _ = vote;
    Ok(ValidateCallbackResult::Valid)
}

fn validate_flag(flag: &Flag) -> ExternResult<ValidateCallbackResult> {
    // Same as votes: we can't fetch the poll during pure validation.
    // The coordinator checks that the poll exists and enforces one-flag-per-agent.
    let _ = flag;
    Ok(ValidateCallbackResult::Valid)
}
