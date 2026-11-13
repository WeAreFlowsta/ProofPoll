//! The entry types of the polls zome and what the page receives of them.
//! Mirror your own zome's structs here.

#![allow(unused_imports)]
use crate::commands::*;
use crate::polls::*;
use holochain_types::prelude::{ActionHash, AgentPubKey};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::sync::Mutex;

// ── Entry types matching the zome definitions ─────────────────────────
//
// These structs must match the entry types in your coordinator zome.
// Holochain entries are MessagePack-encoded, so Serialize + Deserialize
// are required. The frontend never sees these directly — they're decoded
// in the Tauri commands and returned as response types (below).

/// Used to decode MigratedPoll entries when merging v1.0 and v1.1 poll lists.
/// Mirrors the MigratedPoll entry type in the v1.1 integrity zome.
#[derive(serde::Deserialize)]
pub(crate) struct MigratedPollEntry {
    pub(crate) old_action_hash: ActionHash,
    pub(crate) new_action_hash: ActionHash,
    #[allow(dead_code)]
    pub(crate) migrated_at: i64,
}

/// What the app reads of a carried record's proof (v1.5's `Carried`): who
/// wrote the original and its hash. Never sent to the frontend.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct Origin {
    pub original_action_hash: ActionHash,
    pub original_author: AgentPubKey,
}

/// Two polls that ask the same question: the same title, description,
/// options and closing time (a copy made by the 0.5 move keeps all four).
pub(crate) fn same_question(a: &Poll, b: &Poll) -> bool {
    a.title == b.title && a.description == b.description && a.options == b.options && a.closes_at == b.closes_at
}

/// The author a record counts for: the original author its proof names
/// when it was carried, else its writer.
pub(crate) fn counted_author(record: &crate::hc::RecordView, origin: &Option<Origin>) -> String {
    origin.as_ref().map(|o| o.original_author.to_string()).unwrap_or_else(|| record.author.clone())
}

/// Mirrors PollType in the v1.2 integrity zome.
///
/// Must use the same serde representation so `decode_entry` works: rmp_serde
/// encodes unit enum variants as externally-tagged maps `{"Public": nil}`, not
/// as strings. Using a matching enum here round-trips correctly. Tauri then
/// re-serialises to JSON (human-readable) which produces the string `"Public"`
/// that the frontend already expects.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub enum PollType {
    Anonymous,
    Public,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    /// None for v1.0/v1.1 polls (no poll_type field — treated as Anonymous).
    #[serde(default)]
    pub poll_type: Option<PollType>,
    /// v1.5: where a carried poll came from.
    #[serde(default, skip_serializing)]
    pub carried_from: Option<Origin>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Only present on v1.2 public poll votes.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Only present on v1.2 public poll votes.
    #[serde(default)]
    pub profile_picture: Option<String>,
    /// v1.5: where a carried vote came from.
    #[serde(default, skip_serializing)]
    pub carried_from: Option<Origin>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
    /// Only sent for v1.2. Omit for v1.0/v1.1 migration calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_type: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
    /// Only sent for v1.2 public polls; None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Only sent for v1.2 public polls; None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
}

// ── Frontend response types ───────────────────────────────────────────
//
// These are what the frontend receives via Tauri invoke().
// ActionHashes are converted to strings because TypeScript can't handle
// Holochain's native hash types. The frontend uses these string hashes
// to reference entries in subsequent calls.

#[derive(serde::Serialize, Clone)]
pub struct PollListItem {
    pub hash: String,
    pub poll: Poll,
    pub author: String,
    /// Which DHT this poll lives on: "1.0", "1.1", or "1.2".
    /// The frontend passes this back when voting so the vote goes to the correct cell.
    pub dna_version: String,
}

#[derive(serde::Serialize)]
pub struct PollDetail {
    /// The poll this detail is of: the hash asked for, or the copy on the
    /// active network an older hash opens.
    pub hash: String,
    pub poll: Poll,
    pub author: String,
    /// Which DHT this poll lives on: "1.0", "1.1", or "1.2".
    pub dna_version: String,
}

#[derive(serde::Serialize, Clone)]
pub struct VoteData {
    pub vote: VoteResponse,
    pub author: String,
    /// Display name from v1.2 public poll votes. None for anonymous or pre-v1.2 votes.
    pub display_name: Option<String>,
    /// Profile picture URL from v1.2 public poll votes. None for anonymous or pre-v1.2 votes.
    pub profile_picture: Option<String>,
}

#[derive(serde::Serialize, Clone)]
pub struct VoteResponse {
    pub hash: String,
    pub poll_action_hash: String,
    pub option_index: u32,
}
