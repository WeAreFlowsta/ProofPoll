import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { useNavigate } from "@builder.io/qwik-city";
import { linkedContext } from "~/lib/context";
import { createPoll, type PollType } from "~/lib/holochain";

export default component$(() => {
  const linked = useContext(linkedContext);
  const nav = useNavigate();
  const title = useSignal("");
  const description = useSignal("");
  const options = useSignal<string[]>(["", ""]);
  const closesAt = useSignal("");
  const minDateTime = useSignal("");
  const noExpiry = useSignal(false);
  const pollType = useSignal<PollType>("Anonymous");
  const submitting = useSignal(false);
  const error = useSignal<string | null>(null);

  useVisibleTask$(() => {
    const now = new Date();
    minDateTime.value = now.toISOString().slice(0, 16);
    // Default to 2 days from now
    const twoDays = new Date(Date.now() + 2 * 24 * 60 * 60 * 1000);
    closesAt.value = twoDays.toISOString().slice(0, 16);
  });

  const addOption = $(() => {
    if (options.value.length < 10) {
      options.value = [...options.value, ""];
    }
  });

  const removeOption = $((index: number) => {
    if (options.value.length > 2) {
      options.value = options.value.filter((_, i) => i !== index);
    }
  });

  const updateOption = $((index: number, value: string) => {
    const updated = [...options.value];
    updated[index] = value;
    options.value = updated;
  });

  const submit = $(async () => {
    error.value = null;

    const trimmedTitle = title.value.trim();
    if (!trimmedTitle) {
      error.value = "Title is required";
      return;
    }

    const trimmedOptions = options.value
      .map((o) => o.trim())
      .filter((o) => o.length > 0);
    if (trimmedOptions.length < 2) {
      error.value = "At least 2 options are required";
      return;
    }

    let closesAtTs: number | null = null;
    if (!noExpiry.value) {
      if (!closesAt.value) {
        error.value = "Set a closing time or check 'No expiry'";
        return;
      }
      closesAtTs = Math.floor(new Date(closesAt.value).getTime() / 1000);
      if (closesAtTs <= Math.floor(Date.now() / 1000)) {
        error.value = "Closing time must be in the future";
        return;
      }
    }

    submitting.value = true;

    try {
      const hash = await createPoll({
        title: trimmedTitle,
        description: description.value.trim(),
        options: trimmedOptions,
        closes_at: closesAtTs,
        poll_type: pollType.value,
      });

      await nav(`/poll/${hash}/`);
    } catch (e: any) {
      error.value = e.message || String(e) || "Failed to create poll";
      submitting.value = false;
    }
  });

  if (!linked.value) {
    return (
      <div class="max-w-xl mx-auto text-center py-16">
        <h1 class="text-2xl font-bold mb-4">Create Poll</h1>
        <p class="text-gray-400 mb-6">
          Sign in with Flowsta to create polls with verified identity.
        </p>
        <a href="/identity/?link=true&returnTo=/create/">
          <img
            src="/assets/flowsta-signin.svg"
            alt="Sign in with Flowsta"
            width={158}
            height={36}
            class="hover:opacity-80 transition-opacity mx-auto"
          />
        </a>
      </div>
    );
  }

  return (
    <div class="max-w-xl mx-auto">
      <h1 class="text-2xl font-bold mb-6">Create Poll</h1>

      {error.value && (
        <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-3 rounded-lg mb-4">
          {error.value}
        </div>
      )}

      <div class="space-y-5">
        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">
            Title
          </label>
          <input
            type="text"
            value={title.value}
            onInput$={(e) =>
              (title.value = (e.target as HTMLInputElement).value)
            }
            class="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
            placeholder="What should we decide?"
          />
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">
            Description (optional)
          </label>
          <textarea
            value={description.value}
            onInput$={(e) =>
              (description.value = (e.target as HTMLTextAreaElement).value)
            }
            class="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500 h-24 resize-none"
            placeholder="Add more context..."
          />
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-2">
            Options
          </label>
          <div class="space-y-2">
            {options.value.map((opt, i) => (
              <div key={i} class="flex gap-2">
                <input
                  type="text"
                  value={opt}
                  onInput$={(e) =>
                    updateOption(i, (e.target as HTMLInputElement).value)
                  }
                  class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
                  placeholder={`Option ${i + 1}`}
                />
                {options.value.length > 2 && (
                  <button
                    type="button"
                    onClick$={() => removeOption(i)}
                    class="text-gray-500 hover:text-red-400 px-2"
                  >
                    x
                  </button>
                )}
              </div>
            ))}
          </div>
          {options.value.length < 10 && (
            <button
              type="button"
              onClick$={addOption}
              class="mt-2 text-sm text-indigo-400 hover:text-indigo-300"
            >
              + Add option
            </button>
          )}
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">
            Closes at
          </label>
          {!noExpiry.value && (
            <input
              type="datetime-local"
              value={closesAt.value}
              min={minDateTime.value}
              onInput$={(e) =>
                (closesAt.value = (e.target as HTMLInputElement).value)
              }
              class="bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
            />
          )}
          <label class="flex items-center gap-2 mt-2 cursor-pointer w-fit">
            <input
              type="checkbox"
              checked={noExpiry.value}
              onChange$={(e) =>
                (noExpiry.value = (e.target as HTMLInputElement).checked)
              }
              class="accent-indigo-500"
            />
            <span class="text-sm text-gray-400">No expiry</span>
          </label>
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-2">
            Poll type
          </label>
          <div class="flex gap-3">
            <button
              type="button"
              onClick$={() => (pollType.value = "Anonymous")}
              class={`flex-1 py-2 px-4 rounded-lg border text-sm font-medium transition-colors ${
                pollType.value === "Anonymous"
                  ? "bg-indigo-600 border-indigo-500 text-white"
                  : "bg-gray-900 border-gray-700 text-gray-400 hover:border-gray-500"
              }`}
            >
              Anonymous
            </button>
            <button
              type="button"
              onClick$={() => (pollType.value = "Public")}
              class={`flex-1 py-2 px-4 rounded-lg border text-sm font-medium transition-colors ${
                pollType.value === "Public"
                  ? "bg-indigo-600 border-indigo-500 text-white"
                  : "bg-gray-900 border-gray-700 text-gray-400 hover:border-gray-500"
              }`}
            >
              Public
            </button>
          </div>
          <p class="text-xs text-gray-500 mt-1.5">
            {pollType.value === "Anonymous"
              ? "Votes are counted but voter identities are not shown."
              : "Voters' display names are shown alongside their vote."}
          </p>
        </div>

        <button
          type="button"
          onClick$={submit}
          disabled={submitting.value}
          class="w-full bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium py-2.5 rounded-full"
        >
          {submitting.value ? "Creating..." : "Create Poll"}
        </button>
      </div>
    </div>
  );
});
