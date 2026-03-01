use hdk::prelude::*;
use polls_integrity::*;

#[hdk_dependent_entry_types]
enum EntryZomes {
    Integrity(polls_integrity::EntryTypes),
}

// --- Input types ---

#[derive(Serialize, Deserialize, Debug)]
pub struct CreatePollInput {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub closes_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CastVoteInput {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
}

// --- Poll functions ---

#[hdk_extern]
pub fn create_poll(input: CreatePollInput) -> ExternResult<ActionHash> {
    let now = sys_time()?.as_seconds_and_nanos().0;

    let poll = Poll {
        title: input.title,
        description: input.description,
        options: input.options,
        created_at: now,
        closes_at: input.closes_at,
    };

    let action_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Poll(poll)))?;

    // Link from the AllPolls anchor to this poll
    let anchor = all_polls_anchor()?;
    create_link(anchor, action_hash.clone(), LinkTypes::AllPolls, ())?;

    Ok(action_hash)
}

#[hdk_extern]
pub fn get_poll(action_hash: ActionHash) -> ExternResult<Option<Record>> {
    get(action_hash, GetOptions::default())
}

#[hdk_extern]
pub fn get_all_polls(_: ()) -> ExternResult<Vec<Record>> {
    let anchor = all_polls_anchor()?;
    let links = get_links(
        LinkQuery::try_new(anchor, LinkTypes::AllPolls)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid poll link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}

// --- Vote functions ---

#[hdk_extern]
pub fn cast_vote(input: CastVoteInput) -> ExternResult<ActionHash> {
    // Fetch the poll to validate option_index
    let poll_record = get(input.poll_action_hash.clone(), GetOptions::default())?
        .ok_or(wasm_error!("Poll not found"))?;

    let poll: Poll = poll_record
        .entry()
        .to_app_option()
        .map_err(|_| wasm_error!("Could not deserialize poll"))?
        .ok_or(wasm_error!("Poll entry is None"))?;

    if input.option_index as usize >= poll.options.len() {
        return Err(wasm_error!("Invalid option index"));
    }

    // Check if poll is closed
    if let Some(closes_at) = poll.closes_at {
        let now = sys_time()?.as_seconds_and_nanos().0;
        if now > closes_at {
            return Err(wasm_error!("Poll is closed"));
        }
    }

    // Check for double-vote: look at existing PollToVotes links
    let my_agent = agent_info()?.agent_initial_pubkey;
    let existing_links = get_links(
        LinkQuery::try_new(input.poll_action_hash.clone(), LinkTypes::PollToVotes)?,
        GetStrategy::default(),
    )?;

    for link in &existing_links {
        if link.author == my_agent {
            return Err(wasm_error!("You have already voted on this poll"));
        }
    }

    // Create the vote entry
    let vote = Vote {
        poll_action_hash: input.poll_action_hash.clone(),
        option_index: input.option_index,
    };

    let vote_hash = create_entry(&EntryZomes::Integrity(EntryTypes::Vote(vote)))?;

    // Link from poll to vote
    create_link(
        input.poll_action_hash,
        vote_hash.clone(),
        LinkTypes::PollToVotes,
        (),
    )?;

    Ok(vote_hash)
}

/// Returns all vote records for a given poll.
#[hdk_extern]
pub fn get_poll_votes(poll_action_hash: ActionHash) -> ExternResult<Vec<Record>> {
    let links = get_links(
        LinkQuery::try_new(poll_action_hash, LinkTypes::PollToVotes)?,
        GetStrategy::default(),
    )?;

    let mut records = Vec::new();
    for link in links {
        let hash = ActionHash::try_from(link.target)
            .map_err(|_| wasm_error!("Invalid vote link target"))?;
        if let Some(record) = get(hash, GetOptions::default())? {
            records.push(record);
        }
    }

    Ok(records)
}
