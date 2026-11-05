//! ProofPoll DNA v1.5 on a real Holochain 0.7.1 conductor.
//!
//! Loads the packed bundle from `../workdir/proofpoll_v1_5.dna`, so a passing
//! run says the committed artifact works on this conductor: every native
//! validation arm, carried records checked against real v1.3 and v1.4
//! signatures (`fixtures/carried_records.json`), every forgery refused, a
//! later copy under another key of its author, two copies of one poll read
//! as one, and two conductors gossiping native and carried records.
//!
//! Two tests also load the test coordinator in `tests/hostile` (build it
//! first, as `tests/hostile.rs` says).

use holo_hash::{ActionHashB64, AgentPubKeyB64};
use holochain::prelude::*;
use holochain::sweettest::*;
use polls_coordinator::{CarryInput, CastVoteInput, CreateEncryptedEntryInput, CreatePollInput, FlagPollInput, MapLaterCopyInput};
use polls_integrity::{Carried, FlagReason, LaterCopy, MigratedPollFields, Poll, PollType, Vote};
use proofpoll_v1_5_tests::{carried_records, SignedRecord, MIGRATED_POLL, POLL, VOTE};
use std::path::Path;

const ZOME: &str = "polls";

async fn dna() -> DnaFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../workdir/proofpoll_v1_5.dna");
    SweetDnaFile::from_bundle(&path)
        .await
        .expect("dna/v1.5/workdir/proofpoll_v1_5.dna loads (run build.sh first)")
}

fn poll_input(title: &str, options: &[&str]) -> CreatePollInput {
    CreatePollInput {
        title: title.to_string(),
        description: "test".to_string(),
        options: options.iter().map(|s| s.to_string()).collect(),
        closes_at: None,
        poll_type: PollType::Anonymous,
    }
}

fn decode<T: TryFrom<SerializedBytes, Error = SerializedBytesError>>(record: &Record) -> T {
    record
        .entry()
        .to_app_option::<T>()
        .ok()
        .flatten()
        .expect("record carries the expected app entry")
}

/// A fixture record as the carrier hands it over.
fn carried(r: &SignedRecord, poll_mapping: Option<ActionHash>) -> Carried {
    let signature: [u8; 64] = hex::decode(&r.signature).unwrap().try_into().unwrap();
    Carried {
        original_dna: r.dna.clone(),
        original_action_hash: ActionHashB64::from_b64_str(&r.action_hash).unwrap().into(),
        original_author: AgentPubKeyB64::from_b64_str(&r.author).unwrap().into(),
        original_timestamp: Timestamp::from_micros(r.timestamp_us),
        original_action: hex::decode(&r.action).unwrap(),
        original_signature: Signature(signature),
        original_entry: hex::decode(&r.entry).unwrap(),
        poll_mapping,
    }
}

fn of_kind(records: &[SignedRecord], entry_index: u8) -> Vec<SignedRecord> {
    records.iter().filter(|r| r.zome_index == 1 && r.entry_index == entry_index).cloned().collect()
}

