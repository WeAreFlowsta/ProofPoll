//! Fixture loader for the v1.5 sweettest.
//!
//! `fixtures/carried_records.json` holds real records from a ProofPoll test
//! instance, exactly as their author signed them: v1.3 written through
//! Holochain 0.6.1 (a v1.3 cell cannot run on a 0.7.1 conductor, so these
//! bytes are how a test gets one) and v1.4 through 0.7.1.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedRecord {
    /// "1.3" or "1.4".
    pub dna: String,
    /// The record's action hash on its network (base64url, `uhCkk…`).
    pub action_hash: String,
    pub author: String,
    pub timestamp_us: i64,
    pub zome_index: u8,
    pub entry_index: u8,
    /// The bytes the author signed (hex).
    pub action: String,
    pub signature: String,
    pub entry: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CarriedRecords {
    pub v13: Vec<SignedRecord>,
    pub v14: Vec<SignedRecord>,
}

pub fn carried_records() -> CarriedRecords {
    let raw = include_str!("../fixtures/carried_records.json");
    serde_json::from_str(raw).expect("fixture parses")
}

/// polls_integrity's entry index for each kind on v1.3 and v1.4.
pub const POLL: u8 = 0;
pub const VOTE: u8 = 1;
pub const MIGRATED_POLL: u8 = 3;
