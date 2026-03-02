/**
 * Holochain zome call helpers for ProofPoll.
 *
 * Thin wrappers around Tauri invoke() — all zome calls go through the Rust
 * backend. No @holochain/client in the frontend.
 */

import { invoke } from "@tauri-apps/api/core";

// Types matching the Rust response types

export interface Poll {
  title: string;
  description: string;
  options: string[];
  created_at: number;
  closes_at: number | null;
}

export interface PollListItem {
  hash: string;
  poll: Poll;
  author: string;
}

export interface PollDetail {
  poll: Poll;
  author: string;
}

export interface VoteData {
  vote: { poll_action_hash: string; option_index: number };
  author: string;
}

// Poll operations

export async function createPoll(input: {
  title: string;
  description: string;
  options: string[];
  closes_at: number | null;
}): Promise<string> {
  return invoke<string>("create_poll", input);
}

export async function getPoll(actionHash: string): Promise<PollDetail | null> {
  return invoke<PollDetail | null>("get_poll", { actionHash });
}

export async function getAllPolls(): Promise<PollListItem[]> {
  return invoke<PollListItem[]>("get_all_polls");
}

export async function castVote(
  pollActionHash: string,
  optionIndex: number,
): Promise<string> {
  return invoke<string>("cast_vote", {
    pollActionHash,
    optionIndex,
  });
}

export async function getPollVotes(
  pollActionHash: string,
): Promise<VoteData[]> {
  return invoke<VoteData[]>("get_poll_votes", {
    pollActionHash,
  });
}

// Identity linking

export interface IdentityLinkData {
  vault_agent_pub_key: string;
  entry_action_hash: string;
  linked_at: number;
}

export async function commitIdentityLink(
  vaultAgentPubKey: string,
  vaultSignature: string,
): Promise<string> {
  return invoke<string>("commit_identity_link", {
    vaultAgentPubKey,
    vaultSignature,
  });
}

export async function getLinkedAgents(
  agentPubKey: string,
): Promise<string[]> {
  return invoke<string[]>("get_linked_agents", {
    agentPubKey,
  });
}

export async function getIdentityLink(): Promise<IdentityLinkData | null> {
  return invoke<IdentityLinkData | null>("get_identity_link");
}

export async function revokeIdentityLink(): Promise<void> {
  return invoke<void>("revoke_identity_link");
}
