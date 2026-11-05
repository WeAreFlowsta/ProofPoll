//! Carried records: a v1.3 or v1.4 record written again on v1.5 with its
//! original record attached. The v1.5
//! integrity zome checks that proof; this module builds it from a record
//! read on the older DNA, and runs the same checks first, so nothing is
//! submitted that the network would refuse.
//!
//! The proof is the original action exactly as its author signed it. Each
//! line's own types re-encode a decoded action to those same bytes (both
//! lines use one serializer, `holochain_serialized_bytes`), which the tests
//! below check on real records; the signature then proves it.

use holochain_types_07::prelude::{ActionHash, AgentPubKey, Signature, Timestamp};
use serde::{Deserialize, Serialize};

/// The v1.5 integrity zome's `Carried`, field for field: the input of its
/// carry functions.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Carried {
    /// "1.3" (Holochain 0.6) or "1.4" (0.7).
    pub original_dna: String,
    pub original_action_hash: ActionHash,
    pub original_author: AgentPubKey,
    pub original_timestamp: Timestamp,
    #[serde(with = "serde_bytes")]
    pub original_action: Vec<u8>,
    pub original_signature: Signature,
    #[serde(with = "serde_bytes")]
    pub original_entry: Vec<u8>,
    /// A carried vote or flag: the mapping record of the poll it was cast
    /// on (`get_migration_record`).
    pub poll_mapping: Option<ActionHash>,
}

#[derive(Serialize, Debug)]
pub struct CarryInput {
    pub carried: Carried,
}

/// A v1.3 record (Holochain 0.6) as a carried one.
pub fn from_v13(
    action: &holochain_types::prelude::Action,
    signature: &holochain_types::prelude::Signature,
    entry: &[u8],
    poll_mapping: Option<ActionHash>,
) -> Result<Carried, String> {
    let original_action = holochain_types::prelude::encode(action).map_err(|e| format!("encode the v1.3 action: {e}"))?;
    Ok(Carried {
        original_dna: "1.3".to_string(),
        original_action_hash: action_hash_of(&original_action),
        original_author: AgentPubKey::from_raw_39(action.author().get_raw_39().to_vec()),
        original_timestamp: Timestamp::from_micros(action.timestamp().as_micros()),
        original_action,
        original_signature: Signature(signature.0),
        original_entry: entry.to_vec(),
        poll_mapping,
    })
}

/// A v1.4 record (Holochain 0.7) as a carried one.
pub fn from_v14(
    action: &holochain_types_07::prelude::Action,
    signature: &Signature,
    entry: &[u8],
    poll_mapping: Option<ActionHash>,
) -> Result<Carried, String> {
    let original_action = holochain_types_07::prelude::encode(action).map_err(|e| format!("encode the v1.4 action: {e}"))?;
    Ok(Carried {
        original_dna: "1.4".to_string(),
        original_action_hash: action_hash_of(&original_action),
        original_author: action.author().clone(),
        original_timestamp: action.timestamp(),
        original_action,
        original_signature: signature.clone(),
        original_entry: entry.to_vec(),
        poll_mapping,
    })
}

/// The records a v1.3 or v1.4 zome call returned (`Vec<Record>`), each as a
/// carried one; records with no app entry are left out.
pub fn from_records(dna: &str, bytes: &[u8]) -> Result<Vec<Carried>, String> {
    match dna {
        "1.3" => {
            let recs: Vec<holochain_types::prelude::Record> = crate::hc::decode(bytes)?;
            Ok(recs.iter().filter_map(|r| from_record_06(r).transpose()).collect::<Result<_, _>>()?)
        }
        "1.4" => {
            let recs: Vec<holochain_types_07::prelude::Record> = holochain_types_07::prelude::ExternIO::from(bytes.to_vec())
                .decode()
                .map_err(|e| format!("Failed to decode records: {}", e))?;
            Ok(recs.iter().filter_map(|r| from_record_07(r).transpose()).collect::<Result<_, _>>()?)
        }
        other => Err(format!("records are not carried from DNA {other}")),
    }
}

/// The record a v1.3 or v1.4 zome call returned (`Option<Record>`), as a
/// carried one.
pub fn from_record(dna: &str, bytes: &[u8]) -> Result<Option<Carried>, String> {
    match dna {
        "1.3" => {
            let rec: Option<holochain_types::prelude::Record> = crate::hc::decode(bytes)?;
            rec.as_ref().map_or(Ok(None), from_record_06)
        }
        "1.4" => {
            let rec: Option<holochain_types_07::prelude::Record> = holochain_types_07::prelude::ExternIO::from(bytes.to_vec())
                .decode()
                .map_err(|e| format!("Failed to decode record: {}", e))?;
            rec.as_ref().map_or(Ok(None), from_record_07)
        }
        other => Err(format!("records are not carried from DNA {other}")),
    }
}

