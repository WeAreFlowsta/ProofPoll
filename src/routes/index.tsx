import { component$, useSignal, useVisibleTask$ } from "@builder.io/qwik";
import { Link } from "@builder.io/qwik-city";
import { getAllPolls, type PollListItem } from "~/lib/holochain";

export default component$(() => {
  const polls = useSignal<PollListItem[]>([]);
  const loading = useSignal(true);
  const error = useSignal<string | null>(null);

  useVisibleTask$(async () => {
    try {
      polls.value = await getAllPolls();
    } catch (e: any) {
      error.value = e.message || "Failed to load polls";
    } finally {
      loading.value = false;
    }
  });

  return (
    <div>
      <div class="flex items-center justify-between mb-6">
        <h1 class="text-2xl font-bold">Polls</h1>
        <Link
          href="/create/"
          class="bg-indigo-600 hover:bg-indigo-500 text-white px-4 py-2 rounded-lg text-sm font-medium"
        >
          Create Poll
        </Link>
      </div>

      {loading.value ? (
        <div class="text-gray-400">Loading polls...</div>
      ) : error.value ? (
        <div class="text-red-400">{error.value}</div>
      ) : polls.value.length === 0 ? (
        <div class="text-center py-16">
          <p class="text-gray-400 text-lg mb-4">No polls yet</p>
          <Link
            href="/create/"
            class="text-indigo-400 hover:text-indigo-300"
          >
            Create the first poll
          </Link>
        </div>
      ) : (
        <div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {polls.value.map((p) => {
            const isOpen =
              !p.poll.closes_at ||
              p.poll.closes_at > Date.now() / 1000;

            return (
              <Link
                key={p.hash}
                href={`/poll/${p.hash}/`}
                class="bg-gray-900 border border-gray-800 rounded-lg p-5 hover:border-indigo-600 transition-colors"
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
                <div class="text-xs text-gray-500">
                  {p.poll.options.length} options
                </div>
              </Link>
            );
          })}
        </div>
      )}
    </div>
  );
});
