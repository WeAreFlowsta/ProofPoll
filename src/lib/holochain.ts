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
  dna_version: "1.0" | "1.1" | "1.2" | "1.3";
}

export interface PollDetail {
  poll: Poll;
  author: string;
  /** Which DHT this poll lives on. Pass back to castVote and getPollVotes. */
  dna_version: "1.0" | "1.1" | "1.2" | "1.3";
}

export interface VoteData {
  vote: { hash: string; poll_action_hash: string; option_index: number };
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
  // Tauri v2 maps camelCase from JS → snake_case in Rust, so we must send
  // camelCase keys even though the TypeScript interface uses snake_case.
  return invoke<string>("create_poll", {
    title: input.title,
    description: input.description,
    options: input.options,
    closesAt: input.closes_at,
    pollType: input.poll_type,
  });
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

/**
 * The set of Holochain agent keys that all belong to THIS user — used to
 * recognise the user's own polls/votes/flags regardless of which device or
 * install authored them.
 *
 * Why a set, not a single key: ProofPoll generates a fresh conductor agent
 * key on every install. The user's stable identity is their Flowsta Vault
 * agent; each install links its local agent to that Vault agent via an
 * IsSamePerson attestation. `get_linked_agents(vaultAgent)` therefore returns
 * every ProofPoll agent the user has ever linked (this is a designed-in query
 * — the agent-linking zome indexes the link from the Vault agent's pubkey too).
 *
 * IMPORTANT: this is for RECOGNITION (read) only. Mutating an entry
 * (delete a poll, remove a flag) is still bound to the CURRENT local agent —
 * Holochain only lets the original author update/delete, so a different linked
 * agent cannot. Use the local agent directly for those gates, not this set.
 *
 * Best-effort: if the user has never linked (fresh, not signed in) or the
 * Vault link isn't available, the set is just the local agent.
 */
export async function loadMyAgentSet(
  localAgent: string | null,
): Promise<Set<string>> {
  const set = new Set<string>();
  if (localAgent) set.add(localAgent);
  try {
    const link = await getIdentityLink();
    if (link?.vault_agent_pub_key) {
      const siblings = await getLinkedAgents(link.vault_agent_pub_key);
      for (const a of siblings) set.add(a);
    }
  } catch {
    // Not linked / conductor not ready / offline — local agent only.
  }
  return set;
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

// ── Encrypted entries (v1.3) ──────────────────────────────────────────

export interface DraftPollItem {
  hash: string;
  title: string;
  description: string;
  options: string[];
  closes_at: number | null;
  poll_type: string;
  created_at: number;
}

export async function saveVoteRationale(
  voteActionHash: string,
  rationaleText: string,
): Promise<string> {
  return invoke<string>("save_vote_rationale", {
    voteActionHash,
    rationaleText,
  });
}

export async function getVoteRationale(
  voteActionHash: string,
): Promise<string | null> {
  return invoke<string | null>("get_vote_rationale", { voteActionHash });
}

export async function saveDraftPoll(input: {
  title: string;
  description: string;
  options: string[];
  closes_at: number | null;
  poll_type: PollType;
}): Promise<string> {
  return invoke<string>("save_draft_poll", {
    title: input.title,
    description: input.description,
    options: input.options,
    closesAt: input.closes_at,
    pollType: input.poll_type,
  });
}

export async function getMyDrafts(): Promise<DraftPollItem[]> {
  return invoke<DraftPollItem[]>("get_my_drafts");
}

export async function publishDraft(draftActionHash: string): Promise<string> {
  return invoke<string>("publish_draft", { draftActionHash });
}

export async function deleteDraft(draftActionHash: string): Promise<string> {
  return invoke<string>("delete_draft", { draftActionHash });
}
