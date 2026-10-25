//! ProofPoll DNA v1.4 on a real Holochain 0.7.1 conductor.
//!
//! Loads the packed bundle from `../workdir/proofpoll_v1_4.dna` (the bytes
//! the app ships), so a passing run says the committed artifact works on
//! this conductor: every validation arm, the migration round-trip fed with a
//! v1.3 export, and two conductors gossiping a poll and a vote.

use holo_hash::{ActionHashB64, AgentPubKeyB64};
use holochain::prelude::*;
use holochain::sweettest::*;
use polls_coordinator::{
    CastVoteInput, CreateEncryptedEntryInput, CreatePollInput, FlagPollInput,
    RegisterMigratedPollInput,
};
use polls_integrity::{FlagReason, MigratedPoll, Poll, PollType};
use proofpoll_v1_4_tests::v1_3_export;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const ZOME: &str = "polls";

async fn dna() -> DnaFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../workdir/proofpoll_v1_4.dna");
    SweetDnaFile::from_bundle(&path)
        .await
        .expect("dna/v1.4/workdir/proofpoll_v1_4.dna loads (run build.sh first)")
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

fn now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

fn decode<T: TryFrom<SerializedBytes, Error = SerializedBytesError>>(record: &Record) -> T {
    record
        .entry()
        .to_app_option::<T>()
        .ok()
        .flatten()
        .expect("record carries the expected app entry")
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

    // MigratedPoll: same hashes, carried record newer than its migration, then valid
    let other: ActionHash = conductor
        .call(&zome, "create_poll", poll_input("Second", &["X", "Y"]))
        .await;
    let same: Result<ActionHash, _> = conductor
        .call_fallible(
            &zome,
            "register_migrated_poll",
            RegisterMigratedPollInput {
                old_action_hash: poll_hash.clone(),
                new_action_hash: poll_hash.clone(),
                original_author: None,
                original_timestamp: None,
                original_signed_action: None,
            },
        )
        .await;
    assert!(same.is_err());
    let future: Result<ActionHash, _> = conductor
        .call_fallible(
            &zome,
            "register_migrated_poll",
            RegisterMigratedPollInput {
                old_action_hash: poll_hash.clone(),
                new_action_hash: other.clone(),
                original_author: Some(app.agent().clone()),
                original_timestamp: Some(now_s() + 10 * 365 * 24 * 3600),
                original_signed_action: None,
            },
        )
        .await;
    assert!(future.is_err(), "validate_migrated_poll: carried poll newer than its migration");
    let ok: ActionHash = conductor
        .call(
            &zome,
            "register_migrated_poll",
            RegisterMigratedPollInput {
                old_action_hash: poll_hash.clone(),
                new_action_hash: other.clone(),
                original_author: Some(app.agent().clone()),
                original_timestamp: Some(poll.created_at),
                original_signed_action: None,
            },
        )
        .await;
    let mapped: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", poll_hash.clone()).await;
    assert_eq!(mapped, Some(other));
    let mappings: Vec<Record> = conductor.call(&zome, "get_all_migration_mappings", ()).await;
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].action_address(), &ok);
    let mp: MigratedPoll = decode(&mappings[0]);
    let from = mp.carried_from.expect("carried_from filled");
    assert_eq!(&from.witness, app.agent());
    assert_eq!(&from.original_author, app.agent());
    assert!(from.certificate.is_none());

    // Delete: only the author (the only agent here, so it succeeds)
    let _: ActionHash = conductor.call(&zome, "delete_poll", poll_hash.clone()).await;
}

/// The app's migration engine, fed with a v1.3 export: create on v1.4,
/// register the mapping with the carried origin, read it all back.
#[tokio::test(flavor = "multi_thread")]
async fn migration_round_trip_from_v1_3_export() {
    let export = v1_3_export();
    assert_eq!(export.network_seed, "proofpoll-network-v1.3");
    let mut conductor = SweetConductor::standard().await;
    let app = conductor.setup_app("proofpoll", [&dna().await]).await.unwrap();
    let zome = app.cells()[0].zome(ZOME);

    let mut expected = Vec::new();
    for (i, p) in export.polls.iter().enumerate() {
        let old: ActionHash = ActionHashB64::from_b64_str(&p.old_action_hash).unwrap().into();
        // Idempotency check the engine does first
        let before: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", old.clone()).await;
        assert!(before.is_none());

        let new_hash: ActionHash = conductor
            .call(
                &zome,
                "create_poll",
                CreatePollInput {
                    title: p.title.clone(),
                    description: p.description.clone(),
                    options: p.options.clone(),
                    closes_at: p.closes_at,
                    // Migrated polls default to Anonymous, as the app does
                    poll_type: PollType::Anonymous,
                },
            )
            .await;
        // The first poll carries its original signed action; the export has
        // none for the others (a network the agent could no longer read).
        let signed_action = (i == 0).then(|| vec![0xC4, 0x03, 1, 2, 3]);
        let _: ActionHash = conductor
            .call(
                &zome,
                "register_migrated_poll",
                RegisterMigratedPollInput {
                    old_action_hash: old.clone(),
                    new_action_hash: new_hash.clone(),
                    original_author: Some(app.agent().clone()),
                    original_timestamp: Some(p.created_at),
                    original_signed_action: signed_action.clone(),
                },
            )
            .await;
        expected.push((old, new_hash, p.created_at, signed_action));
    }

    for (old, new_hash, _, _) in &expected {
        let mapped: Option<ActionHash> = conductor.call(&zome, "get_migration_mapping", old.clone()).await;
        assert_eq!(mapped.as_ref(), Some(new_hash), "mapping for {old}");
    }
    let polls: Vec<Record> = conductor.call(&zome, "get_all_polls", ()).await;
    assert_eq!(polls.len(), export.polls.len());
    let mappings: Vec<Record> = conductor.call(&zome, "get_all_migration_mappings", ()).await;
    assert_eq!(mappings.len(), export.polls.len());
    for record in &mappings {
        let mp: MigratedPoll = decode(record);
        let (_, _, created_at, signed_action) = expected
            .iter()
            .find(|(old, _, _, _)| old == &mp.old_action_hash)
            .expect("every mapping is one we registered");
        let from = mp.carried_from.expect("carried_from filled from the export");
        assert_eq!(from.original_timestamp, *created_at);
        assert_eq!(from.original_signed_action, *signed_action);
        assert!(from.original_timestamp <= mp.migrated_at);
        assert_eq!(&from.witness, app.agent());
    }
    let agent_b64 = AgentPubKeyB64::from(app.agent().clone()).to_string();
    assert!(agent_b64.starts_with("uhCAk"), "agent keys keep their spelling on 0.7.1");
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
}
