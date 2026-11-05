//! ProofPoll integrity zome (v1.5).
//!
//! v1.4 plus carried records that the network checks. A record from v1.3
//! (the Holochain 0.6 network) or v1.4 can be written here by someone other
//! than its author, with its original record attached: the exact bytes its
//! author signed, the signature and the entry. A node carrying an old
//! network writes them this way, and so does an app moving its own records.
//! Validation checks the proof with no network, so a forged copy cannot be
//! published:
//!   1. the original action decodes (each line's own shape), names the
//!      carried author and time, and hashes to the carried action hash;
//!   2. the original author's signature verifies over those exact bytes;
//!   3. the original entry hashes to the entry hash the action names, and is
//!      the same kind of entry (zome and entry index);
//!   4. the copy equals the original, except a vote's or flag's poll hash,
//!      which the named mapping maps from the original's.
//! A carried record's own author is only its witness: counting, "you voted"
//! and the one-vote rule go by the proven original author.
//!
//! ## For forking developers
//!
//! The pattern carries any record whose content is the same on both sides:
//! keep `Carried`, `check_carried` and the per-type equality in step 4, and
//! add a mirror for each older line's action shape you carry from.

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
    /// The original record, when this poll was carried from v1.3 or v1.4.
    #[serde(default)]
    pub carried_from: Option<Carried>,
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
    /// The original record, when this vote was carried. Its
    /// `poll_mapping` names the mapping of the poll it was cast on.
    #[serde(default)]
    pub carried_from: Option<Carried>,
}

/// A flag on a poll, indicating community concern.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Flag {
    pub poll_action_hash: ActionHash,
    pub reason: FlagReason,
    pub created_at: i64,
    /// The original record, when this flag was carried (as a vote's).
    #[serde(default)]
    pub carried_from: Option<Carried>,
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

/// A record's origin on the network it came from, with the proof a reader
/// checks it by: the original action exactly as its author signed it, the
/// signature, and the original entry.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Carried {
    /// The DNA version it was written on: "1.3" (Holochain 0.6) or "1.4" (0.7).
    pub original_dna: String,
    pub original_action_hash: ActionHash,
    pub original_author: AgentPubKey,
    pub original_timestamp: Timestamp,
    /// The bytes the author signed (Holochain encodes the action and lair
    /// signs those bytes), so no check re-encodes anything.
    #[serde(with = "serde_bytes")]
    pub original_action: Vec<u8>,
    pub original_signature: Signature,
    #[serde(with = "serde_bytes")]
    pub original_entry: Vec<u8>,
    /// A carried vote or flag: the `MigratedPoll` record that maps the
    /// original's poll to the poll it is filed under here.
    pub poll_mapping: Option<ActionHash>,
}

/// Maps a poll on an older DNA to its copy here. A poll has one copy here,
/// carried from its first record; a mapping is valid when that copy's proof
/// names the old hash, or, for a later copy of the same poll (the v1.4 copy
/// its author made of a v1.3 poll), when `via` proves it. So a mapping
/// cannot point an old poll at anything else, and a poll's votes land on one
/// copy whichever older hash they name.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct MigratedPoll {
    /// The poll's ActionHash on the older DHT.
    pub old_action_hash: ActionHash,
    /// The carried poll's ActionHash here.
    pub new_action_hash: ActionHash,
    /// Unix seconds when it was carried.
    pub migrated_at: i64,
    /// For a later copy's hash: the proof that it is its author's copy of the
    /// first record the copy here carries. None for that first record's hash.
    #[serde(default)]
    pub via: Option<LaterCopy>,
}

/// A later copy of a poll, tied to its first record: the later poll record
/// and the mapping its own DNA holds from the first record's hash to it,
/// both carried with their proofs, both by the first record's author.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct LaterCopy {
    pub poll: Carried,
    pub mapping: Carried,
}

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
    /// Always "private" (enforced by validate_encrypted_entry) - the entry body
    /// never reveals content type. Routing is done by link type, which IS
    /// publicly visible; see the metadata caveat in the README.
    pub entry_type_hint: String,
    /// Optional reference to a related entry (e.g. the Vote this rationale
    /// belongs to). NOTE: stored in plaintext - peers can see the relationship.
    /// Forks needing metadata privacy should encrypt references inside the
    /// payload instead.
    pub related_hash: Option<ActionHash>,
}

