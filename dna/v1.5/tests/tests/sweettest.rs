//! ProofPoll DNA v1.5 on a real Holochain 0.7.1 conductor.
//!
//! Loads the packed bundle from `../workdir/proofpoll_v1_5.dna`, so a passing
//! run says the committed artifact works on this conductor: every native
//! validation arm, carried records checked against real v1.3 and v1.4
//! signatures (`fixtures/carried_records.json`), every forgery refused, and
//! two conductors gossiping native and carried records.

use holo_hash::{ActionHashB64, AgentPubKeyB64};
use holochain::prelude::*;
use holochain::sweettest::*;
use polls_coordinator::{CarryInput, CastVoteInput, CreateEncryptedEntryInput, CreatePollInput, FlagPollInput};
use polls_integrity::{Carried, FlagReason, Poll, PollType, Vote};
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
