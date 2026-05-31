import { component$, useContext, useSignal, useComputed$, useVisibleTask$, $ } from "@builder.io/qwik";
import { Link, useNavigate } from "@builder.io/qwik-city";
import { setSignInIntent } from "~/lib/signin";
import { invoke } from "@tauri-apps/api/core";
import { linkedContext } from "~/lib/context";
import { getAllPolls, getPollVotes, getPollFlags, getFlagThreshold, loadMyAgentSet, type PollListItem } from "~/lib/holochain";

type Filter = "current" | "archive" | "created" | "voted";

export default component$(() => {
  const linked = useContext(linkedContext);
  const nav = useNavigate();
  const polls = useSignal<PollListItem[]>([]);
  const loading = useSignal(true);
  const loadingSlow = useSignal(false);
  const error = useSignal<string | null>(null);
  const showSignIn = useSignal(false);
  const filter = useSignal<Filter>("current");
  const myAgent = useSignal<string | null>(null);
  // All agent keys belonging to this user (across installs/devices), for
  // recognising their own polls/votes. See loadMyAgentSet.
  const myAgentSet = useSignal<Set<string>>(new Set());
  // Set of poll hashes the user has voted in
  const votedPolls = useSignal<Set<string>>(new Set());
  const votedLoading = useSignal(false);
  // Flag counts per poll hash
  const flagCounts = useSignal<Map<string, number>>(new Map());
  const flagThreshold = useSignal(3);
  const showFlagged = useSignal(false);
  const flagsLoaded = useSignal(false);

  const loadPolls = $(async () => {
    loading.value = true;
    error.value = null;
    try {
      const [allPolls, status, threshold] = await Promise.all([
        getAllPolls(),
        invoke<{ agent_pub_key: string | null }>("get_app_status"),
        getFlagThreshold().catch(() => 3),
      ]);
      polls.value = allPolls;
      myAgent.value = status.agent_pub_key;
      myAgentSet.value = await loadMyAgentSet(status.agent_pub_key);
      flagThreshold.value = threshold;

      // Load flag counts in background. Flags only exist on v1.1 polls.
      if (allPolls.length > 0) {
        Promise.all(
          allPolls.map(async (p) => {
            if (p.dna_version !== "1.1") return { hash: p.hash, count: 0 };
            try {
              const flags = await getPollFlags(p.hash);
              return { hash: p.hash, count: flags.length };
            } catch {
              return { hash: p.hash, count: 0 };
            }
          }),
        ).then((results) => {
          const counts = new Map<string, number>();
          for (const r of results) {
            counts.set(r.hash, r.count);
          }
          flagCounts.value = counts;
          flagsLoaded.value = true;
        });
      }
    } catch (e: any) {
      error.value = e.message || "Failed to load polls";
    } finally {
      loading.value = false;
    }
  });

  // Load which polls the user voted in (called once when "Voted" filter is first selected)
  const loadVotedPolls = $(async () => {
    if (votedPolls.value.size > 0 || !myAgent.value || polls.value.length === 0) return;
    votedLoading.value = true;
    try {
      const results = await Promise.all(
        polls.value.map(async (p) => {
          try {
            const votes = await getPollVotes(p.hash, p.dna_version);
            const voted = votes.some((v) => myAgentSet.value.has(v.author));
            return voted ? p.hash : null;
          } catch {
            return null;
          }
        }),
      );
      votedPolls.value = new Set(results.filter((h): h is string => h !== null));
    } finally {
      votedLoading.value = false;
    }
  });

  const filteredPolls = useComputed$(() => {
    const now = Date.now() / 1000;
    let result = polls.value;

    // Apply tab filter
    if (filter.value === "current") {
      result = result.filter((p) => !p.poll.closes_at || p.poll.closes_at > now);
    } else if (filter.value === "archive") {
      result = result.filter((p) => p.poll.closes_at && p.poll.closes_at <= now);
    } else if (filter.value === "created") {
      result = result.filter((p) => myAgentSet.value.has(p.author));
    } else if (filter.value === "voted") {
      result = result.filter((p) => votedPolls.value.has(p.hash));
    }

    // Hide flagged polls (unless user toggled "show flagged")
    if (!showFlagged.value && flagsLoaded.value) {
      result = result.filter((p) => {
        const count = flagCounts.value.get(p.hash) ?? 0;
        return count < flagThreshold.value;
      });
    }

    return result;
  });

  useVisibleTask$(async ({ cleanup }) => {
    const timer = setTimeout(() => {
      loadingSlow.value = true;
    }, 3000);
    cleanup(() => clearTimeout(timer));

    await loadPolls();

    // Silently re-fetch the poll list every 30s so polls created by peers
    // on other machines show up without the user having to navigate away
    // and back. We update polls.value directly instead of calling
    // loadPolls() so the loading skeleton doesn't flash on each tick.
    const refresh = setInterval(async () => {
      try {
        const fresh = await getAllPolls();
        polls.value = fresh;
      } catch {
        // Ignore transient failures — the next tick will retry.
      }
    }, 30_000);
    cleanup(() => clearInterval(refresh));
  });

  return (
    <div>
      {/* Sign-in dialog */}
      {showSignIn.value && (
        <div
          class="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
          onClick$={() => (showSignIn.value = false)}
        >
          <div
            class="bg-gray-900 border border-gray-700 rounded-xl p-8 max-w-sm w-full mx-4 text-center"
            onClick$={(e) => e.stopPropagation()}
          >
            <h2 class="text-lg font-semibold text-white mb-2">Sign in required</h2>
            <p class="text-gray-400 text-sm mb-6">
              Sign in with Flowsta to create and vote on polls.
            </p>
            <button
              type="button"
              onClick$={() => {
                setSignInIntent({ autoLink: true, returnTo: "/create/" });
                showSignIn.value = false;
                nav("/identity/");
              }}
              class="bg-transparent border-0 p-0 cursor-pointer inline-block"
            >
              <img
                src="/assets/flowsta-signin.svg"
                alt="Sign in with Flowsta"
                width={158}
                height={36}
                class="hover:opacity-80 transition-opacity mx-auto"
              />
            </button>
            <button
              type="button"
              onClick$={() => (showSignIn.value = false)}
              class="mt-4 text-sm text-gray-500 hover:text-gray-300 block mx-auto"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      <div class="flex items-center justify-between mb-6">
        <h1 class="text-2xl font-bold">Polls</h1>
        {linked.value ? (
          <Link
            href="/create/"
            class="bg-indigo-600 hover:bg-indigo-500 text-white px-4 py-2 rounded-full text-sm font-medium"
          >
            Create Poll
          </Link>
        ) : (
          <button
            type="button"
            onClick$={() => (showSignIn.value = true)}
            class="bg-indigo-600 hover:bg-indigo-500 text-white px-4 py-2 rounded-full text-sm font-medium"
          >
            Create Poll
          </button>
        )}
      </div>

      {/* Filter tabs */}
      {!loading.value && !error.value && polls.value.length > 0 && (
        <div class="flex items-center gap-3 mb-5">
        <div class="flex gap-1 bg-gray-900 rounded-lg p-1 w-fit">
          {(["current", "archive", "created", "voted"] as const).map((f) => (
            <button
              key={f}
              type="button"
              onClick$={async () => {
                filter.value = f;
                if (f === "voted" && votedPolls.value.size === 0) {
                  await loadVotedPolls();
                }
              }}
              class={`px-3 py-1.5 rounded-md text-sm font-medium transition-colors ${
                filter.value === f
                  ? "bg-gray-700 text-white"
                  : "text-gray-400 hover:text-gray-200"
              }`}
            >
              {f === "current" ? "Current" : f === "archive" ? "Archive" : f === "created" ? "Created" : "Voted"}
            </button>
          ))}
        </div>
        {flagsLoaded.value && (
          <button
            type="button"
            onClick$={() => (showFlagged.value = !showFlagged.value)}
            class={`text-xs px-3 py-1.5 rounded-full transition-colors ${
              showFlagged.value
                ? "bg-amber-900/50 text-amber-300 hover:bg-amber-900/70"
                : "bg-gray-800 text-gray-500 hover:text-gray-300"
            }`}
          >
            {showFlagged.value ? "Hide flagged" : "Show flagged"}
          </button>
        )}
        </div>
      )}

      {loading.value ? (
        <div class="text-gray-400">
          <p>Loading polls...</p>
          {loadingSlow.value && (
            <p class="text-gray-500 text-sm mt-2">
              Syncing with the network — first load can take a moment.
            </p>
          )}
        </div>
      ) : error.value ? (
        <div class="bg-red-900/20 border border-red-800/40 rounded-lg p-5 max-w-md">
          <div class="flex items-start gap-3">
            <svg class="w-5 h-5 text-red-400 mt-0.5 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
              <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
            </svg>
            <div>
              <p class="text-red-300 text-sm font-medium mb-1">Couldn't load polls</p>
              <p class="text-red-400/70 text-xs mb-3">{error.value}</p>
              <button
                type="button"
                onClick$={() => loadPolls()}
                class="text-xs bg-red-800/40 hover:bg-red-800/60 text-red-300 px-3 py-1.5 rounded-full font-medium transition-colors"
              >
                Try again
              </button>
            </div>
          </div>
        </div>
      ) : polls.value.length === 0 ? (
        <div class="text-center py-16">
          <p class="text-gray-400 text-lg mb-4">No polls yet</p>
          {linked.value ? (
            <Link
              href="/create/"
              class="text-indigo-400 hover:text-indigo-300"
            >
              Create the first poll
            </Link>
          ) : (
            <button
              type="button"
              onClick$={() => (showSignIn.value = true)}
              class="text-indigo-400 hover:text-indigo-300"
            >
              Create the first poll
            </button>
          )}
        </div>
      ) : votedLoading.value ? (
        <div class="text-gray-400">Loading your votes...</div>
      ) : filteredPolls.value.length === 0 ? (
        <div class="text-center py-16">
          <p class="text-gray-400 text-lg mb-4">
            {filter.value === "current"
              ? "No polls are currently open"
              : filter.value === "archive"
              ? "No polls have closed yet"
              : filter.value === "created"
              ? "You haven't created any polls yet"
              : "You haven't voted on any polls yet"}
          </p>
          {linked.value ? (
            <Link
              href={filter.value === "created" ? "/create/" : "/"}
              onClick$={() => {
                if (filter.value === "archive" || filter.value === "voted") filter.value = "current";
              }}
              class="text-indigo-400 hover:text-indigo-300"
            >
              {filter.value === "created"
                ? "Create your first poll"
                : filter.value === "current"
                ? "Create a poll"
                : "Browse current polls"}
            </Link>
          ) : (
            <button
              type="button"
              onClick$={() => (showSignIn.value = true)}
              class="text-indigo-400 hover:text-indigo-300"
            >
              Sign in to {filter.value === "created" ? "create polls" : "vote"}
            </button>
          )}
        </div>
      ) : (
        <div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {filteredPolls.value.map((p) => {
            const isOpen =
              !p.poll.closes_at ||
              p.poll.closes_at > Date.now() / 1000;

            return (
              <div
                key={p.hash}
                onClick$={() => nav(`/poll/#${p.hash}`)}
                class="bg-gray-900 border border-gray-800 rounded-lg p-5 hover:border-indigo-600 transition-colors cursor-pointer"
              >
                <div class="flex items-start justify-between mb-2">
                  <h2 class="text-lg font-semibold text-white">
                    {p.poll.title}
                  </h2>
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
                {p.poll.description && (
                  <p class="text-gray-400 text-sm mb-3 line-clamp-2">
                    {p.poll.description}
                  </p>
                )}
                <div class="text-xs text-gray-500 flex items-center gap-2">
                  <span>{p.poll.options.length} options</span>
                  {(flagCounts.value.get(p.hash) ?? 0) > 0 && (
                    <span class="text-amber-500">
                      {flagCounts.value.get(p.hash)} flag{(flagCounts.value.get(p.hash) ?? 0) !== 1 ? "s" : ""}
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
});
