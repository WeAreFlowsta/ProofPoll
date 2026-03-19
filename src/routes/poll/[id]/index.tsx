import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { useLocation, useNavigate } from "@builder.io/qwik-city";
import { linkedContext } from "~/lib/context";
import { invoke } from "@tauri-apps/api/core";
import {
  getPoll,
  getPollVotes,
  castVote,
  deletePoll,
  getPollFlags,
  flagPoll,
  removeFlag,
  type Poll,
  type VoteData,
  type FlagData,
  type FlagReason,
} from "~/lib/holochain";

export default component$(() => {
  const linked = useContext(linkedContext);
  const loc = useLocation();
  const nav = useNavigate();
  const poll = useSignal<Poll | null>(null);
  const pollAuthor = useSignal<string | null>(null);
  const pollDnaVersion = useSignal<"1.0" | "1.1" | "1.2">("1.2");
  const votes = useSignal<VoteData[]>([]);
  const myAgent = useSignal<string | null>(null);
  const selectedOption = useSignal<number | null>(null);
  const loading = useSignal(true);
  const voting = useSignal(false);
  const hasVoted = useSignal(false);
  const error = useSignal<string | null>(null);
  const voteError = useSignal<string | null>(null);
  const confirmDelete = useSignal(false);
  const deleting = useSignal(false);
  const deleteError = useSignal<string | null>(null);
  const flags = useSignal<FlagData[]>([]);
  const myFlag = useSignal<FlagData | null>(null);
  const flagging = useSignal(false);
  const flagError = useSignal<string | null>(null);
  const showFlagPicker = useSignal(false);

  const pollHash = loc.params.id;

  useVisibleTask$(async () => {
    try {
      const status = await invoke<{ agent_pub_key: string | null }>(
        "get_app_status",
      );
      myAgent.value = status.agent_pub_key;

      // Get the poll first so we know which DHT it lives on (dna_version).
      // Votes and flags depend on dna_version, so they're fetched after.
      const pollResult = await getPoll(pollHash);

      if (!pollResult) {
        error.value = "Poll not found";
        return;
      }

      poll.value = pollResult.poll;
      pollAuthor.value = pollResult.author;
      pollDnaVersion.value = pollResult.dna_version;

      // Fetch votes from the correct cell. Flags exist on v1.1 and v1.2.
      const [votesResult, flagsResult] = await Promise.all([
        getPollVotes(pollHash, pollResult.dna_version),
        (pollResult.dna_version === "1.1" || pollResult.dna_version === "1.2")
          ? getPollFlags(pollHash).catch(() => [] as FlagData[])
          : Promise.resolve([] as FlagData[]),
      ]);

      votes.value = votesResult;
      flags.value = flagsResult;

      if (myAgent.value) {
        hasVoted.value = votesResult.some(
          (v) => v.author === myAgent.value,
        );
        myFlag.value = flagsResult.find(
          (f) => f.author === myAgent.value,
        ) ?? null;
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
      await castVote(
        pollHash,
        selectedOption.value,
        pollDnaVersion.value,
        poll.value?.poll_type ?? undefined,
      );

      const newVotes = await getPollVotes(pollHash, pollDnaVersion.value);
      votes.value = newVotes;
      hasVoted.value = true;


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

  const submitFlag = $(async (reason: FlagReason) => {
    flagError.value = null;
    flagging.value = true;
    showFlagPicker.value = false;
    try {
      await flagPoll(pollHash, reason);
      const updatedFlags = await getPollFlags(pollHash);
      flags.value = updatedFlags;
      myFlag.value = updatedFlags.find(
        (f) => f.author === myAgent.value,
      ) ?? null;
    } catch (e: any) {
      flagError.value = e.message || "Failed to flag poll";
    } finally {
      flagging.value = false;
    }
  });

  const unflag = $(async () => {
    if (!myFlag.value) return;
    flagError.value = null;
    flagging.value = true;
    try {
      await removeFlag(myFlag.value.hash);
      myFlag.value = null;
      flags.value = flags.value.filter(
        (f) => f.author !== myAgent.value,
      );
    } catch (e: any) {
      flagError.value = e.message || "Failed to remove flag";
    } finally {
      flagging.value = false;
    }
  });

  if (loading.value) {
    return <div class="text-gray-400">Loading poll...</div>;
  }

  if (error.value) {
    return (
      <div class="max-w-md mx-auto mt-12">
        <div class="bg-red-900/20 border border-red-800/40 rounded-lg p-5">
          <div class="flex items-start gap-3">
            <svg class="w-5 h-5 text-red-400 mt-0.5 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
              <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
            </svg>
            <div>
              <p class="text-red-300 text-sm font-medium mb-1">Couldn't load poll</p>
              <p class="text-red-400/70 text-xs mb-3">{error.value}</p>
              <a
                href={`/poll/${pollHash}/`}
                class="text-xs bg-red-800/40 hover:bg-red-800/60 text-red-300 px-3 py-1.5 rounded-full font-medium transition-colors inline-block"
              >
                Try again
              </a>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (!poll.value) return null;

  const p = poll.value;
  const isOpen = !p.closes_at || p.closes_at > Date.now() / 1000;
  const totalVotes = votes.value.length;
  const isPublic = p.poll_type === "Public";

  const voteCounts: number[] = p.options.map(
    (_, i) => votes.value.filter((v) => v.vote.option_index === i).length,
  );

  return (
    <div class="max-w-2xl mx-auto">
      <div class="mb-6">
        <div class="flex items-start justify-between mb-2">
          <h1 class="text-2xl font-bold">{p.title}</h1>
          <div class="flex items-center gap-2 shrink-0">
            {isPublic && (
              <span class="text-xs px-2 py-0.5 rounded bg-blue-900 text-blue-300">
                Public
              </span>
            )}
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
        </div>
        {p.description && (
          <p class="text-gray-400 mb-3">{p.description}</p>
        )}
        <div class="text-xs text-gray-500">
          {totalVotes} vote{totalVotes !== 1 ? "s" : ""}
          {flags.value.length > 0 && (
            <span>
              {" "}
              · {flags.value.length} flag{flags.value.length !== 1 ? "s" : ""}
            </span>
          )}
          {p.closes_at && (
            <span>
              {" "}
              · Closes {new Date(p.closes_at * 1000).toLocaleString()}
            </span>
          )}
        </div>

        {/* Flag / unflag — only on v1.1 and v1.2 polls (v1.0 has no Flag entry type) */}
        {linked.value && myAgent.value && pollAuthor.value !== myAgent.value && (pollDnaVersion.value === "1.1" || pollDnaVersion.value === "1.2") && (
          <div class="mt-3">
            {flagError.value && (
              <div class="text-red-400 text-xs mb-2">{flagError.value}</div>
            )}
            {myFlag.value ? (
              <button
                type="button"
                onClick$={unflag}
                disabled={flagging.value}
                class="text-amber-400 hover:text-amber-300 text-xs disabled:opacity-50"
              >
                {flagging.value ? "Removing flag..." : "You flagged this poll · Unflag"}
              </button>
            ) : showFlagPicker.value ? (
              <div class="bg-gray-800 border border-gray-700 rounded-lg p-3 inline-block">
                <p class="text-xs text-gray-400 mb-2">Why are you flagging this poll?</p>
                <div class="flex flex-wrap gap-2">
                  {(["Spam", "Misleading", "OffTopic", "Inappropriate"] as const).map((reason) => (
                    <button
                      key={reason}
                      type="button"
                      onClick$={() => submitFlag(reason)}
                      disabled={flagging.value}
                      class="text-xs bg-gray-700 hover:bg-gray-600 text-gray-200 px-3 py-1.5 rounded-full disabled:opacity-50"
                    >
                      {reason === "OffTopic" ? "Off Topic" : reason}
                    </button>
                  ))}
                  <button
                    type="button"
                    onClick$={() => (showFlagPicker.value = false)}
                    class="text-xs text-gray-500 hover:text-gray-300 px-2"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <button
                type="button"
                onClick$={() => (showFlagPicker.value = true)}
                disabled={flagging.value}
                class="text-gray-500 hover:text-amber-400 text-xs disabled:opacity-50"
              >
                {flagging.value ? "Flagging..." : "Flag this poll"}
              </button>
            )}
          </div>
        )}

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

            {isPublic && (
              <div class="bg-blue-900/20 border border-blue-800/40 rounded-lg px-3 py-2 text-xs text-blue-300 mb-3">
                This is a public poll — your display name will be shown alongside your vote.
              </div>
            )}

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

      {/* Results */}
      <div class="bg-gray-900 border border-gray-800 rounded-lg p-5 mb-6">
        <h2 class="text-sm font-medium text-gray-300 mb-4">
          Results ({totalVotes} total)
        </h2>

        {totalVotes === 0 ? (
          <p class="text-gray-500 text-sm">No votes yet</p>
        ) : (
          <div class="space-y-4">
            {p.options.map((option, i) => {
              const count = voteCounts[i];
              const pct =
                totalVotes > 0
                  ? Math.round((count / totalVotes) * 100)
                  : 0;
              const optionVoters = isPublic
                ? votes.value.filter((v) => v.vote.option_index === i && v.display_name)
                : [];
              const visibleVoters = optionVoters.slice(0, 5);
              const hiddenCount = optionVoters.length - visibleVoters.length;

              return (
                <div key={i}>
                  <div class="flex justify-between text-sm mb-1">
                    <span class="text-gray-200">{option}</span>
                    <span class="text-gray-400">
                      {count} ({pct}%)
                    </span>
                  </div>
                  <div class="h-2 bg-gray-800 rounded-full overflow-hidden mb-1.5">
                    <div
                      class="h-full bg-indigo-600 rounded-full transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  {visibleVoters.length > 0 && (
                    <div class="flex flex-wrap gap-1.5 mt-1">
                      {visibleVoters.map((v) => (
                        <div key={v.author} class="flex items-center gap-1 bg-gray-800 rounded-full px-2 py-0.5">
                          {v.profile_picture && (
                            <img
                              src={v.profile_picture}
                              alt=""
                              width={14}
                              height={14}
                              class="rounded-full"
                            />
                          )}
                          <span class="text-xs text-gray-300">{v.display_name}</span>
                        </div>
                      ))}
                      {hiddenCount > 0 && (
                        <span class="text-xs text-gray-500 self-center">+{hiddenCount} more</span>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

    </div>
  );
});