/// Every arm of `validate`, reached through the coordinator the app uses.
#[tokio::test(flavor = "multi_thread")]
async fn validation_arms() {
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);

    // Poll: valid
    let poll_hash: ActionHash = conductor
        .call(&zome, "create_poll", poll_input("Lunch?", &["Soup", "Salad"]))
        .await;
    let record: Option<Record> = conductor.call(&zome, "get_poll", poll_hash.clone()).await;
    let poll: Poll = decode(&record.expect("poll readable"));
    assert_eq!(poll.title, "Lunch?");
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(all.len(), 1, "AllPolls link from the anchor");

    // Poll: each Invalid branch of validate_poll
    let bad = [
        poll_input("   ", &["A", "B"]),
        poll_input("One option", &["A"]),
        poll_input(
            "Eleven",
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"],
        ),
        poll_input("Blank option", &["A", " "]),
    ];
    for input in bad {
        let r: Result<ActionHash, _> = conductor.call_fallible(&zome, "create_poll", input).await;
        assert!(r.is_err(), "invalid poll must be rejected");
    }
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(all.len(), 1, "rejected polls leave no trace");

    // Vote
    let vote_hash: ActionHash = conductor
        .call(
            &zome,
            "cast_vote",
            CastVoteInput {
                poll_action_hash: poll_hash.clone(),
                option_index: 1,
                display_name: None,
                profile_picture: None,
            },
        )
        .await;
    let votes: Vec<Record> = conductor.call(&zome, "get_poll_votes", poll_hash.clone()).await;
    assert_eq!(votes.len(), 1);
    let twice: Result<ActionHash, _> = conductor
        .call_fallible(
            &zome,
            "cast_vote",
            CastVoteInput {
                poll_action_hash: poll_hash.clone(),
                option_index: 0,
                display_name: None,
                profile_picture: None,
            },
        )
        .await;
    assert!(twice.is_err(), "one vote per agent");

    // Flag + remove
    let flag_hash: ActionHash = conductor
        .call(
            &zome,
            "flag_poll",
            FlagPollInput {
                poll_action_hash: poll_hash.clone(),
                reason: FlagReason::OffTopic,
            },
        )
        .await;
    let flags: Vec<Record> = conductor.call(&zome, "get_poll_flags", poll_hash.clone()).await;
    assert_eq!(flags.len(), 1);
    let _: ActionHash = conductor.call(&zome, "remove_flag", flag_hash).await;
    let flags: Vec<Record> = conductor.call(&zome, "get_poll_flags", poll_hash.clone()).await;
    assert_eq!(flags.len(), 0, "DeleteLink arm accepts the author's removal");

    // EncryptedEntry: valid draft, then each Invalid branch
    let _: ActionHash = conductor
        .call(
            &zome,
            "create_encrypted_entry",
            CreateEncryptedEntryInput {
                cipher: vec![7; 48],
                nonce: vec![1; 24],
                link_as: "draft_poll".to_string(),
                related_hash: None,
            },
        )
        .await;
    let drafts: Vec<Record> = conductor.call(&zome, "get_my_drafts", ()).await;
    assert_eq!(drafts.len(), 1, "AgentDrafts link");
    let rationale: ActionHash = conductor
        .call(
            &zome,
            "create_encrypted_entry",
            CreateEncryptedEntryInput {
                cipher: vec![9; 16],
                nonce: vec![2; 24],
                link_as: "vote_rationale".to_string(),
                related_hash: Some(vote_hash.clone()),
            },
        )
        .await;
    let found: Option<Record> = conductor.call(&zome, "get_vote_rationale", vote_hash.clone()).await;
    assert_eq!(found.map(|r| r.action_address().clone()), Some(rationale), "VoteToRationale link");
    for (cipher, nonce) in [(vec![7; 8], vec![1; 12]), (vec![], vec![1; 24])] {
        let r: Result<ActionHash, _> = conductor
            .call_fallible(
                &zome,
                "create_encrypted_entry",
                CreateEncryptedEntryInput { cipher, nonce, link_as: "draft_poll".to_string(), related_hash: None },
            )
            .await;
        assert!(r.is_err(), "validate_encrypted_entry rejects a short nonce and an empty cipher");
    }

}

