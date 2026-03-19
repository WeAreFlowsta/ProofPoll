//! ProofPoll integrity zome (v1.2).
//!
//! Extends v1.1 with public poll support:
//!   - `PollType` enum: `Anonymous` (default) or `Public`
//!   - `poll_type` field on `Poll`
//!   - `display_name` and `profile_picture` fields on `Vote`
//!     (populated from the voter's Flowsta profile on public polls)
//!
//! ## For forking developers
//!
//! When creating your own v1.3:
//!   1. Copy this directory to `dna/v1.3/`
//!   2. Add your new entry types to the `EntryTypes` enum
//!   3. Keep `MigratedPoll` and `MigrationIndex` — they power the migration system
//!   4. Update `network_seed` in `dna.yaml` to create a new DHT

use hdi::prelude::*;

// ── Entry types ────────────────────────────────────────────────────────

/// Whether a poll shows voter identities in results.
///
/// - `Anonymous`: votes show counts only. Voter agent keys are on the DHT
///   but the UI does not display them.
/// - `Public`: voters' display names and profile pictures are included in
///   their Vote entry and shown alongside results. Voters are shown a
///   consent notice before casting their vote.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum PollType {
    Anonymous,
    Public,
}

/// A poll with a question and multiple options.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    /// Whether this poll shows voter identities. Locked at creation time.
    pub poll_type: PollType,
}

/// A vote on a specific poll option.
///
/// On `Public` polls, `display_name` and `profile_picture` are populated
/// from the voter's Flowsta profile at vote time and stored permanently
/// on the DHT. The voter explicitly consents to this before voting.
///
/// On `Anonymous` polls both fields are `None`.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Voter's display name (public polls only, voluntarily disclosed).
    pub display_name: Option<String>,
    /// Voter's profile picture URL or base64 data URI (public polls only).
    pub profile_picture: Option<String>,
}

// ── v1.1 entry types (carried forward unchanged) ──────────────────────

/// A flag on a poll, indicating community concern.
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

/// Records a poll that was migrated from the previous DNA version.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct MigratedPoll {
    /// The poll's ActionHash on the previous DHT.
    pub old_action_hash: ActionHash,
    /// The poll's ActionHash on this DHT (after re-creation).
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
        poll_type: PollType::Anonymous,
    })
}

/// Returns a deterministic hash to use as the base for MigrationIndex links.
pub fn migration_anchor() -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title: "MIGRATION_ANCHOR".to_string(),
        description: String::new(),
        options: vec![],
        created_at: 0,
        closes_at: None,
        poll_type: PollType::Anonymous,
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
                    EntryTypes::MigratedPoll(_) => Ok(ValidateCallbackResult::Valid),
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
    let _ = vote;
    Ok(ValidateCallbackResult::Valid)
}

fn validate_flag(flag: &Flag) -> ExternResult<ValidateCallbackResult> {
    let _ = flag;
    Ok(ValidateCallbackResult::Valid)
}
