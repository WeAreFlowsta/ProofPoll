//! Test harness only (feature `harness`): the poll operations the matrix
//! drives through `POST /dev/op`. `harness.rs` asks here first; an operation
//! this folder does not know is the harness's own (sign-in, backup, restore,
//! the chains). A fork replaces these with its own app's operations.

#![allow(unused_imports)]
use crate::commands::{self, AppState};
use crate::harness::{failed, ok, opt_s, s, to_value, Reply};
use crate::polls;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Manager;

/// The reply to a poll operation, or None when `body` names another one.
pub(crate) async fn harness_op(app: &tauri::AppHandle, body: &Value) -> Option<Reply> {
    let state = app.state::<Arc<AppState>>();
    let options = || -> Vec<String> {
        body.get("options")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let closes_at = body.get("closes_at").and_then(|v| v.as_i64());
    Some(match s(body, "op").as_str() {
        "create_poll" => to_value(
            polls::create_poll(state, s(body, "title"), s(body, "description"), options(), closes_at, opt_s(body, "poll_type")).await,
        ),
        "polls" => to_value(polls::get_all_polls(state).await),
        "poll" => to_value(polls::get_poll(state, s(body, "poll")).await),
        "vote" => to_value(
            polls::cast_vote(
                state,
                s(body, "poll"),
                body.get("option").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                opt_s(body, "dna_version").unwrap_or_else(|| "1.5".into()),
                opt_s(body, "poll_type"),
            )
            .await,
        ),
        "votes" => to_value(polls::get_poll_votes(state, s(body, "poll"), opt_s(body, "dna_version").unwrap_or_else(|| "1.5".into())).await),
        "save_draft" => to_value(
            polls::save_draft_poll(state, s(body, "title"), s(body, "description"), options(), closes_at, opt_s(body, "poll_type")).await,
        ),
        "drafts" => to_value(polls::get_my_drafts(state).await),
        "publish_draft" => to_value(polls::publish_draft(state, s(body, "draft")).await),
        "save_rationale" => to_value(polls::save_vote_rationale(state, s(body, "vote"), s(body, "text")).await),
        "rationale" => to_value(polls::get_vote_rationale(state, s(body, "vote")).await),
        // ProofPoll 0.4.x's shapes on v1.3, for the migration leg.
        "v13_poll" => to_value(polls::harness_v13_poll(&state, s(body, "title"), s(body, "description"), options(), closes_at, opt_s(body, "poll_type")).await),
        "v13_flag" => to_value(polls::harness_v13_flag(&state, s(body, "poll"), opt_s(body, "reason").unwrap_or_else(|| "Spam".into())).await),
        "v13_delete" => to_value(polls::harness_v13_remove(&state, "delete_poll", s(body, "poll")).await),
        "v13_unflag" => to_value(polls::harness_v13_remove(&state, "remove_flag", s(body, "flag")).await),
        "v13_draft" => {
            let draft = json!({
                "title": s(body, "title"),
                "description": s(body, "description"),
                "options": options(),
                "closes_at": closes_at,
                "poll_type": "Anonymous",
            });
            to_value(polls::harness_v13_encrypted(&state, "draft_poll", None, draft.to_string().into_bytes()).await)
        }
        "v13_rationale" => to_value(
            polls::harness_v13_encrypted(&state, "vote_rationale", Some(s(body, "vote")), s(body, "text").into_bytes()).await,
        ),
        "v13_vote" => to_value(
            polls::harness_v13_vote(&state, s(body, "poll"), body.get("option").and_then(|v| v.as_u64()).unwrap_or(0) as u32, opt_s(body, "display_name")).await,
        ),
        _ => return None,
    })
}