/// Real v1.3 polls and a vote, carried with their proofs: the network
/// checks each against its author's signature, the copy equals the
/// original, and a second carry of the same record writes nothing new.
#[tokio::test(flavor = "multi_thread")]
async fn v13_polls_and_a_vote_carried_with_their_proofs() {
    let fixtures = carried_records();
    let polls = of_kind(&fixtures.v13, POLL);
    let votes = of_kind(&fixtures.v13, VOTE);
    assert_eq!((polls.len(), votes.len()), (2, 1), "the fixture holds two v1.3 polls and a vote");
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);

    let mut copies = Vec::new();
    for r in &polls {
        let c = carried(r, None);
        let copy: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: c.clone() }).await;
        let record: Option<Record> = conductor.call(&zome, "get_poll", copy.clone()).await;
        let poll: Poll = decode(&record.expect("copy readable"));
        let from = poll.carried_from.expect("the copy carries its origin");
        assert_eq!(from.original_author, c.original_author);
        assert_eq!(from.original_action_hash, c.original_action_hash);
        assert!(poll.created_at > 0, "the original's creation time kept");
        let mapped: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", c.original_action_hash.clone()).await;
        assert_eq!(mapped.as_ref(), Some(&copy));
        let again: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: c }).await;
        assert_eq!(again, copy, "once per original");
        copies.push(copy);
    }
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(all.len(), 2, "one copy per poll");

    // The vote, filed under its poll's copy by the mapping it names.
    let vote = &votes[0];
    let original: polls_integrity::VoteFields = polls_integrity::original_fields(&carried(vote, None)).unwrap();
    let mapping: Option<ActionHash> = conductor.call(&zome, "get_migration_record", original.poll_action_hash.clone()).await;
    let mapping = mapping.expect("the vote's poll was carried");
    let c = carried(vote, Some(mapping));
    let vote_hash: ActionHash = conductor.call(&zome, "carry_vote", CarryInput { carried: c.clone() }).await;
    let copy_of_its_poll: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", original.poll_action_hash.clone()).await;
    let filed: Vec<Record> = conductor.call(&zome, "get_poll_votes", copy_of_its_poll.clone().unwrap()).await;
    assert_eq!(filed.len(), 1);
    let v: Vote = decode(&filed[0]);
    assert_eq!(v.option_index, original.option_index);
    assert_eq!(v.carried_from.expect("carried").original_author, c.original_author, "counted for its voter");
    let again: ActionHash = conductor.call(&zome, "carry_vote", CarryInput { carried: c }).await;
    assert_eq!(again, vote_hash, "once per voter and poll");

    // Another person votes natively beside it.
    let _: ActionHash = conductor
        .call(&zome, "cast_vote", CastVoteInput { poll_action_hash: copy_of_its_poll.clone().unwrap(), option_index: 0, display_name: None, profile_picture: None })
        .await;
    let filed: Vec<Record> = conductor.call(&zome, "get_poll_votes", copy_of_its_poll.unwrap()).await;
    assert_eq!(filed.len(), 2);
    let _ = app;
}

/// Records written on v1.4 (Holochain 0.7.1) carry the same way: here the
/// test instance's own v1.4 poll copies.
#[tokio::test(flavor = "multi_thread")]
async fn v14_records_carried_with_their_proofs() {
    let fixtures = carried_records();
    let polls = of_kind(&fixtures.v14, POLL);
    assert_eq!(polls.len(), 2);
    assert_eq!(of_kind(&fixtures.v14, MIGRATED_POLL).len(), 2, "v1.4 mappings are not carried (v1.5 makes its own)");
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);
    for r in &polls {
        let copy: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: carried(r, None) }).await;
        let record: Option<Record> = conductor.call(&zome, "get_poll", copy).await;
        let poll: Poll = decode(&record.unwrap());
        assert_eq!(poll.carried_from.unwrap().original_dna, "1.4");
    }
}

/// Every way to forge a carried record is refused by validation, and leaves
/// nothing behind.
#[tokio::test(flavor = "multi_thread")]
async fn forged_carried_records_are_refused() {
    let fixtures = carried_records();
    let polls = of_kind(&fixtures.v13, POLL);
    let vote = of_kind(&fixtures.v13, VOTE)[0].clone();
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);
    let genuine = carried(&polls[0], None);

    // Each forgery and the reason it must be refused for (any one of them).
    let mut forgeries: Vec<(&str, Carried, &[&str])> = Vec::new();
    let mut c = genuine.clone();
    c.original_signature.0[0] ^= 1;
    forgeries.push(("a flipped signature byte", c, &["signature does not verify"]));
    let mut c = genuine.clone();
    let last = c.original_entry.len() - 1;
    c.original_entry[last] ^= 1;
    forgeries.push(("a flipped entry byte", c, &["does not hash to the entry", "entry does not decode"]));
    let mut c = genuine.clone();
    c.original_action_hash = carried(&polls[1], None).original_action_hash;
    forgeries.push(("another record's action hash", c, &["does not hash to the carried action hash"]));
    let mut c = genuine.clone();
    c.original_author = app.agent().clone();
    forgeries.push(("another author", c, &["names another author or time"]));
    let mut c = genuine.clone();
    c.original_dna = "1.4".to_string();
    forgeries.push(("the wrong line", c, &["action does not decode"]));
    forgeries.push(("a vote passed as a poll", carried(&vote, None), &["entry does not decode", "another kind of entry"]));
    for (what, c, reasons) in forgeries {
        let r: Result<ActionHash, _> = conductor.call_fallible(&zome, "carry_poll", CarryInput { carried: c }).await;
        let e = format!("{:?}", r.expect_err(what));
        assert!(reasons.iter().any(|reason| e.contains(reason)), "{what} refused for its own reason, not: {e}");
    }
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert!(all.is_empty(), "no forgery left a poll");

    // A vote pointed at the mapping of a poll it was not cast on.
    let original: polls_integrity::VoteFields = polls_integrity::original_fields(&carried(&vote, None)).unwrap();
    let other = polls.iter().find(|p| ActionHashB64::from_b64_str(&p.action_hash).map(ActionHash::from).ok() != Some(original.poll_action_hash.clone())).unwrap();
    let _: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: carried(other, None) }).await;
    let wrong: Option<ActionHash> = conductor.call(&zome, "get_migration_record", carried(other, None).original_action_hash).await;
    let r: Result<ActionHash, _> = conductor.call_fallible(&zome, "carry_vote", CarryInput { carried: carried(&vote, wrong) }).await;
    let e = format!("{:?}", r.expect_err("a vote under another poll's mapping"));
    assert!(e.contains("does not map the original's poll"), "refused for its own reason, not: {e}");
}

