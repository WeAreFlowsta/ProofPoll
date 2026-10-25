//! Fixture loader for the v1.4 sweettest.
//!
//! `fixtures/v1_3_export.json` is what the app's migration engine reads
//! from a v1.3 cell (Holochain 0.6.1): each poll as stored there plus the
//! action hash it had on that network. A v1.3 cell cannot run on a 0.7.1
//! conductor, so the round-trip test feeds this export instead.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportedPoll {
    /// The poll's action hash on the v1.3 network (base64url, `uhCkk…`).
    pub old_action_hash: String,
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    /// "Anonymous" | "Public", as the v1.3 entry spells it.
    pub poll_type: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct V13Export {
    pub dna: String,
    pub network_seed: String,
    pub exported_at: i64,
    pub polls: Vec<ExportedPoll>,
}

pub fn v1_3_export() -> V13Export {
    let raw = include_str!("../fixtures/v1_3_export.json");
    serde_json::from_str(raw).expect("fixture parses")
}
