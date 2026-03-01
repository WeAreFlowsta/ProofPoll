import { component$, useSignal, $ } from "@builder.io/qwik";
import { useNavigate } from "@builder.io/qwik-city";
import { createPoll } from "~/lib/holochain";

export default component$(() => {
  const nav = useNavigate();
  const title = useSignal("");
  const description = useSignal("");
  const options = useSignal<string[]>(["", ""]);
  const closesAt = useSignal("");
  const submitting = useSignal(false);
  const error = useSignal<string | null>(null);

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

    submitting.value = true;

    try {
      let closesAtTs: number | null = null;
      if (closesAt.value) {
        closesAtTs = Math.floor(new Date(closesAt.value).getTime() / 1000);
      }

      const hash = await createPoll({
        title: trimmedTitle,
        description: description.value.trim(),
        options: trimmedOptions,
        closes_at: closesAtTs,
      });

      await nav(`/poll/${hash}/`);
    } catch (e: any) {
      error.value = e.message || "Failed to create poll";
      submitting.value = false;
    }
  });

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
            Closes at (optional)
          </label>
          <input
            type="datetime-local"
            value={closesAt.value}
            onInput$={(e) =>
              (closesAt.value = (e.target as HTMLInputElement).value)
            }
            class="bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
          />
        </div>

        <button
          type="button"
          onClick$={submit}
          disabled={submitting.value}
          class="w-full bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium py-2.5 rounded-lg"
        >
          {submitting.value ? "Creating..." : "Create Poll"}
        </button>
      </div>
    </div>
  );
});