fn hash_of(r: &SignedRecord) -> ActionHash {
    ActionHashB64::from_b64_str(&r.action_hash).unwrap().into()
}

/// A poll its author moved v1.3 -> v1.4 has one copy here: carried from the
/// v1.3 record, with the v1.4 copy's hash mapped to it by the v1.4 mapping's
/// proof (the test instance's own move, both real). Mismatched or misplaced
/// proofs are refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_copy_maps_to_the_first_records_copy() {
    let fixtures = carried_records();
    let polls13 = of_kind(&fixtures.v13, POLL);
    let polls14 = of_kind(&fixtures.v14, POLL);
    let maps14 = of_kind(&fixtures.v14, MIGRATED_POLL);
    assert_eq!((polls13.len(), polls14.len(), maps14.len()), (2, 2, 2), "two polls moved v1.3 -> v1.4, with their mappings");
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);

    // Each v1.4 mapping with the v1.3 poll and the v1.4 copy it names.
    let mut moved = Vec::new();
    for m in &maps14 {
        let f: MigratedPollFields = polls_integrity::original_fields(&carried(m, None)).unwrap();
        let first = polls13.iter().find(|p| hash_of(p) == f.old_action_hash).expect("the mapping's v1.3 poll").clone();
        let later = polls14.iter().find(|p| hash_of(p) == f.new_action_hash).expect("the mapping's v1.4 copy").clone();
        moved.push((first, later, m.clone()));
    }

    // Forgeries first, against the first poll's copy.
    let (first_a, later_a, map_a) = &moved[0];
    let (first_b, later_b, map_b) = &moved[1];
    let copy_a: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: carried(first_a, None) }).await;
    let mut forgeries: Vec<(&str, LaterCopy, &[&str])> = Vec::new();
    forgeries.push(("another poll's mapping", LaterCopy { poll: carried(later_a, None), mapping: carried(map_b, None), vouch: None }, &["maps the first record to the later copy"]));
    forgeries.push(("another poll's later copy", LaterCopy { poll: carried(later_b, None), mapping: carried(map_a, None), vouch: None }, &["maps the first record to the later copy"]));
    // (A v1.3 poll not carried yet: a hash already mapped returns its copy before any check.)
    forgeries.push(("a later copy on the first record's DNA", LaterCopy { poll: carried(first_b, None), mapping: carried(map_a, None), vouch: None }, &["different DNAs", "later DNA"]));
    let mut c = carried(map_a, None);
    c.original_author = app.agent().clone();
    forgeries.push(("a mapping by another author", LaterCopy { poll: carried(later_a, None), mapping: c, vouch: None }, &["by one author"]));
    let (mut p, mut m) = (carried(later_a, None), carried(map_a, None));
    p.original_author = app.agent().clone();
    m.original_author = app.agent().clone();
    forgeries.push(("a later copy by another key, no vouch", LaterCopy { poll: p.clone(), mapping: m.clone(), vouch: None }, &["a key it vouches for"]));
    forgeries.push(("a later copy by another key, a made-up vouch", LaterCopy { poll: p, mapping: m, vouch: Some(Signature([7; 64])) }, &["a key it vouches for"]));
    let mut c = carried(map_a, None);
    c.original_signature.0[0] ^= 1;
    forgeries.push(("a mapping with a flipped signature byte", LaterCopy { poll: carried(later_a, None), mapping: c, vouch: None }, &["signature does not verify"]));
    forgeries.push(("a poll passed as its mapping", LaterCopy { poll: carried(later_a, None), mapping: carried(later_a, None), vouch: None }, &["another kind of entry", "entry does not decode"]));
    for (what, later, reasons) in forgeries {
        let r: Result<ActionHash, _> = conductor.call_fallible(&zome, "map_later_copy", MapLaterCopyInput { copy: copy_a.clone(), later }).await;
        let e = format!("{:?}", r.expect_err(what));
        assert!(reasons.iter().any(|reason| e.contains(reason)), "{what} refused for its own reason, not: {e}");
    }
    let unmapped: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", hash_of(later_a)).await;
    assert_eq!(unmapped, None, "no forgery left a mapping");

    // The genuine ones: each later copy's hash opens its first record's copy.
    for (first, later, map) in &moved {
        let copy: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: carried(first, None) }).await;
        let to: ActionHash = conductor
            .call(&zome, "map_later_copy", MapLaterCopyInput { copy: copy.clone(), later: LaterCopy { poll: carried(later, None), mapping: carried(map, None), vouch: None } })
            .await;
        assert_eq!(to, copy);
        let via_later: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", hash_of(later)).await;
        let via_first: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", hash_of(first)).await;
        assert_eq!((via_later.as_ref(), via_first.as_ref()), (Some(&copy), Some(&copy)), "both older hashes open the one copy");
        let again: ActionHash = conductor
            .call(&zome, "map_later_copy", MapLaterCopyInput { copy: copy.clone(), later: LaterCopy { poll: carried(later, None), mapping: carried(map, None), vouch: None } })
            .await;
        assert_eq!(again, copy, "once per later copy");
        let record: Option<ActionHash> = conductor.call(&zome, "get_migration_record", hash_of(later)).await;
        assert!(record.is_some(), "a vote cast on the later copy names this mapping");
    }
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(all.len(), 2, "one copy per poll, not one per older hash");
}

