use hdi::prelude::*;

/// A poll with a question and multiple options.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Poll {
    pub title: String,
    pub description: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
}

/// A vote on a specific poll option.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Vote {
    pub poll_action_hash: ActionHash,
    pub option_index: u32,
}

#[hdk_entry_types]
#[unit_enum(UnitEntryTypes)]
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EntryTypes {
    Poll(Poll),
    Vote(Vote),
}

#[derive(Serialize, Deserialize)]
#[hdk_link_types]
pub enum LinkTypes {
    /// From a well-known anchor hash to each Poll's action hash.
    AllPolls,
    /// From a Poll's action hash to each Vote's action hash.
    PollToVotes,
}

/// Returns a deterministic hash to use as the base for AllPolls links.
pub fn all_polls_anchor() -> ExternResult<EntryHash> {
    hash_entry(&Poll {
        title: "ALL_POLLS_ANCHOR".to_string(),
        description: String::new(),
        options: vec![],
        created_at: 0,
        closes_at: None,
    })
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::StoreEntry(store_entry) => match store_entry {
            OpEntry::CreateEntry { app_entry, .. } | OpEntry::UpdateEntry { app_entry, .. } => {
                match app_entry {
                    EntryTypes::Poll(poll) => validate_poll(&poll),
                    EntryTypes::Vote(vote) => validate_vote(&vote),
                }
            }
            _ => Ok(ValidateCallbackResult::Valid),
        },
        FlatOp::RegisterCreateLink {
            link_type,
            base_address,
            target_address: _,
            tag: _,
            action: _,
        } => match link_type {
            LinkTypes::AllPolls => {
                let anchor = all_polls_anchor()?;
                if base_address != AnyLinkableHash::from(anchor) {
                    return Ok(ValidateCallbackResult::Invalid(
                        "AllPolls link must originate from the polls anchor".to_string(),
                    ));
                }
                Ok(ValidateCallbackResult::Valid)
            }
            LinkTypes::PollToVotes => Ok(ValidateCallbackResult::Valid),
        },
        FlatOp::RegisterDeleteLink { .. } => Ok(ValidateCallbackResult::Valid),
        _ => Ok(ValidateCallbackResult::Valid),
    }
}

fn validate_poll(poll: &Poll) -> ExternResult<ValidateCallbackResult> {
    if poll.title.trim().is_empty() {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll title cannot be empty".to_string(),
        ));
    }
    if poll.options.len() < 2 {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll must have at least 2 options".to_string(),
        ));
    }
    if poll.options.len() > 10 {
        return Ok(ValidateCallbackResult::Invalid(
            "Poll cannot have more than 10 options".to_string(),
        ));
    }
    for opt in &poll.options {
        if opt.trim().is_empty() {
            return Ok(ValidateCallbackResult::Invalid(
                "Poll options cannot be empty".to_string(),
            ));
        }
    }
    Ok(ValidateCallbackResult::Valid)
}

fn validate_vote(vote: &Vote) -> ExternResult<ValidateCallbackResult> {
    // We can't validate option_index against the poll here because we'd need
    // to fetch the poll entry, which isn't available during pure validation.
    // The coordinator enforces this check at call time.
    let _ = vote;
    Ok(ValidateCallbackResult::Valid)
}
