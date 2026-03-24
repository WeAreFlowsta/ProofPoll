import { component$, useSignal, useVisibleTask$, useContext, $ } from "@builder.io/qwik";
import { useNavigate } from "@builder.io/qwik-city";
import { linkedContext } from "~/lib/context";
import { getMyDrafts, publishDraft, deleteDraft, type DraftPollItem } from "~/lib/holochain";

export default component$(() => {
  const linked = useContext(linkedContext);
  const nav = useNavigate();
  const drafts = useSignal<DraftPollItem[]>([]);
  const loading = useSignal(true);
  const publishing = useSignal<string | null>(null);
  const deleteConfirm = useSignal<string | null>(null);
  const deleting = useSignal(false);
  const error = useSignal<string | null>(null);

  useVisibleTask$(async () => {
    if (!linked.value) {
      loading.value = false;
      return;
    }
    try {
      drafts.value = await getMyDrafts();
    } catch (e: any) {
      error.value = e.message || "Failed to load drafts";
    } finally {
      loading.value = false;
    }
  });

  const handlePublish = $(async (hash: string) => {
    publishing.value = hash;
    error.value = null;
    try {
      const pollHash = await publishDraft(hash);
      await nav(`/poll/${pollHash}/`);
    } catch (e: any) {
      error.value = e.message || "Failed to publish draft";
      publishing.value = null;
    }
  });

  const handleDelete = $(async (hash: string) => {
    deleting.value = true;
    error.value = null;
    try {
      await deleteDraft(hash);
      drafts.value = drafts.value.filter((d) => d.hash !== hash);
      deleteConfirm.value = null;
    } catch (e: any) {
      error.value = e.message || "Failed to delete draft";
    } finally {
      deleting.value = false;
    }
  });

  if (!linked.value) {
    return (
      <div class="max-w-xl mx-auto text-center py-16">
        <h1 class="text-2xl font-bold mb-4">Drafts</h1>
        <p class="text-gray-400">Sign in with Flowsta to view your draft polls.</p>
      </div>
    );
  }

  if (loading.value) {
    return <div class="text-gray-400">Loading drafts...</div>;
  }

  return (
    <div class="max-w-2xl mx-auto">
      <div class="flex items-center justify-between mb-6">
        <h1 class="text-2xl font-bold">Drafts</h1>
        <div class="flex items-center gap-2 text-xs text-gray-500">
          <svg class="w-4 h-4 text-amber-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
            <path stroke-linecap="round" stroke-linejoin="round" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z" />
          </svg>
          Encrypted on the DHT — only you can read these
        </div>
      </div>

      {error.value && (
        <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-3 rounded-lg mb-4 text-sm">
          {error.value}
        </div>
      )}

      {drafts.value.length === 0 ? (
        <div class="bg-gray-900 border border-gray-800 rounded-lg p-8 text-center">
          <p class="text-gray-400 mb-4">No draft polls yet.</p>
          <a
            href="/create/"
            class="text-indigo-400 hover:text-indigo-300 text-sm font-medium"
          >
            Create a new poll
          </a>
        </div>
      ) : (
        <div class="space-y-3">
          {drafts.value.map((draft) => {
            const isPublishing = publishing.value === draft.hash;
            const isDeleting = deleteConfirm.value === draft.hash;

            return (
              <div
                key={draft.hash}
                class="bg-gray-900 border border-gray-800 rounded-lg p-5"
              >
                <div class="flex items-start justify-between mb-2">
                  <div>
                    <h3 class="font-medium text-white">{draft.title || "Untitled"}</h3>
                    {draft.description && (
                      <p class="text-gray-400 text-sm mt-1">{draft.description}</p>
                    )}
                  </div>
                  <span class="text-xs text-gray-500 shrink-0 ml-4">
                    {new Date(draft.created_at * 1000).toLocaleDateString()}
                  </span>
                </div>

                {draft.options.length > 0 && (
                  <div class="flex flex-wrap gap-1.5 mb-3">
                    {draft.options.filter(Boolean).map((opt, i) => (
                      <span
                        key={i}
                        class="text-xs bg-gray-800 text-gray-300 px-2 py-1 rounded"
                      >
                        {opt}
                      </span>
                    ))}
                  </div>
                )}

                <div class="flex items-center gap-2 text-xs text-gray-500 mb-3">
                  <span>{draft.poll_type}</span>
                  {draft.closes_at && (
                    <span>
                      · Closes {new Date(draft.closes_at * 1000).toLocaleDateString()}
                    </span>
                  )}
                </div>

                {isDeleting ? (
                  <div class="flex items-center gap-2">
                    <span class="text-xs text-red-400">Delete this draft?</span>
                    <button
                      type="button"
                      disabled={deleting.value}
                      onClick$={() => handleDelete(draft.hash)}
                      class="text-xs text-red-400 border border-red-500/50 hover:bg-red-500/20 px-2 py-1 rounded disabled:opacity-50"
                    >
                      {deleting.value ? "..." : "Yes"}
                    </button>
                    <button
                      type="button"
                      onClick$={() => (deleteConfirm.value = null)}
                      class="text-xs text-gray-400 border border-gray-600 hover:bg-gray-700 px-2 py-1 rounded"
                    >
                      No
                    </button>
                  </div>
                ) : (
                  <div class="flex gap-2">
                    <button
                      type="button"
                      onClick$={() => handlePublish(draft.hash)}
                      disabled={isPublishing}
                      class="bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium px-4 py-1.5 rounded-full text-xs"
                    >
                      {isPublishing ? "Publishing..." : "Publish"}
                    </button>
                    <button
                      type="button"
                      onClick$={() => (deleteConfirm.value = draft.hash)}
                      class="text-gray-500 hover:text-red-400 text-xs"
                    >
                      Delete
                    </button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
});