/// A Holochain 0.6 Create as it encodes (an enum tagged by `type`).
#[derive(serde::Serialize, Debug)]
#[serde(tag = "type")]
enum Action06 {
    Create(Create06),
}

#[derive(serde::Serialize, Debug)]
struct Create06 {
    author: AgentPubKey,
    timestamp: Timestamp,
    action_seq: u32,
    prev_action: ActionHash,
    entry_type: EntryType,
    entry_hash: EntryHash,
}

/// A v1.4 poll mapping as it encodes.
#[derive(serde::Serialize, Debug)]
struct MigratedPollV14 {
    old_action_hash: ActionHash,
    new_action_hash: ActionHash,
    migrated_at: i64,
    carried_from: Option<()>,
}

/// `entry` written on `dna` ("1.3" in Holochain 0.6's action shape, "1.4"
/// in 0.7's) by `author` as entry `entry_index` of the polls zome, signed by
/// `author`'s key in `conductor`'s keystore.
async fn written_by(conductor: &SweetConductor, author: &AgentPubKey, dna: &str, entry_index: u8, entry: Vec<u8>, at: Timestamp) -> Carried {
    let entry_hash = EntryHash::with_data_sync(&Entry::App(AppEntryBytes(SerializedBytes::from(UnsafeBytes::from(entry.clone())))));
    let entry_type = EntryType::App(AppEntryDef::new(entry_index.into(), polls_integrity::POLLS_ZOME.into(), EntryVisibility::Public));
    let prev_action = ActionHash::from_raw_32(vec![0x11; 32]);
    let action = match dna {
        "1.3" => holochain_serialized_bytes::encode(&Action06::Create(Create06 { author: author.clone(), timestamp: at, action_seq: 7, prev_action, entry_type, entry_hash })),
        _ => holochain_serialized_bytes::encode(&Action {
            header: ActionHeader { author: author.clone(), timestamp: at, action_seq: 7, prev_action: Some(prev_action) },
            data: ActionData::Create(CreateData { entry_type, entry_hash }),
        }),
    }
    .unwrap();
    let signature = conductor.keystore().sign(author.clone(), action.clone().into()).await.unwrap();
    Carried {
        original_dna: dna.into(),
        original_action_hash: ActionHash::from_raw_32(holo_hash::encode::blake2b_256(&action)),
        original_author: author.clone(),
        original_timestamp: at,
        original_action: action,
        original_signature: signature,
        original_entry: entry,
        poll_mapping: None,
    }
}

