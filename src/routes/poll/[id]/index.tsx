import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { Link, useLocation, useNavigate } from "@builder.io/qwik-city";
import { linkedContext } from "~/lib/context";
import { invoke } from "@tauri-apps/api/core";
import {
  getPoll,
  getPollVotes,
  castVote,
  deletePoll,
  getLinkedAgents,
  type Poll,
  type VoteData,
} from "~/lib/holochain";

interface VerifiedResults {
  verifiedVoteCount: number;
  verifiedCounts: number[];
  identityCount: number;
}

export default component$(() => {
  const linked = useContext(linkedContext);
  const loc = useLocation();
  const nav = useNavigate();
  const poll = useSignal<Poll | null>(null);
  const pollAuthor = useSignal<string | null>(null);
  const votes = useSignal<VoteData[]>([]);
  const myAgent = useSignal<string | null>(null);
  const selectedOption = useSignal<number | null>(null);
  const loading = useSignal(true);
  const voting = useSignal(false);
  const hasVoted = useSignal(false);
  const error = useSignal<string | null>(null);
  const voteError = useSignal<string | null>(null);
  const verified = useSignal<VerifiedResults | null>(null);
  const verifiedLoading = useSignal(false);
  const confirmDelete = useSignal(false);
  const deleting = useSignal(false);
  const deleteError = useSignal<string | null>(null);

  const pollHash = loc.params.id;

  const loadVerifiedResults = $(
    async (currentVotes: VoteData[], optionCount: number) => {
      if (currentVotes.length === 0) return;
      verifiedLoading.value = true;

      try {
        // Get unique voters.
        const voterKeys = new Map<string, VoteData>();
        for (const v of currentVotes) {
          if (!voterKeys.has(v.author)) {
            voterKeys.set(v.author, v);
          }
        }

        // For each voter, get linked agents (Vault keys).
        // Build: vaultKey -> [voterKey1, voterKey2, ...]
        const vaultToVoters = new Map<string, string[]>();
        const unlinkedVoters = new Set<string>();

        for (const [voterKey] of voterKeys) {
          try {
            const linked = await getLinkedAgents(voterKey);
            if (linked.length > 0) {
              // Use the first linked key as the identity cluster key.
              const vaultKey = linked[0];
              const existing = vaultToVoters.get(vaultKey) || [];
              existing.push(voterKey);
              vaultToVoters.set(vaultKey, existing);
            } else {
              unlinkedVoters.add(voterKey);
            }
          } catch {
            // If get_linked_agents fails, treat as unlinked.
            unlinkedVoters.add(voterKey);
          }
        }

        // Deduplicate: for each identity cluster, keep only the first vote.
        const deduplicatedVotes: VoteData[] = [];

        // Add one vote per identity cluster.
        for (const [, voters] of vaultToVoters) {
          const firstVote = currentVotes.find((v) =>
            voters.includes(v.author),
          );
          if (firstVote) {
            deduplicatedVotes.push(firstVote);
          }
        }

        // Add all unlinked votes (can't deduplicate without identity).
        for (const voterKey of unlinkedVoters) {
          const vote = currentVotes.find((v) => v.author === voterKey);
          if (vote) {
            deduplicatedVotes.push(vote);
          }
        }

        // Count per option.
        const verifiedCounts = Array.from({ length: optionCount }, (_, i) =>
          deduplicatedVotes.filter((v) => v.vote.option_index === i).length,
        );

        verified.value = {
          verifiedVoteCount: deduplicatedVotes.length,
          verifiedCounts,
          identityCount: vaultToVoters.size,
        };
      } catch (e) {
        console.error("Failed to compute verified results:", e);
      } finally {
        verifiedLoading.value = false;
      }
    },
  );

  useVisibleTask$(async () => {
    try {
      const status = await invoke<{ agent_pub_key: string | null }>(
        "get_app_status",
      );
      myAgent.value = status.agent_pub_key;

      const [pollResult, votesResult] = await Promise.all([
        getPoll(pollHash),
        getPollVotes(pollHash),
      ]);

      if (!pollResult) {
        error.value = "Poll not found";
        return;
      }

      poll.value = pollResult.poll;
      pollAuthor.value = pollResult.author;
      votes.value = votesResult;

      if (myAgent.value) {
        hasVoted.value = votesResult.some(
          (v) => v.author === myAgent.value,
        );
      }

      // Load verified results in the background.
      if (votesResult.length > 0) {
        loadVerifiedResults(votesResult, pollResult.poll.options.length);
      }
    } catch (e: any) {
      error.value = e.message || "Failed to load poll";
    } finally {
      loading.value = false;
    }
  });

  const submitVote = $(async () => {
    if (selectedOption.value === null) return;
    voteError.value = null;
    voting.value = true;

    try {
      await castVote(pollHash, selectedOption.value);

      const newVotes = await getPollVotes(pollHash);
      votes.value = newVotes;
      hasVoted.value = true;

      // Refresh verified results.
      if (poll.value) {
        loadVerifiedResults(newVotes, poll.value.options.length);
      }
    } catch (e: any) {
      voteError.value = e.message || "Failed to cast vote";
    } finally {
      voting.value = false;
    }
  });

  const confirmDeletePoll = $(async () => {
    deleting.value = true;
    deleteError.value = null;
    try {
      await deletePoll(pollHash);
      await nav("/");
    } catch (e: any) {
      deleteError.value = e.message || String(e) || "Failed to delete poll";
      deleting.value = false;
    }
  });

  if (loading.value) {
    return <div class="text-gray-400">Loading poll...</div>;
  }

  if (error.value) {
    return <div class="text-red-400">{error.value}</div>;
  }

  if (!poll.value) return null;

  const p = poll.value;
  const isOpen = !p.closes_at || p.closes_at > Date.now() / 1000;
  const totalVotes = votes.value.length;

  const voteCounts: number[] = p.options.map(
    (_, i) => votes.value.filter((v) => v.vote.option_index === i).length,
  );

  return (
    <div class="max-w-2xl mx-auto">
      <div class="mb-6">
        <div class="flex items-start justify-between mb-2">
          <h1 class="text-2xl font-bold">{p.title}</h1>
          <span
            class={`text-xs px-2 py-0.5 rounded ${
              isOpen
                ? "bg-green-900 text-green-300"
                : "bg-gray-800 text-gray-400"
            }`}
          >
            {isOpen ? "Open" : "Closed"}
          </span>
        </div>
        {p.description && (
          <p class="text-gray-400 mb-3">{p.description}</p>
        )}
        <div class="text-xs text-gray-500">
          {totalVotes} vote{totalVotes !== 1 ? "s" : ""}
          {verified.value && verified.value.identityCount > 0 && (
            <span>
              {" "}
              · {verified.value.identityCount} verified
            </span>
          )}
          {p.closes_at && (
            <span>
              {" "}
              · Closes {new Date(p.closes_at * 1000).toLocaleString()}
            </span>
          )}
        </div>

        {/* Delete button — only visible to poll creator */}
        {linked.value && myAgent.value && pollAuthor.value === myAgent.value && (
          <div class="mt-3">
            {!confirmDelete.value ? (
              <button
                type="button"
                onClick$={() => (confirmDelete.value = true)}
                class="text-red-400 hover:text-red-300 text-xs"
              >
                Delete poll
              </button>
            ) : (
              <div class="bg-red-900/20 border border-red-800 rounded-lg p-3">
                <p class="text-sm text-gray-300 mb-3">
                  Are you sure you want to delete this poll? This cannot be undone.
                </p>
                {deleteError.value && (
                  <div class="text-red-300 text-sm mb-2">{deleteError.value}</div>
                )}
                <div class="flex gap-2">
                  <button
                    type="button"
                    onClick$={confirmDeletePoll}
                    disabled={deleting.value}
                    class="bg-red-700 hover:bg-red-600 disabled:opacity-50 text-white font-medium px-4 py-1.5 rounded-full text-sm"
                  >
                    {deleting.value ? "Deleting..." : "Delete"}
                  </button>
                  <button
                    type="button"
                    onClick$={() => (confirmDelete.value = false)}
                    class="bg-gray-700 hover:bg-gray-600 text-gray-200 font-medium px-4 py-1.5 rounded-full text-sm"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      {/* Voting form */}
      {isOpen && !hasVoted.value && (
        linked.value ? (
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-5 mb-6">
            <h2 class="text-sm font-medium text-gray-300 mb-3">
              Cast your vote
            </h2>

            {voteError.value && (
              <div class="bg-red-900/50 border border-red-700 text-red-300 px-3 py-2 rounded text-sm mb-3">
                {voteError.value}
              </div>
            )}

            <div class="space-y-2 mb-4">
              {p.options.map((option, i) => (
                <label
                  key={i}
                  class={`flex items-center gap-3 p-3 rounded-lg border cursor-pointer transition-colors ${
                    selectedOption.value === i
                      ? "border-indigo-500 bg-indigo-950/50"
                      : "border-gray-700 hover:border-gray-600"
                  }`}
                >
                  <input
                    type="radio"
                    name="vote"
                    checked={selectedOption.value === i}
                    onChange$={() => (selectedOption.value = i)}
                    class="accent-indigo-500"
                  />
                  <span class="text-white">{option}</span>
                </label>
              ))}
            </div>

            <button
              type="button"
              onClick$={submitVote}
              disabled={selectedOption.value === null || voting.value}
              class="bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-full text-sm"
            >
              {voting.value ? "Voting..." : "Vote"}
            </button>
          </div>
        ) : (
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-5 mb-6 text-center">
            <p class="text-gray-400 mb-4">
              Sign in with Flowsta to vote on this poll.
            </p>
            <a href={`/identity/?link=true&returnTo=/poll/${pollHash}/`}>
              <img
                src="/assets/flowsta-signin.svg"
                alt="Sign in with Flowsta to vote"
                width={158}
                height={36}
                class="hover:opacity-80 transition-opacity mx-auto"
              />
            </a>
          </div>
        )
      )}

      {hasVoted.value && (
        <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg mb-6 text-sm">
          You have voted on this poll.
        </div>
      )}

      {/* Raw Results */}
      <div class="bg-gray-900 border border-gray-800 rounded-lg p-5 mb-6">
        <h2 class="text-sm font-medium text-gray-300 mb-4">
          Results ({totalVotes} total)
        </h2>

        {totalVotes === 0 ? (
          <p class="text-gray-500 text-sm">No votes yet</p>
        ) : (
          <div class="space-y-3">
            {p.options.map((option, i) => {
              const count = voteCounts[i];
              const pct =
                totalVotes > 0
                  ? Math.round((count / totalVotes) * 100)
                  : 0;

              return (
                <div key={i}>
                  <div class="flex justify-between text-sm mb-1">
                    <span class="text-gray-200">{option}</span>
                    <span class="text-gray-400">
                      {count} ({pct}%)
                    </span>
                  </div>
                  <div class="h-2 bg-gray-800 rounded-full overflow-hidden">
                    <div
                      class="h-full bg-indigo-600 rounded-full transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Verified Results — only shown when identity-linked voters exist */}
      {verifiedLoading.value && (
        <div class="text-gray-500 text-sm mb-6">
          Computing verified results...
        </div>
      )}

      {verified.value && verified.value.identityCount > 0 && (
        <div class="bg-gray-900 border border-indigo-900 rounded-lg p-5">
          <h2 class="text-sm font-medium text-indigo-300 mb-1">
            Verified Results
          </h2>
          <p class="text-xs text-gray-500 mb-4">
            {verified.value.verifiedVoteCount} verified vote
            {verified.value.verifiedVoteCount !== 1 ? "s" : ""} from{" "}
            {verified.value.identityCount} confirmed{" "}
            {verified.value.identityCount !== 1 ? "people" : "person"}.
            One vote per person.
          </p>

          <div class="space-y-3">
            {p.options.map((option, i) => {
              const count = verified.value!.verifiedCounts[i];
              const total = verified.value!.verifiedVoteCount;
              const pct =
                total > 0 ? Math.round((count / total) * 100) : 0;

              return (
                <div key={`v-${i}`}>
                  <div class="flex justify-between text-sm mb-1">
                    <span class="text-gray-200">{option}</span>
                    <span class="text-gray-400">
                      {count} ({pct}%)
                    </span>
                  </div>
                  <div class="h-2 bg-gray-800 rounded-full overflow-hidden">
                    <div
                      class="h-full bg-indigo-400 rounded-full transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
});
