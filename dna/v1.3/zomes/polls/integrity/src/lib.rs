//! ProofPoll integrity zome (v1.3).
//!
//! Extends v1.2 with encrypted public entries:
//!   - `EncryptedEntry` type for private data stored on the public DHT
//!   - `VoteToRationale` link: Vote → encrypted rationale
//!   - `AgentDrafts` link: agent anchor → encrypted draft poll
//!
//! Encrypted entries are opaque ciphertext on the DHT. Only the author
//! can decrypt them using their lair-managed keys (xsalsa20poly1305).
//!
//! ## For forking developers
//!
//! When creating your own v1.4:
//!   1. Copy this directory to `dna/v1.4/`
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

// ── v1.3 entry types ────────────────────────────────────────────────

/// An encrypted blob stored on the public DHT.
///
/// Only the author can decrypt the contents using their lair-managed
/// x25519 key (derived from their Ed25519 signing key). The DHT
/// replicates the ciphertext for backup, but peers cannot read it.
///
/// Used for: vote rationales, draft polls, and any future private data.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct EncryptedEntry {
    /// Ciphertext (xsalsa20poly1305 via lair crypto_box).
    pub cipher: Vec<u8>,
    /// 24-byte nonce used in encryption.
    pub nonce: Vec<u8>,
    /// Routing hint so the client knows how to deserialize after decryption.
    /// Values: "vote_rationale", "draft_poll"
    pub entry_type_hint: String,
    /// Optional reference to a related entry (e.g. the Vote this rationale belongs to).
    pub related_hash: Option<ActionHash>,
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
    EncryptedEntry(EncryptedEntry),
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
    /// From a Vote's action hash to its encrypted rationale.
    VoteToRationale,
    /// From an agent-specific anchor to their encrypted draft polls.
    AgentDrafts,
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

/// Returns a deterministic hash for an agent's encrypted drafts anchor.
pub fn agent_drafts_anchor(agent: &AgentPubKey) -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title: format!("AGENT_DRAFTS_{}", agent),
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
                    EntryTypes::EncryptedEntry(ee) => validate_encrypted_entry(&ee),
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
            LinkTypes::VoteToRationale => Ok(ValidateCallbackResult::Valid),
            LinkTypes::AgentDrafts => Ok(ValidateCallbackResult::Valid),
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

fn validate_encrypted_entry(ee: &EncryptedEntry) -> ExternResult<ValidateCallbackResult> {
    if ee.cipher.is_empty() {
        return Ok(ValidateCallbackResult::Invalid(
            "Cipher cannot be empty".to_string(),
        ));
    }
    if ee.nonce.len() != 24 {
        return Ok(ValidateCallbackResult::Invalid(
            "Nonce must be 24 bytes".to_string(),
        ));
    }
    if ee.entry_type_hint != "private" {
        return Ok(ValidateCallbackResult::Invalid(
            "entry_type_hint must be 'private'".to_string(),
        ));
    }
    Ok(ValidateCallbackResult::Valid)
}