/// An install whose v1.4 key is not its v1.3 key (a 0.6 key made by its
/// conductor): the person's v1.3 poll and their v1.4 copy of it, under the
/// later key, are one poll here once the v1.3 key vouches for the later one,
/// and only then.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_copy_under_a_new_key_needs_the_first_keys_vouch() {
    let fixtures = carried_records();
    let (poll13, poll14) = (of_kind(&fixtures.v13, POLL).remove(0), of_kind(&fixtures.v14, POLL).remove(0));
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);
    let keystore = conductor.keystore();
    let (first_key, later_key) = (SweetAgents::one(keystore.clone()).await, SweetAgents::one(keystore.clone()).await);

    let first = written_by(&conductor, &first_key, "1.3", POLL, hex::decode(&poll13.entry).unwrap(), Timestamp::from_micros(poll13.timestamp_us)).await;
    let later = written_by(&conductor, &later_key, "1.4", POLL, hex::decode(&poll14.entry).unwrap(), Timestamp::from_micros(poll14.timestamp_us)).await;
    let mapping_entry = holochain_serialized_bytes::encode(&MigratedPollV14 {
        old_action_hash: first.original_action_hash.clone(),
        new_action_hash: later.original_action_hash.clone(),
        migrated_at: poll14.timestamp_us / 1_000_000,
        carried_from: None,
    })
    .unwrap();
    let mapping = written_by(&conductor, &later_key, "1.4", MIGRATED_POLL, mapping_entry, Timestamp::from_micros(poll14.timestamp_us + 1_000)).await;
    let copy: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: first.clone() }).await;

    let vouch = |by: &AgentPubKey, for_key: &AgentPubKey| {
        let (keystore, by, payload) = (keystore.clone(), by.clone(), polls_integrity::key_vouch_payload(for_key));
        async move { keystore.sign(by, payload.into()).await.unwrap() }
    };
    let later_copy = |vouch: Option<Signature>| MapLaterCopyInput { copy: copy.clone(), later: LaterCopy { poll: later.clone(), mapping: mapping.clone(), vouch } };
    let refusals = [
        ("no vouch", None),
        ("the later key vouching for itself", Some(vouch(&later_key, &later_key).await)),
        ("the first key vouching for another key", Some(vouch(&first_key, app.agent()).await)),
    ];
    for (what, v) in refusals {
        let r: Result<ActionHash, _> = conductor.call_fallible(&zome, "map_later_copy", later_copy(v)).await;
        let e = format!("{:?}", r.expect_err(what));
        assert!(e.contains("a key it vouches for"), "{what} refused for its own reason, not: {e}");
    }
    let unmapped: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", later.original_action_hash.clone()).await;
    assert_eq!(unmapped, None, "no refusal left a mapping");

    let to: ActionHash = conductor.call(&zome, "map_later_copy", later_copy(Some(vouch(&first_key, &later_key).await))).await;
    assert_eq!(to, copy);
    let via_later: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", later.original_action_hash.clone()).await;
    let via_first: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", first.original_action_hash.clone()).await;
    assert_eq!((via_later.as_ref(), via_first.as_ref()), (Some(&copy), Some(&copy)), "both keys' hashes open the one copy");
    let all: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(all.len(), 1, "one poll, not one per key");
}

/// The bundle with the test coordinator (`tests/hostile`, built in its own
/// target folder) beside its own.
async fn dna_with_test_coordinator() -> DnaFile {
    let mut dna = dna().await;
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("hostile/target/wasm32-unknown-unknown/release/polls_hostile_test.wasm");
    let wasm = DnaWasm::from(std::fs::read(&file).expect("polls_hostile_test.wasm is built (see tests/hostile.rs)"));
    let hash = holo_hash::WasmHash::with_data(&wasm).await;
    let def = ZomeDef::Wasm(WasmZomeDef::new(hash, Some(vec!["polls_integrity".into()])));
    dna.update_coordinators(vec![(ZomeName::from("hostile"), def.into())], vec![wasm]).await.unwrap();
    dna
}