/// An agent link (the agent-linking zome's `IsSamePersonEntry`) carried
/// from an older DNA. Only one of a link's two agents may write it, so a
/// carrier writes this instead; it proves itself twice over, by the
/// original record's signature and by the two agents' signatures inside it.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct CarriedLink {
    pub carried: Carried,
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
    CarriedLink(CarriedLink),
}

// ── Link types ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[hdk_link_types]
pub enum LinkTypes {
    /// From a well-known anchor hash to each Poll's action hash.
    AllPolls,
    /// From a Poll's action hash to each Vote's action hash. A carried
    /// vote's link tag is its original voter's key (39 bytes).
    PollToVotes,
    /// From a Poll's action hash to each Flag's action hash; tagged like
    /// votes.
    PollToFlags,
    /// From the migration anchor to each MigratedPoll's action hash.
    MigrationIndex,
    /// From a Vote's action hash to its encrypted rationale.
    VoteToRationale,
    /// From an agent-specific anchor to their encrypted draft polls.
    AgentDrafts,
    /// From each of a carried link's two agents to the CarriedLink.
    AgentToCarriedLink,
}

// ── Anchors ───────────────────────────────────────────────────────────

fn anchor(title: String) -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title,
        description: String::new(),
        options: vec![],
        created_at: 0,
        closes_at: None,
        poll_type: PollType::Anonymous,
        carried_from: None,
    })
}

/// Returns a deterministic hash to use as the base for AllPolls links.
pub fn all_polls_anchor() -> ExternResult<EntryHash> {
    anchor("ALL_POLLS_ANCHOR".to_string())
}

/// Returns a deterministic hash to use as the base for MigrationIndex links.
pub fn migration_anchor() -> ExternResult<EntryHash> {
    anchor("MIGRATION_ANCHOR".to_string())
}

/// Returns a deterministic hash for an agent's encrypted drafts anchor.
pub fn agent_drafts_anchor(agent: &AgentPubKey) -> ExternResult<EntryHash> {
    anchor(format!("AGENT_DRAFTS_{}", agent))
}

// ── The original record ───────────────────────────────────────────────

/// Where an older DNA keeps each entry kind (v1.3 and v1.4 share one
/// layout: agent_linking_integrity is zome 0, polls_integrity zome 1).
pub const POLLS_ZOME: u8 = 1;
pub const POLL_ENTRY: u8 = 0;
pub const VOTE_ENTRY: u8 = 1;
pub const FLAG_ENTRY: u8 = 2;
pub const MIGRATED_POLL_ENTRY: u8 = 3;
pub const AGENT_LINKING_ZOME: u8 = 0;
pub const IS_SAME_PERSON_ENTRY: u8 = 0;

/// What the checks read of an original action, from either line.
pub struct Original {
    pub author: AgentPubKey,
    pub timestamp: Timestamp,
    pub entry_hash: EntryHash,
    pub zome_index: u8,
    pub entry_index: u8,
}

