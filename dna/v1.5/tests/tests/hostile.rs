//! A stranger's own coordinator (`tests/hostile`, test only) beside the
//! real one: same DNA hash, same network. On v1.5 the integrity rules refuse
//! each write it tries, so the writer's own conductor refuses to commit it.
//!
//! Build first, with build.sh's RUSTFLAGS: `./build.sh` in `v1.5`, then
//! `cargo build --release --target wasm32-unknown-unknown` in
//! `tests/hostile` (its own target folder: building it into the DNA's would
//! leave a differently built integrity crate where `build.sh` picks it up).

use holochain::prelude::*;
use holochain::sweettest::*;
use polls_coordinator::{CastVoteInput, CreateEncryptedEntryInput, CreatePollInput, FlagPollInput};
use polls_integrity::{FlagReason, PollType};
use std::path::Path;

const ZOME: &str = "polls";

/// The bundle, with `hostile` added beside its coordinators when asked
/// (coordinators are not part of the DNA hash).
async fn dna(hostile: bool) -> DnaFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../workdir/proofpoll_v1_5.dna");
    let mut dna = SweetDnaFile::from_bundle(&path).await.expect("v1.5/workdir/proofpoll_v1_5.dna loads (run build.sh first)");
    if hostile {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("hostile/target/wasm32-unknown-unknown/release/polls_hostile_test.wasm");
        let wasm = DnaWasm::from(std::fs::read(&file).expect("polls_hostile_test.wasm is built"));
        let hash = holo_hash::WasmHash::with_data(&wasm).await;
        let def = ZomeDef::Wasm(WasmZomeDef::new(hash, Some(vec!["polls_integrity".into()])));
        dna.update_coordinators(vec![(ZomeName::from("hostile"), def.into())], vec![wasm]).await.unwrap();
    }
    dna
}

fn poll_input(title: &str) -> CreatePollInput {
    CreatePollInput {
        title: title.to_string(),
        description: "test".to_string(),
        options: vec!["Yes".to_string(), "No".to_string()],
        closes_at: None,
        poll_type: PollType::Anonymous,
    }
}

fn vote_input(poll: &ActionHash) -> CastVoteInput {
    CastVoteInput { poll_action_hash: poll.clone(), option_index: 0, display_name: None, profile_picture: None }
}

#[derive(serde::Serialize, Debug)]
struct FileInput {
    base: ActionHash,
    target: ActionHash,
}

#[derive(serde::Serialize, Debug)]
struct DraftInput {
    agent: AgentPubKey,
    entry: ActionHash,
}

#[tokio::test(flavor = "multi_thread")]
async fn a_strangers_writes_are_refused() {
    let (ours, hostile) = (dna(false).await, dna(true).await);
    assert_eq!(ours.dna_hash(), hostile.dna_hash());
    let mut conductor = SweetConductor::standard().await;
    let alice = conductor.setup_app("alice", [&ours]).await.unwrap().into_cells().remove(0);
    let mallory = conductor.setup_app("mallory", [&hostile]).await.unwrap().into_cells().remove(0);
    let (az, mz, mh) = (alice.zome(ZOME), mallory.zome(ZOME), mallory.zome("hostile"));

    let poll: ActionHash = conductor.call(&az, "create_poll", poll_input("Alice's")).await;
    let vote: ActionHash = conductor.call(&az, "cast_vote", vote_input(&poll)).await;
    let flag: ActionHash = conductor.call(&az, "flag_poll", FlagPollInput { poll_action_hash: poll.clone(), reason: FlagReason::OffTopic }).await;
    let draft: ActionHash = conductor
        .call(&az, "create_encrypted_entry", CreateEncryptedEntryInput { cipher: vec![7; 48], nonce: vec![1; 24], link_as: "draft_poll".into(), related_hash: None })
        .await;
    let mpoll: ActionHash = conductor.call(&mz, "create_poll", poll_input("Mallory's")).await;
    let mvote: ActionHash = conductor.call(&mz, "cast_vote", vote_input(&mpoll)).await;
    let mentry: ActionHash = conductor.call(&mh, "own_entry", ()).await;
    await_consistency([&alice, &mallory]).await.expect("both see each other's records");

    async fn refused<I: serde::Serialize + std::fmt::Debug>(conductor: &SweetConductor, zome: &SweetZome, f: &str, input: I, expect: &str) {
        let r: Result<serde::de::IgnoredAny, _> = conductor.call_fallible(zome, f, input).await;
        let e = format!("{:?}", r.expect_err(f));
        assert!(e.contains(expect), "{f}: expected \"{expect}\", got {e}");
    }
    // Deletes and updates.
    refused(&conductor, &mh, "delete_record", poll.clone(), "Only a record's author can delete it").await;
    refused(&conductor, &mh, "delete_record", flag.clone(), "Only a record's author can delete it").await;
    refused(&conductor, &mh, "delete_record", draft.clone(), "Only a record's author can delete it").await;
    refused(&conductor, &mh, "delete_record", vote.clone(), "never deleted").await;
    refused(&conductor, &mh, "delete_record", mvote.clone(), "never deleted").await;
    refused(&conductor, &mh, "update_poll", poll.clone(), "never updated").await;
    refused(&conductor, &mh, "update_poll", mpoll.clone(), "never updated").await;
    // Misfiled links.
    refused(&conductor, &mh, "link_poll", poll.clone(), "filed by its own author").await;
    refused(&conductor, &mh, "link_vote", FileInput { base: mpoll.clone(), target: vote.clone() }, "filed under the poll it names").await;
    refused(&conductor, &mh, "link_vote", FileInput { base: poll.clone(), target: mvote.clone() }, "filed under the poll it names").await;
    refused(&conductor, &mh, "link_vote", FileInput { base: poll.clone(), target: vote.clone() }, "filed by its own author").await;
    refused(&conductor, &mh, "link_vote", FileInput { base: mpoll.clone(), target: mpoll.clone() }, "points at a vote").await;
    refused(&conductor, &mh, "link_reason", FileInput { base: vote.clone(), target: mentry.clone() }, "filed under its author's own vote").await;
    refused(&conductor, &mh, "link_reason", FileInput { base: mvote.clone(), target: draft.clone() }, "filed by its own author").await;
    refused(&conductor, &mh, "link_draft", DraftInput { agent: alice.agent_pubkey().clone(), entry: mentry.clone() }, "filed under its own author's anchor").await;
    refused(&conductor, &mh, "link_draft", DraftInput { agent: mallory.agent_pubkey().clone(), entry: draft.clone() }, "filed by its own author").await;
    // Link removals.
    refused(&conductor, &mh, "remove_poll_links", (), "Only a link's author can remove it").await;
    refused(&conductor, &mh, "remove_flag_links", poll.clone(), "Only a link's author can remove it").await;
    refused(&conductor, &mh, "remove_vote_links", mpoll.clone(), "never removed").await;

    // Alice reads everything as she wrote it.
    let polls: Vec<Record> = conductor.call(&az, "get_all_polls", ()).await;
    assert_eq!(polls.len(), 2, "both polls listed once");
    let votes: Vec<Record> = conductor.call(&az, "get_poll_votes", poll.clone()).await;
    assert_eq!(votes.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![vote]);
    let flags: Vec<Record> = conductor.call(&az, "get_poll_flags", poll.clone()).await;
    assert_eq!(flags.len(), 1);
    let drafts: Vec<Record> = conductor.call(&az, "get_my_drafts", ()).await;
    assert_eq!(drafts.iter().map(|r| r.action_address().clone()).collect::<Vec<_>>(), vec![draft]);
}