/// A v1.3 poll and vote as they encode.
#[derive(serde::Serialize, Debug)]
struct PollV13 {
    title: String,
    description: String,
    options: Vec<String>,
    created_at: i64,
    closes_at: Option<i64>,
    poll_type: PollType,
}

#[derive(serde::Serialize, Debug)]
struct VoteV13 {
    poll_action_hash: ActionHash,
    option_index: u32,
    display_name: Option<String>,
    profile_picture: Option<String>,
}

/// Two carriers at once: a second copy of one v1.3 poll (the test
/// coordinator's `carry_again`, as `carry_poll` writes one without its look
/// for a copy already there). The list shows one poll, the first carried;
/// the old hash opens it; a vote carried onto either copy counts on both,
/// once; and a voter who voted on one copy cannot vote again on the other.
/// The local reads (the app's fallback when the network is slow) read the
/// same.
#[tokio::test(flavor = "multi_thread")]
async fn two_copies_of_one_poll_read_as_one() {
    let mut conductor = SweetConductor::standard().await;
    let cell = conductor.setup_app("carrier", [&dna_with_test_coordinator().await]).await.unwrap().into_cells().remove(0);
    let (zome, test) = (cell.zome(ZOME), cell.zome("hostile"));
    let keystore = conductor.keystore();
    let (author, voter) = (SweetAgents::one(keystore.clone()).await, SweetAgents::one(keystore.clone()).await);
    let at = Timestamp::from_micros(Timestamp::now().as_micros() - 60_000_000);
    let poll_entry = PollV13 {
        title: "Carried twice".into(),
        description: String::new(),
        options: vec!["Yes".into(), "No".into()],
        created_at: at.as_micros() / 1_000_000,
        closes_at: None,
        poll_type: PollType::Anonymous,
    };
    let poll = written_by(&conductor, &author, "1.3", POLL, holochain_serialized_bytes::encode(&poll_entry).unwrap(), at).await;
    let vote_entry = VoteV13 { poll_action_hash: poll.original_action_hash.clone(), option_index: 1, display_name: None, profile_picture: None };
    let vote = written_by(&conductor, &voter, "1.3", VOTE, holochain_serialized_bytes::encode(&vote_entry).unwrap(), Timestamp::from_micros(at.as_micros() + 1_000_000)).await;

    let first: ActionHash = conductor.call(&zome, "carry_poll", CarryInput { carried: poll.clone() }).await;
    let (second, second_mapping): (ActionHash, ActionHash) = conductor.call(&test, "carry_again", poll.clone()).await;
    assert_ne!(first, second);
    let listed: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(listed.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![first.clone()], "one poll, the first copy");
    let local: Vec<Record> = conductor.call(&zome, "get_all_polls_local", ()).await;
    assert_eq!(local.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![first.clone()], "the local list reads the same");
    let mappings: Vec<Record> = conductor.call(&zome, "get_all_migration_mappings_local", ()).await;
    assert_eq!(mappings.len(), 2, "both copies' mappings, read locally");
    let opened: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", poll.original_action_hash.clone()).await;
    assert_eq!(opened, Some(first.clone()), "the old hash opens the first copy");

    // The vote carried onto the second copy counts on both, and carrying it
    // again by the first copy's mapping returns it.
    let mut v = vote.clone();
    v.poll_mapping = Some(second_mapping);
    let carried_vote: ActionHash = conductor.call(&zome, "carry_vote", CarryInput { carried: v }).await;
    let mut v = vote.clone();
    v.poll_mapping = conductor.call(&zome, "get_migration_record", poll.original_action_hash.clone()).await;
    let again: ActionHash = conductor.call(&zome, "carry_vote", CarryInput { carried: v }).await;
    assert_eq!(again, carried_vote, "one vote per voter across both copies");
    for copy in [&first, &second] {
        for read in ["get_poll_votes", "get_poll_votes_local"] {
            let votes: Vec<Record> = conductor.call(&zome, read, copy.clone()).await;
            assert_eq!(votes.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![carried_vote.clone()], "{read}");
        }
    }

    // A native vote on the first copy; then none on the second.
    let _: ActionHash = conductor
        .call(&zome, "cast_vote", CastVoteInput { poll_action_hash: first.clone(), option_index: 0, display_name: None, profile_picture: None })
        .await;
    let twice: Result<ActionHash, _> = conductor
        .call_fallible(&zome, "cast_vote", CastVoteInput { poll_action_hash: second.clone(), option_index: 1, display_name: None, profile_picture: None })
        .await;
    assert!(format!("{:?}", twice.expect_err("a second vote")).contains("already voted"));
    for read in ["get_poll_votes", "get_poll_votes_local"] {
        let votes: Vec<Record> = conductor.call(&zome, read, second.clone()).await;
        assert_eq!(votes.len(), 2, "the carried vote and the native one ({read})");
    }

    // A flag on the first copy reads on the second, locally too.
    let flag: ActionHash = conductor.call(&zome, "flag_poll", FlagPollInput { poll_action_hash: first.clone(), reason: FlagReason::Spam }).await;
    for read in ["get_poll_flags", "get_poll_flags_local"] {
        let flags: Vec<Record> = conductor.call(&zome, read, second.clone()).await;
        assert_eq!(flags.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![flag.clone()], "{read}");
    }
}