/// A Holochain 0.6 action as far as a check reads it: an enum tagged by
/// `type` (0.7 changed the action shape, so this DNA has no 0.6 `Action`).
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum Action06 {
    Create(Create06),
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct Create06 {
    author: AgentPubKey,
    timestamp: Timestamp,
    entry_type: EntryType,
    entry_hash: EntryHash,
}

/// Decode the original action on its own line's shape.
pub fn original_of(c: &Carried) -> Result<Original, String> {
    let (author, timestamp, entry_type, entry_hash) = match c.original_dna.as_str() {
        "1.3" => match holochain_serialized_bytes::decode::<_, Action06>(&c.original_action) {
            Ok(Action06::Create(a)) => (a.author, a.timestamp, a.entry_type, a.entry_hash),
            Ok(Action06::Other) => return Err("the original action is not a Create".to_string()),
            Err(e) => return Err(format!("the original action does not decode: {e}")),
        },
        "1.4" => {
            let a: Action = holochain_serialized_bytes::decode(&c.original_action)
                .map_err(|e| format!("the original action does not decode: {e}"))?;
            let Some((hash, kind)) = a.entry_data() else {
                return Err("the original action carries no entry".to_string());
            };
            (a.author().clone(), a.timestamp(), kind.clone(), hash.clone())
        }
        other => return Err(format!("records are not carried from DNA {other}")),
    };
    let EntryType::App(def) = entry_type else {
        return Err("the original is not an app entry".to_string());
    };
    Ok(Original { author, timestamp, entry_hash, zome_index: def.zome_index.0, entry_index: def.entry_index.0 })
}

/// Steps 1-3 for any carried record, expecting the original to be entry
/// `entry_index` of zome `zome_index`. `Err` is the reason it is invalid.
pub fn check_carried(c: &Carried, zome_index: u8, entry_index: u8) -> ExternResult<Result<Original, String>> {
    let original = match original_of(c) {
        Ok(o) => o,
        Err(e) => return Ok(Err(e)),
    };
    if original.author != c.original_author || original.timestamp != c.original_timestamp {
        return Ok(Err("the original action names another author or time".to_string()));
    }
    let hashed = ActionHash::from_raw_32(holo_hash::encode::blake2b_256(&c.original_action));
    if hashed != c.original_action_hash {
        return Ok(Err("the original action does not hash to the carried action hash".to_string()));
    }
    if !verify_signature_raw(c.original_author.clone(), c.original_signature.clone(), c.original_action.clone())? {
        return Ok(Err("the original author's signature does not verify".to_string()));
    }
    let entry = Entry::App(AppEntryBytes(SerializedBytes::from(UnsafeBytes::from(c.original_entry.clone()))));
    if hash_entry(entry)? != original.entry_hash {
        return Ok(Err("the original entry does not hash to the entry the action names".to_string()));
    }
    if (original.zome_index, original.entry_index) != (zome_index, entry_index) {
        return Ok(Err("the original is another kind of entry".to_string()));
    }
    Ok(Ok(original))
}

/// The original entries' fields (v1.3 and v1.4 wrote the same shapes, with
/// no `carried_from`).
#[derive(Deserialize, Debug)]
pub struct PollFields {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    pub poll_type: PollType,
}

#[derive(Deserialize, Debug)]
pub struct VoteFields {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    pub display_name: Option<String>,
    pub profile_picture: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FlagFields {
    pub poll_action_hash: ActionHash,
    pub reason: FlagReason,
    pub created_at: i64,
}

/// An older DNA's poll mapping (v1.3 and v1.4; v1.4's extra fields unread).
#[derive(Deserialize, Debug)]
pub struct MigratedPollFields {
    pub old_action_hash: ActionHash,
    pub new_action_hash: ActionHash,
    pub migrated_at: i64,
}

/// The agent-linking zome's entry, as a carried link checks it.
#[derive(Deserialize, Debug)]
pub struct IsSamePerson {
    pub agent_a: AgentPubKey,
    pub signature_a: Signature,
    pub agent_b: AgentPubKey,
    pub signature_b: Signature,
    pub created_at: i64,
}

/// The carried record's original entry, decoded.
pub fn original_fields<T: serde::de::DeserializeOwned + std::fmt::Debug>(c: &Carried) -> Result<T, String> {
    holochain_serialized_bytes::decode(&c.original_entry).map_err(|e| format!("the original entry does not decode: {e}"))
}

// ── Validation ────────────────────────────────────────────────────────

fn invalid(reason: impl Into<String>) -> ExternResult<ValidateCallbackResult> {
    Ok(ValidateCallbackResult::Invalid(reason.into()))
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::CreateEntry(op_entry) => match op_entry {
            OpEntry::CreateEntry { app_entry, .. } | OpEntry::UpdateEntry { app_entry, .. } => {
                match app_entry {
                    EntryTypes::Poll(poll) => validate_poll(&poll),
                    EntryTypes::Vote(vote) => validate_vote(&vote),
                    EntryTypes::Flag(flag) => validate_flag(&flag),
                    EntryTypes::MigratedPoll(mp) => validate_migrated_poll(&mp),
                    EntryTypes::EncryptedEntry(ee) => validate_encrypted_entry(&ee),
                    EntryTypes::CarriedLink(link) => validate_carried_link(&link),
                }
            }
            _ => Ok(ValidateCallbackResult::Valid),
        },
        FlatOp::Link(OpLink::CreateLink { link_type, action }) => match link_type {
            LinkTypes::AllPolls => {
                if action.data.base_address != AnyLinkableHash::from(all_polls_anchor()?) {
                    return invalid("AllPolls link must originate from the polls anchor");
                }
                Ok(ValidateCallbackResult::Valid)
            }
            LinkTypes::PollToVotes | LinkTypes::PollToFlags => validate_voter_tag(&action.data.target_address, &action.data.tag),
            LinkTypes::VoteToRationale => Ok(ValidateCallbackResult::Valid),
            LinkTypes::AgentDrafts => Ok(ValidateCallbackResult::Valid),
            LinkTypes::MigrationIndex => {
                if action.data.base_address != AnyLinkableHash::from(migration_anchor()?) {
                    return invalid("MigrationIndex link must originate from the migration anchor");
                }
                Ok(ValidateCallbackResult::Valid)
            }
            LinkTypes::AgentToCarriedLink => validate_carried_link_lookup(&action.data.base_address, &action.data.target_address),
        },
        FlatOp::Link(OpLink::DeleteLink { .. }) => Ok(ValidateCallbackResult::Valid),
        _ => Ok(ValidateCallbackResult::Valid),
    }
}

fn validate_poll(poll: &Poll) -> ExternResult<ValidateCallbackResult> {
    if poll.title.trim().is_empty() {
        return invalid("Poll title cannot be empty");
    }
    if poll.options.len() < 2 {
        return invalid("Poll must have at least 2 options");
    }
    if poll.options.len() > 10 {
        return invalid("Poll cannot have more than 10 options");
    }
    if poll.options.iter().any(|o| o.trim().is_empty()) {
        return invalid("Poll options cannot be empty");
    }
    let Some(c) = &poll.carried_from else { return Ok(ValidateCallbackResult::Valid) };
    if c.poll_mapping.is_some() {
        return invalid("A carried poll names no poll mapping");
    }
    if let Err(e) = check_carried(c, POLLS_ZOME, POLL_ENTRY)? {
        return invalid(e);
    }
    let original: PollFields = match original_fields(c) {
        Ok(f) => f,
        Err(e) => return invalid(e),
    };
    let same = original.title == poll.title
        && original.description == poll.description
        && original.options == poll.options
        && original.created_at == poll.created_at
        && original.closes_at == poll.closes_at
        && original.poll_type == poll.poll_type;
    if !same {
        return invalid("A carried poll must equal its original");
    }
    Ok(ValidateCallbackResult::Valid)
}

/// The mapping a carried vote or flag names: its old poll hash must be the
/// original's poll and its new one the poll the copy is filed under.
fn check_mapping(c: &Carried, original_poll: &ActionHash, copy_poll: &ActionHash) -> ExternResult<Result<(), String>> {
    let Some(mapping_hash) = &c.poll_mapping else {
        return Ok(Err("a carried vote or flag names its poll's mapping".to_string()));
    };
    let record = must_get_valid_record(mapping_hash.clone())?;
    let Ok(Some(mapping)) = record.entry().to_app_option::<MigratedPoll>() else {
        return Ok(Err("the named poll mapping is not a MigratedPoll".to_string()));
    };
    if &mapping.old_action_hash != original_poll || &mapping.new_action_hash != copy_poll {
        return Ok(Err("the named poll mapping does not map the original's poll to this one".to_string()));
    }
    Ok(Ok(()))
}

fn validate_vote(vote: &Vote) -> ExternResult<ValidateCallbackResult> {
    let Some(c) = &vote.carried_from else { return Ok(ValidateCallbackResult::Valid) };
    if let Err(e) = check_carried(c, POLLS_ZOME, VOTE_ENTRY)? {
        return invalid(e);
    }
    let original: VoteFields = match original_fields(c) {
        Ok(f) => f,
        Err(e) => return invalid(e),
    };
    if original.option_index != vote.option_index
        || original.display_name != vote.display_name
        || original.profile_picture != vote.profile_picture
    {
        return invalid("A carried vote must equal its original");
    }
    if let Err(e) = check_mapping(c, &original.poll_action_hash, &vote.poll_action_hash)? {
        return invalid(e);
    }
    Ok(ValidateCallbackResult::Valid)
}

fn validate_flag(flag: &Flag) -> ExternResult<ValidateCallbackResult> {
    let Some(c) = &flag.carried_from else { return Ok(ValidateCallbackResult::Valid) };
    if let Err(e) = check_carried(c, POLLS_ZOME, FLAG_ENTRY)? {
        return invalid(e);
    }
    let original: FlagFields = match original_fields(c) {
        Ok(f) => f,
        Err(e) => return invalid(e),
    };
    if original.reason != flag.reason || original.created_at != flag.created_at {
        return invalid("A carried flag must equal its original");
    }
    if let Err(e) = check_mapping(c, &original.poll_action_hash, &flag.poll_action_hash)? {
        return invalid(e);
    }
    Ok(ValidateCallbackResult::Valid)
}

fn validate_migrated_poll(mp: &MigratedPoll) -> ExternResult<ValidateCallbackResult> {
    if mp.old_action_hash == mp.new_action_hash {
        return invalid("MigratedPoll must map two different action hashes");
    }
    let record = must_get_valid_record(mp.new_action_hash.clone())?;
    let carried = record
        .entry()
        .to_app_option::<Poll>()
        .ok()
        .flatten()
        .and_then(|p| p.carried_from);
    let Some(first) = carried else {
        return invalid("A mapping names a poll carried from the old one");
    };
    let Some(later) = &mp.via else {
        return if first.original_action_hash == mp.old_action_hash {
            Ok(ValidateCallbackResult::Valid)
        } else {
            invalid("A mapping names a poll carried from the old one")
        };
    };
    match check_later_copy(&first, later, &mp.old_action_hash)? {
        Ok(()) => Ok(ValidateCallbackResult::Valid),
        Err(e) => invalid(e),
    }
}

/// A later copy (`old_action_hash`, on a later DNA) of the first record the
/// copy here carries: both proofs verify, the carried mapping maps the first
/// record's hash to the later poll, and all three are by one author.
fn check_later_copy(first: &Carried, later: &LaterCopy, old_action_hash: &ActionHash) -> ExternResult<Result<(), String>> {
    if later.poll.poll_mapping.is_some() || later.mapping.poll_mapping.is_some() {
        return Ok(Err("a later copy's records name no poll mapping".to_string()));
    }
    if later.poll.original_action_hash != *old_action_hash {
        return Ok(Err("the later copy is not the poll this mapping maps".to_string()));
    }
    if later.poll.original_dna != later.mapping.original_dna {
        return Ok(Err("the later copy and its mapping are on different DNAs".to_string()));
    }
    if !(first.original_dna == "1.3" && later.poll.original_dna == "1.4") {
        return Ok(Err("a later copy is on a later DNA than the first record".to_string()));
    }
    if later.poll.original_author != first.original_author || later.mapping.original_author != first.original_author {
        return Ok(Err("a later copy and its mapping are by the first record's author".to_string()));
    }
    if let Err(e) = check_carried(&later.poll, POLLS_ZOME, POLL_ENTRY)? {
        return Ok(Err(format!("the later copy: {e}")));
    }
    if let Err(e) = check_carried(&later.mapping, POLLS_ZOME, MIGRATED_POLL_ENTRY)? {
        return Ok(Err(format!("the later copy's mapping: {e}")));
    }
    let m: MigratedPollFields = match original_fields(&later.mapping) {
        Ok(m) => m,
        Err(e) => return Ok(Err(e)),
    };
    if m.old_action_hash != first.original_action_hash || m.new_action_hash != *old_action_hash {
        return Ok(Err("the later copy's mapping maps the first record to the later copy".to_string()));
    }
    Ok(Ok(()))
}

/// A vote or flag link tagged with a voter (a carried one): the tag must be
/// the original author its target proves. Untagged links file native ones.
fn validate_voter_tag(target: &AnyLinkableHash, tag: &LinkTag) -> ExternResult<ValidateCallbackResult> {
    if tag.0.is_empty() {
        return Ok(ValidateCallbackResult::Valid);
    }
    let Ok(voter) = AgentPubKey::try_from_raw_39(tag.0.clone()) else {
        return invalid("A vote or flag link's tag is a voter's key");
    };
    let Some(target) = target.clone().into_action_hash() else {
        return invalid("A tagged vote or flag link points at a record");
    };
    let record = must_get_valid_record(target)?;
    let carried = match record.entry().to_app_option::<Vote>() {
        Ok(Some(v)) => v.carried_from,
        _ => record.entry().to_app_option::<Flag>().ok().flatten().and_then(|f| f.carried_from),
    };
    match carried {
        Some(c) if c.original_author == voter => Ok(ValidateCallbackResult::Valid),
        _ => invalid("A link's voter tag names the voter its carried record proves"),
    }
}

fn validate_carried_link(link: &CarriedLink) -> ExternResult<ValidateCallbackResult> {
    let c = &link.carried;
    if c.poll_mapping.is_some() {
        return invalid("A carried link names no poll mapping");
    }
    if let Err(e) = check_carried(c, AGENT_LINKING_ZOME, IS_SAME_PERSON_ENTRY)? {
        return invalid(e);
    }
    let entry: IsSamePerson = match original_fields(c) {
        Ok(f) => f,
        Err(e) => return invalid(e),
    };
    // The agent-linking zome's own rules, as the original network checked them.
    if entry.agent_a >= entry.agent_b {
        return invalid("A carried link's agents are two keys in order");
    }
    if c.original_author != entry.agent_a && c.original_author != entry.agent_b {
        return invalid("A carried link was written by one of its two agents");
    }
    let mut payload = Vec::with_capacity(78);
    payload.extend_from_slice(entry.agent_a.get_raw_39());
    payload.extend_from_slice(entry.agent_b.get_raw_39());
    if !verify_signature_raw(entry.agent_a.clone(), entry.signature_a.clone(), payload.clone())?
        || !verify_signature_raw(entry.agent_b.clone(), entry.signature_b.clone(), payload)?
    {
        return invalid("A carried link's two signatures verify");
    }
    Ok(ValidateCallbackResult::Valid)
}

/// A lookup link to a carried link starts from one of its two agents.
fn validate_carried_link_lookup(base: &AnyLinkableHash, target: &AnyLinkableHash) -> ExternResult<ValidateCallbackResult> {
    let Some(target) = target.clone().into_action_hash() else {
        return invalid("A carried link lookup points at a record");
    };
    let record = must_get_valid_record(target)?;
    let Ok(Some(link)) = record.entry().to_app_option::<CarriedLink>() else {
        return invalid("A carried link lookup points at a CarriedLink");
    };
    let entry: IsSamePerson = match original_fields(&link.carried) {
        Ok(f) => f,
        Err(e) => return invalid(e),
    };
    let from_one = [entry.agent_a, entry.agent_b].iter().any(|a| AnyLinkableHash::from(a.clone()) == *base);
    if !from_one {
        return invalid("A carried link lookup starts from one of its two agents");
    }
    Ok(ValidateCallbackResult::Valid)
}

fn validate_encrypted_entry(ee: &EncryptedEntry) -> ExternResult<ValidateCallbackResult> {
    if ee.cipher.is_empty() {
        return invalid("Cipher cannot be empty");
    }
    if ee.nonce.len() != 24 {
        return invalid("Nonce must be 24 bytes");
    }
    if ee.entry_type_hint != "private" {
        return invalid("entry_type_hint must be 'private'");
    }
    Ok(ValidateCallbackResult::Valid)
}
