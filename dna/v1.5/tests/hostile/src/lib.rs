//! Test only. What a stranger can write against the polls integrity zome
//! with a coordinator of their own: the network checks no author on a
//! delete, an update or a link removal, and none on who files what where.
//! The sweettests run this beside the real coordinator; on v1.5 the
//! integrity rules refuse each write. `carry_again` is the one valid write
//! here: a second carrier's copy, for the readers' test.

use hdk::prelude::*;
use polls_integrity::*;

#[hdk_dependent_entry_types]
enum EntryZomes {
    Integrity(polls_integrity::EntryTypes),
}

/// Delete someone else's record (a poll, a vote, a flag, a mapping).
#[hdk_extern]
pub fn delete_record(hash: ActionHash) -> ExternResult<ActionHash> {
    delete_entry(hash)
}

/// Rewrite someone else's poll by updating it.
#[hdk_extern]
pub fn update_poll(hash: ActionHash) -> ExternResult<ActionHash> {
    let poll = Poll {
        title: "Rewritten".into(),
        description: String::new(),
        options: vec!["Yes".into(), "No".into()],
        created_at: sys_time()?.as_seconds_and_nanos().0,
        closes_at: None,
        poll_type: PollType::Anonymous,
        carried_from: None,
    };
    update_entry(hash, &EntryZomes::Integrity(EntryTypes::Poll(poll)))
}

/// List someone else's poll under the polls anchor again.
#[hdk_extern]
pub fn link_poll(poll: ActionHash) -> ExternResult<ActionHash> {
    create_link(all_polls_anchor()?, poll, LinkTypes::AllPolls, ())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileInput {
    pub base: ActionHash,
    pub target: ActionHash,
}

/// File a vote under a poll.
#[hdk_extern]
pub fn link_vote(input: FileInput) -> ExternResult<ActionHash> {
    create_link(input.base, input.target, LinkTypes::PollToVotes, ())
}

/// File an encrypted entry as a vote's reason.
#[hdk_extern]
pub fn link_reason(input: FileInput) -> ExternResult<ActionHash> {
    create_link(input.base, input.target, LinkTypes::VoteToRationale, ())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DraftInput {
    pub agent: AgentPubKey,
    pub entry: ActionHash,
}

/// File an encrypted entry among someone else's drafts.
#[hdk_extern]
pub fn link_draft(input: DraftInput) -> ExternResult<ActionHash> {
    create_link(agent_drafts_anchor(&input.agent)?, input.entry, LinkTypes::AgentDrafts, ())
}

/// An encrypted entry of one's own, to file somewhere.
#[hdk_extern]
pub fn own_entry(_: ()) -> ExternResult<ActionHash> {
    let entry = EncryptedEntry { cipher: vec![1; 32], nonce: vec![2; 24], entry_type_hint: "private".into(), related_hash: None };
    create_entry(&EntryZomes::Integrity(EntryTypes::EncryptedEntry(entry)))
}

fn remove_all(base: AnyLinkableHash, link_type: LinkTypes) -> ExternResult<u32> {
    let links = get_links(LinkQuery::try_new(base, link_type)?, GetStrategy::Network)?;
    for link in &links {
        delete_link(link.create_link_hash.clone(), GetOptions::default())?;
    }
    Ok(links.len() as u32)
}

/// Take every poll off the polls anchor.
#[hdk_extern]
pub fn remove_poll_links(_: ()) -> ExternResult<u32> {
    remove_all(all_polls_anchor()?.into(), LinkTypes::AllPolls)
}

/// Take a poll's votes off it.
#[hdk_extern]
pub fn remove_vote_links(poll: ActionHash) -> ExternResult<u32> {
    remove_all(poll.into(), LinkTypes::PollToVotes)
}

/// Take a poll's flags off it.
#[hdk_extern]
pub fn remove_flag_links(poll: ActionHash) -> ExternResult<u32> {
    remove_all(poll.into(), LinkTypes::PollToFlags)
}

/// A second copy of a carried poll, written as `carry_poll` writes one but
/// without its look for a copy already there: what two carriers at once
/// leave. Returns the copy and its mapping.
#[hdk_extern]
pub fn carry_again(carried: Carried) -> ExternResult<(ActionHash, ActionHash)> {
    let f: PollFields = original_fields(&carried).map_err(|e| wasm_error!(e))?;
    let old_action_hash = carried.original_action_hash.clone();
    let poll = Poll {
        title: f.title,
        description: f.description,
        options: f.options,
        created_at: f.created_at,
        closes_at: f.closes_at,
        poll_type: f.poll_type,
        carried_from: Some(carried),
    };
    let poll_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Poll(poll)))?;
    create_link(all_polls_anchor()?, poll_hash.clone(), LinkTypes::AllPolls, ())?;
    let mapping = MigratedPoll { old_action_hash, new_action_hash: poll_hash.clone(), migrated_at: sys_time()?.as_seconds_and_nanos().0, via: None };
    let mapping_hash = create_entry(&EntryZomes::Integrity(EntryTypes::MigratedPoll(mapping)))?;
    create_link(migration_anchor()?, mapping_hash.clone(), LinkTypes::MigrationIndex, ())?;
    Ok((poll_hash, mapping_hash))
}