fn from_record_06(r: &holochain_types::prelude::Record) -> Result<Option<Carried>, String> {
    let Some(entry) = r.entry().as_option().and_then(|e| e.as_app_entry()) else { return Ok(None) };
    from_v13(r.action(), r.signature(), entry.as_ref().bytes(), None).map(Some)
}

fn from_record_07(r: &holochain_types_07::prelude::Record) -> Result<Option<Carried>, String> {
    let Some(entry) = r.entry().as_option().and_then(|e| e.as_app_entry()) else { return Ok(None) };
    from_v14(r.action(), r.signature(), entry.as_ref().bytes(), None).map(Some)
}

/// An action's hash on its network: blake2b-256 of its encoded bytes.
fn action_hash_of(encoded: &[u8]) -> ActionHash {
    ActionHash::from_raw_32(holo_hash_07::encode::blake2b_256(encoded))
}

/// The original action's author, time, entry hash and entry kind (zome and
/// entry index), decoded with its own line's types.
fn original_of(c: &Carried) -> Result<(AgentPubKey, Timestamp, Vec<u8>, (u8, u8)), String> {
    match c.original_dna.as_str() {
        "1.3" => {
            use holochain_types::prelude::{Action, EntryType};
            let a: Action = holochain_types::prelude::decode(&c.original_action).map_err(|e| format!("the original action does not decode: {e}"))?;
            let Some((hash, EntryType::App(def))) = a.entry_data() else { return Err("the original is not an app entry".to_string()) };
            Ok((
                AgentPubKey::from_raw_39(a.author().get_raw_39().to_vec()),
                Timestamp::from_micros(a.timestamp().as_micros()),
                hash.get_raw_39().to_vec(),
                (def.zome_index.0, def.entry_index.0),
            ))
        }
        "1.4" => {
            use holochain_types_07::prelude::{Action, EntryType};
            let a: Action = holochain_types_07::prelude::decode(&c.original_action).map_err(|e| format!("the original action does not decode: {e}"))?;
            let Some((hash, EntryType::App(def))) = a.entry_data() else { return Err("the original is not an app entry".to_string()) };
            Ok((a.author().clone(), a.timestamp(), hash.get_raw_39().to_vec(), (def.zome_index.0, def.entry_index.0)))
        }
        other => Err(format!("records are not carried from DNA {other}")),
    }
}

/// The v1.5 integrity zome's steps 1-3 (its step 4, the copy equal to the
/// original, holds by construction: the carry functions build the copy
/// from the original entry). Returns the entry's (zome, entry) index.
pub fn check(c: &Carried) -> Result<(u8, u8), String> {
    let (author, timestamp, entry_hash, kind) = original_of(c)?;
    if author != c.original_author || timestamp != c.original_timestamp {
        return Err("the original action names another author or time".to_string());
    }
    if action_hash_of(&c.original_action) != c.original_action_hash {
        return Err("the original action does not hash to the carried action hash".to_string());
    }
    if !verifies(&c.original_author, &c.original_signature, &c.original_action) {
        return Err("the original author's signature does not verify".to_string());
    }
    use holochain_integrity_types_07::entry::{AppEntryBytes, Entry};
    use holochain_types_07::prelude::{EntryHash, SerializedBytes, UnsafeBytes};
    let hashed = EntryHash::with_data_sync(&Entry::App(AppEntryBytes(SerializedBytes::from(UnsafeBytes::from(c.original_entry.clone())))));
    if hashed.get_raw_39() != entry_hash.as_slice() {
        return Err("the original entry does not hash to the entry the action names".to_string());
    }
    Ok(kind)
}

/// The v1.5 integrity zome's `LaterCopy`: a later copy of a poll (a v1.4
/// copy its author made of a v1.3 poll) and that DNA's mapping to it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LaterCopy {
    pub poll: Carried,
    pub mapping: Carried,
    /// When the later copy is under another key of the first record's
    /// author: that author's signature over `key_vouch_payload` of the later
    /// key.
    #[serde(default)]
    pub vouch: Option<Signature>,
}

/// What a key signs to vouch that `other` is also its holder's (the v1.5
/// integrity zome's `key_vouch_payload`).
pub fn key_vouch_payload(other: &AgentPubKey) -> Vec<u8> {
    let mut payload = b"ProofPoll: this key is also mine\n".to_vec();
    payload.extend_from_slice(other.get_raw_39());
    payload
}

/// Whether `signature` by `key` verifies over `message`.
fn verifies(key: &AgentPubKey, signature: &Signature, message: &[u8]) -> bool {
    let Ok(key) = <[u8; 32]>::try_from(key.get_raw_32()) else { return false };
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .map(|k| k.verify_strict(message, &ed25519_dalek::Signature::from_bytes(&signature.0)).is_ok())
        .unwrap_or(false)
}

