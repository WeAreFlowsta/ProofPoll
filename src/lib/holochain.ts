/**
 * Holochain zome call helpers for ProofPoll.
 *
 * Thin wrappers around Tauri invoke() — all zome calls go through the Rust
 * backend. No @holochain/client in the frontend.
 *
 * ## For forking developers
 *
 * This file has four sections:
 *   1. **App types + functions** (top) — Poll, Vote, Flag types and their
 *      invoke() wrappers. Replace these with your own data model.
 *   2. **Identity linking** — Flowsta integration. Keep as-is.
 *   3. **Flagging** — Community moderation. Keep or adapt.
 *   4. **Migration status** — DNA version upgrade tracking. Keep as-is.
 *
 * Each function is a one-liner that calls the matching Tauri command in
 * `src-tauri/src/commands.rs`. Add/remove functions as you add/remove commands.
 */

import { invoke } from "@tauri-apps/api/core";

// ── App-specific types (replace with your data model) ─────────────────

export type PollType = "Anonymous" | "Public";

export interface Poll {
  title: string;
  description: string;
  options: string[];
  created_at: number;
  closes_at: number | null;
  /** "Anonymous" or "Public". Null for pre-v1.2 polls (treated as Anonymous). */
  poll_type: PollType | null;
}

export interface PollListItem {
  hash: string;
  poll: Poll;
  author: string;
  /** Which DHT this poll lives on. Pass back to castVote and getPollVotes. */
  dna_version: "1.0" | "1.1" | "1.2";
}

export interface PollDetail {
  poll: Poll;
  author: string;
  /** Which DHT this poll lives on. Pass back to castVote and getPollVotes. */
  dna_version: "1.0" | "1.1" | "1.2";
}

export interface VoteData {
  vote: { poll_action_hash: string; option_index: number };
  author: string;
  /** Set on public v1.2 polls. Null otherwise. */
  display_name: string | null;
  /** Set on public v1.2 polls. Null otherwise. */
  profile_picture: string | null;
}

// ── App-specific operations (replace with your invoke() wrappers) ─────

export async function createPoll(input: {
  title: string;
  description: string;
  options: string[];
  closes_at: number | null;
  poll_type: PollType;
}): Promise<string> {
  return invoke<string>("create_poll", input);
}

export async function getPoll(actionHash: string): Promise<PollDetail | null> {
  return invoke<PollDetail | null>("get_poll", { actionHash });
}

export async function getAllPolls(): Promise<PollListItem[]> {
  return invoke<PollListItem[]>("get_all_polls");
}

export async function deletePoll(actionHash: string): Promise<string> {
  return invoke<string>("delete_poll", { actionHash });
}

export async function castVote(
  pollActionHash: string,
  optionIndex: number,
  dnaVersion: "1.0" | "1.1" | "1.2",
  pollType?: PollType,
): Promise<string> {
  return invoke<string>("cast_vote", {
    pollActionHash,
    optionIndex,
    dnaVersion,
    pollType: pollType ?? null,
  });
}

export async function getPollVotes(
  pollActionHash: string,
  dnaVersion: "1.0" | "1.1" | "1.2",
): Promise<VoteData[]> {
  return invoke<VoteData[]>("get_poll_votes", {
    pollActionHash,
    dnaVersion,
  });
}

// ── Profile cache (Flowsta infrastructure — keep as-is) ───────────────
//
// Caches the user's display name and profile picture locally so the app
// works without the Flowsta Vault running. The Vault is only needed for
// the initial identity linking ceremony. See layout.tsx for the load/save flow.

export interface CachedProfile {
  display_name: string | null;
  profile_picture: string | null;
}

export async function getCachedProfile(): Promise<CachedProfile | null> {
  return invoke<CachedProfile | null>("get_cached_profile");
}

export async function saveProfileCache(
  displayName: string | null,
  profilePicture: string | null,
): Promise<void> {
  return invoke<void>("save_profile_cache", {
    displayName,
    profilePicture,
  });
}

// ── Identity linking (Flowsta infrastructure — keep as-is) ────────────

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

// ── Flagging (community moderation — keep or adapt) ───────────────────

export type FlagReason = "Spam" | "Misleading" | "OffTopic" | "Inappropriate";

export interface FlagData {
  hash: string;
  flag: { poll_action_hash: string; reason: string; created_at: number };
  author: string;
}

export async function flagPoll(
  pollActionHash: string,
  reason: FlagReason,
): Promise<string> {
  return invoke<string>("flag_poll", { pollActionHash, reason });
}

export async function getPollFlags(
  pollActionHash: string,
): Promise<FlagData[]> {
  return invoke<FlagData[]>("get_poll_flags", { pollActionHash });
}

export async function removeFlag(flagActionHash: string): Promise<string> {
  return invoke<string>("remove_flag", { flagActionHash });
}

export async function getFlagThreshold(): Promise<number> {
  return invoke<number>("get_flag_threshold");
}

// ── Migration status (infrastructure — keep as-is) ────────────────────

export interface MigrationState {
  status: "NotStarted" | "InProgress" | "Complete" | { Error: string };
  polls_migrated: { old_hash: string; new_hash: string; title: string }[];
  votes_pending: {
    v1_0_poll_hash: string;
    option_index: number;
    poll_title: string;
    retry_count: number;
  }[];
  votes_migrated: {
    old_poll_hash: string;
    new_poll_hash: string;
    option_index: number;
  }[];
}

export async function getMigrationStatus(): Promise<MigrationState> {
  return invoke<MigrationState>("get_migration_status");
}

export async function abandonPendingVotes(): Promise<void> {
  return invoke<void>("abandon_pending_votes");
}