/// Two conductors on a local rendezvous: a poll authored on one is read and
/// voted on from the other (remote validation of both ops on 0.7.1).
#[tokio::test(flavor = "multi_thread")]
async fn two_conductors_gossip_a_poll_and_a_vote() {
    let dna = dna().await;
    let mut conductors = SweetConductorBatch::standard(2).await;
    let apps = conductors.setup_app("proofpoll", [&dna]).await.unwrap();
    conductors.exchange_peer_info().await;
    let ((alice,), (bob,)) = apps.into_tuples();
    let alice_zome = alice.zome(ZOME);
    let bob_zome = bob.zome(ZOME);
    let a = conductors.get(0).unwrap();
    let b = conductors.get(1).unwrap();

    let poll_hash: ActionHash = a
        .call(&alice_zome, "create_poll", poll_input("Gossip?", &["Yes", "No"]))
        .await;
    await_consistency([&alice, &bob]).await.expect("poll reaches bob");
    let seen: Vec<Record> = b.call(&bob_zome, "get_all_polls", ()).await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].action_address(), &poll_hash);

    let _: ActionHash = b
        .call(
            &bob_zome,
            "cast_vote",
            CastVoteInput {
                poll_action_hash: poll_hash.clone(),
                option_index: 0,
                display_name: None,
                profile_picture: None,
            },
        )
        .await;
    await_consistency([&alice, &bob]).await.expect("vote reaches alice");
    let votes: Vec<Record> = a.call(&alice_zome, "get_poll_votes", poll_hash.clone()).await;
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].action().author(), bob.agent_pubkey());

    let not_author: Result<ActionHash, _> = b.call_fallible(&bob_zome, "delete_poll", poll_hash).await;
    assert!(not_author.is_err(), "only the poll's author deletes it");

    // Carried records validate on the other conductor too (remote
    // validation of the proof and of the mapping a vote names).
    let fixtures = carried_records();
    let v13_vote = of_kind(&fixtures.v13, VOTE)[0].clone();
    let original: polls_integrity::VoteFields = polls_integrity::original_fields(&carried(&v13_vote, None)).unwrap();
    let its_poll = of_kind(&fixtures.v13, POLL)
        .into_iter()
        .find(|p| ActionHashB64::from_b64_str(&p.action_hash).map(ActionHash::from).ok() == Some(original.poll_action_hash.clone()))
        .unwrap();
    let copy: ActionHash = a.call(&alice_zome, "carry_poll", CarryInput { carried: carried(&its_poll, None) }).await;
    let mapping: Option<ActionHash> = a.call(&alice_zome, "get_migration_record", original.poll_action_hash.clone()).await;
    let _: ActionHash = a.call(&alice_zome, "carry_vote", CarryInput { carried: carried(&v13_vote, mapping) }).await;
    await_consistency([&alice, &bob]).await.expect("carried records reach bob");
    let votes: Vec<Record> = b.call(&bob_zome, "get_poll_votes", copy).await;
    assert_eq!(votes.len(), 1, "bob validated the carried poll and vote");
}