/// The input of v1.5's `map_later_copy`: the poll's one copy there.
#[derive(Serialize, Debug)]
pub struct MapLaterCopyInput {
    pub copy: ActionHash,
    pub later: LaterCopy,
}

/// The (zome, entry) index of each kind on v1.3 and v1.4.
pub const POLL: (u8, u8) = (1, 0);
pub const VOTE: (u8, u8) = (1, 1);
pub const FLAG: (u8, u8) = (1, 2);
pub const MAPPING: (u8, u8) = (1, 3);
pub const PRIVATE: (u8, u8) = (1, 4);
pub const LINK: (u8, u8) = (0, 0);

/// A vote's or flag's poll, on the vote's own DNA.
#[derive(Deserialize, Debug)]
pub struct OnPoll {
    pub poll_action_hash: ActionHash,
}

/// An older DNA's poll mapping.
#[derive(Deserialize, Debug)]
pub struct Mapping {
    pub old_action_hash: ActionHash,
    pub new_action_hash: ActionHash,
}

/// A carried record's entry, read as `T`.
pub fn fields<T: serde::de::DeserializeOwned>(c: &Carried) -> Option<T> {
    rmp_serde::from_slice(&c.original_entry).ok()
}

/// The v1.5 integrity zome's check of a later copy of `first` (the record
/// the poll's copy there carries): by one person, one key or the first
/// record's key vouching for the later one. Run first so a mapping the
/// network would refuse is never submitted.
pub fn check_later_copy(first: &Carried, later: &LaterCopy) -> Result<(), String> {
    if later.poll.poll_mapping.is_some() || later.mapping.poll_mapping.is_some() {
        return Err("a later copy's records name no poll mapping".to_string());
    }
    if later.poll.original_dna != later.mapping.original_dna {
        return Err("the later copy and its mapping are on different DNAs".to_string());
    }
    if !(first.original_dna == "1.3" && later.poll.original_dna == "1.4") {
        return Err("a later copy is on a later DNA than the first record".to_string());
    }
    if later.mapping.original_author != later.poll.original_author {
        return Err("a later copy and its mapping are by one author".to_string());
    }
    let vouched = || later.vouch.as_ref().is_some_and(|v| verifies(&first.original_author, v, &key_vouch_payload(&later.poll.original_author)));
    if later.poll.original_author != first.original_author && !vouched() {
        return Err("a later copy is by the first record's author or a key it vouches for".to_string());
    }
    if check(&later.poll).map_err(|e| format!("the later copy: {e}"))? != POLL {
        return Err("the later copy: the original is another kind of entry".to_string());
    }
    if check(&later.mapping).map_err(|e| format!("the later copy's mapping: {e}"))? != MAPPING {
        return Err("the later copy's mapping: the original is another kind of entry".to_string());
    }
    let m: Mapping = holochain_types_07::prelude::decode(&later.mapping.original_entry)
        .map_err(|e| format!("the later copy's mapping does not decode: {e}"))?;
    if m.old_action_hash != first.original_action_hash || m.new_action_hash != later.poll.original_action_hash {
        return Err("the later copy's mapping maps the first record to the later copy".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v1.5 DNA's fixtures: real records from a test instance, exactly
    /// as signed (written by the harness's `fixtures` op).
    #[derive(Deserialize)]
    struct Record {
        dna: String,
        action_hash: String,
        author: String,
        timestamp_us: i64,
        zome_index: u8,
        entry_index: u8,
        action: String,
        signature: String,
        entry: String,
    }

    #[derive(Deserialize)]
    struct Fixtures {
        v13: Vec<Record>,
        v14: Vec<Record>,
    }

    fn fixtures() -> Fixtures {
        serde_json::from_str(include_str!("../../dna/v1.5/tests/fixtures/carried_records.json")).unwrap()
    }

    /// Decode a fixture's action with its own line's types and build the
    /// carried record from it, as the app and the node do from a record.
    fn rebuilt(r: &Record) -> Carried {
        let action = hex::decode(&r.action).unwrap();
        let signature: [u8; 64] = hex::decode(&r.signature).unwrap().try_into().unwrap();
        let entry = hex::decode(&r.entry).unwrap();
        match r.dna.as_str() {
            "1.3" => {
                let a: holochain_types::prelude::Action = holochain_types::prelude::decode(&action).unwrap();
                from_v13(&a, &holochain_types::prelude::Signature(signature), &entry, None).unwrap()
            }
            _ => {
                let a: holochain_types_07::prelude::Action = holochain_types_07::prelude::decode(&action).unwrap();
                from_v14(&a, &Signature(signature), &entry, None).unwrap()
            }
        }
    }

    #[test]
    fn a_decoded_action_re_encodes_to_the_signed_bytes_on_both_lines() {
        let f = fixtures();
        assert_eq!((f.v13.len(), f.v14.len()), (3, 4));
        for r in f.v13.iter().chain(f.v14.iter()) {
            let c = rebuilt(r);
            assert_eq!(hex::encode(&c.original_action), r.action, "{} re-encodes byte for byte", r.action_hash);
            assert_eq!(c.original_action_hash.to_string(), r.action_hash, "and hashes to its action hash");
            assert_eq!(c.original_author.to_string(), r.author);
            assert_eq!(c.original_timestamp.as_micros(), r.timestamp_us);
            assert_eq!(check(&c), Ok((r.zome_index, r.entry_index)), "{} passes the network's checks", r.action_hash);
        }
    }

    /// The fixture's two v1.3 polls, their v1.4 copies and the v1.4 mappings
    /// (the test instance's own move): (first, later copy) for each.
    fn moved(f: &Fixtures) -> Vec<(Carried, LaterCopy)> {
        let polls13: Vec<Carried> = f.v13.iter().filter(|r| (r.zome_index, r.entry_index) == POLL).map(rebuilt).collect();
        let polls14: Vec<Carried> = f.v14.iter().filter(|r| (r.zome_index, r.entry_index) == POLL).map(rebuilt).collect();
        f.v14
            .iter()
            .filter(|r| (r.zome_index, r.entry_index) == MAPPING)
            .map(|r| {
                let mapping = rebuilt(r);
                let m: Mapping = holochain_types_07::prelude::decode(&mapping.original_entry).unwrap();
                let first = polls13.iter().find(|p| p.original_action_hash == m.old_action_hash).unwrap().clone();
                let poll = polls14.iter().find(|p| p.original_action_hash == m.new_action_hash).unwrap().clone();
                (first, LaterCopy { poll, mapping, vouch: None })
            })
            .collect()
    }

    #[test]
    fn a_later_copy_passes_the_networks_check_and_a_mismatch_does_not() {
        let f = fixtures();
        let moved = moved(&f);
        assert_eq!(moved.len(), 2);
        for (first, later) in &moved {
            assert_eq!(check_later_copy(first, later), Ok(()));
        }
        let (first_a, later_a) = &moved[0];
        let (_, later_b) = &moved[1];
        let swapped = LaterCopy { poll: later_a.poll.clone(), mapping: later_b.mapping.clone(), vouch: None };
        assert!(check_later_copy(first_a, &swapped).unwrap_err().contains("maps the first record"));
        let wrong_dna = LaterCopy { poll: first_a.clone(), mapping: later_a.mapping.clone(), vouch: None };
        assert!(check_later_copy(first_a, &wrong_dna).unwrap_err().contains("different DNAs"));
        let mut other_author = later_a.clone();
        other_author.mapping.original_author = AgentPubKey::from_raw_32(vec![7u8; 32]);
        assert!(check_later_copy(first_a, &other_author).unwrap_err().contains("by one author"));
        let mut other_key = later_a.clone();
        other_key.poll.original_author = AgentPubKey::from_raw_32(vec![7u8; 32]);
        other_key.mapping.original_author = other_key.poll.original_author.clone();
        assert!(check_later_copy(first_a, &other_key).unwrap_err().contains("a key it vouches for"));
        other_key.vouch = Some(Signature([7u8; 64]));
        assert!(check_later_copy(first_a, &other_key).unwrap_err().contains("a key it vouches for"));
        let poll_as_mapping = LaterCopy { poll: later_a.poll.clone(), mapping: later_a.poll.clone(), vouch: None };
        assert!(check_later_copy(first_a, &poll_as_mapping).unwrap_err().contains("another kind of entry"));
    }

    #[test]
    fn a_forged_record_fails_the_checks() {
        let f = fixtures();
        let genuine = rebuilt(&f.v13[0]);
        let mut c = genuine.clone();
        c.original_signature.0[5] ^= 1;
        assert!(check(&c).unwrap_err().contains("signature"));
        let mut c = genuine.clone();
        c.original_entry[3] ^= 1;
        assert!(check(&c).unwrap_err().contains("entry"));
        let mut c = genuine.clone();
        c.original_action_hash = rebuilt(&f.v13[1]).original_action_hash;
        assert!(check(&c).unwrap_err().contains("hash to the carried action hash"));
        let mut c = genuine.clone();
        c.original_author = AgentPubKey::from_raw_32(vec![7u8; 32]);
        assert!(check(&c).unwrap_err().contains("another author"));
        let mut c = genuine;
        c.original_dna = "1.4".to_string();
        assert!(check(&c).unwrap_err().contains("does not decode"));
    }
}
