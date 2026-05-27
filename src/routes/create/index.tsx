import { component$, useContext, useSignal, useVisibleTask$, useComputed$, $ } from "@builder.io/qwik";
import { useNavigate, Link } from "@builder.io/qwik-city";
import { linkedContext } from "~/lib/context";
import { createPoll, saveDraftPoll, type PollType } from "~/lib/holochain";

const MONTHS = ["January","February","March","April","May","June","July","August","September","October","November","December"];
const DAYS = ["Mo","Tu","We","Th","Fr","Sa","Su"];

export default component$(() => {
  const linked = useContext(linkedContext);
  const nav = useNavigate();
  const title = useSignal("");
  const description = useSignal("");
  const options = useSignal<string[]>(["", ""]);
  // Stored as "YYYY-MM-DD" — expires at 23:59 on that day
  const closesAtDate = useSignal("");
  const noExpiry = useSignal(false);
  const showCalendar = useSignal(false);
  // Calendar navigation state
  const calYear = useSignal(new Date().getFullYear());
  const calMonth = useSignal(new Date().getMonth()); // 0-11
  const pollType = useSignal<PollType>("Anonymous");
  const submitting = useSignal(false);
  const savingDraft = useSignal(false);
  const draftSaved = useSignal(false);
  const error = useSignal<string | null>(null);

  useVisibleTask$(() => {
    // Default to 2 days from now
    const twoDays = new Date(Date.now() + 2 * 24 * 60 * 60 * 1000);
    closesAtDate.value = twoDays.toISOString().slice(0, 10);
    calYear.value = twoDays.getFullYear();
    calMonth.value = twoDays.getMonth();
  });

  // Format selected date for display
  const displayDate = useComputed$(() => {
    if (!closesAtDate.value) return "";
    const d = new Date(closesAtDate.value + "T00:00:00");
    return d.toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
  });

  // Build calendar grid for current calYear/calMonth
  const calendarDays = useComputed$(() => {
    const year = calYear.value;
    const month = calMonth.value;
    const today = new Date();
    today.setHours(0, 0, 0, 0);

    const firstDay = new Date(year, month, 1);
    // Mon=0 ... Sun=6
    const startOffset = (firstDay.getDay() + 6) % 7;
    const daysInMonth = new Date(year, month + 1, 0).getDate();
    const daysInPrev = new Date(year, month, 0).getDate();

    const cells: { date: string; day: number; current: boolean; past: boolean; selected: boolean }[] = [];

    // Leading days from previous month
    for (let i = startOffset - 1; i >= 0; i--) {
      const d = new Date(year, month - 1, daysInPrev - i);
      cells.push({ date: d.toISOString().slice(0, 10), day: daysInPrev - i, current: false, past: true, selected: false });
    }

    // Days in current month
    for (let d = 1; d <= daysInMonth; d++) {
      const date = new Date(year, month, d);
      const dateStr = date.toISOString().slice(0, 10);
      cells.push({
        date: dateStr,
        day: d,
        current: true,
        past: date < today,
        selected: dateStr === closesAtDate.value,
      });
    }

    // Trailing days to complete the last row
    const remaining = (7 - (cells.length % 7)) % 7;
    for (let d = 1; d <= remaining; d++) {
      const date = new Date(year, month + 1, d);
      cells.push({ date: date.toISOString().slice(0, 10), day: d, current: false, past: false, selected: false });
    }

    return cells;
  });

  const prevMonth = $(() => {
    if (calMonth.value === 0) { calMonth.value = 11; calYear.value -= 1; }
    else calMonth.value -= 1;
  });

  const nextMonth = $(() => {
    if (calMonth.value === 11) { calMonth.value = 0; calYear.value += 1; }
    else calMonth.value += 1;
  });

  const pickDate = $((dateStr: string) => {
    closesAtDate.value = dateStr;
    showCalendar.value = false;
  });

  const addOption = $(() => {
    if (options.value.length < 10) options.value = [...options.value, ""];
  });

  const removeOption = $((index: number) => {
    if (options.value.length > 2) options.value = options.value.filter((_, i) => i !== index);
  });

  const updateOption = $((index: number, value: string) => {
    const updated = [...options.value];
    updated[index] = value;
    options.value = updated;
  });

  const saveDraft = $(async () => {
    error.value = null;
    const trimmedTitle = title.value.trim();
    if (!trimmedTitle) { error.value = "Title is required to save a draft"; return; }

    const trimmedOptions = options.value.map((o) => o.trim()).filter((o) => o.length > 0);

    savingDraft.value = true;
    try {
      let closesAtTs: number | null = null;
      if (!noExpiry.value && closesAtDate.value) {
        const d = new Date(closesAtDate.value + "T23:59:59");
        closesAtTs = Math.floor(d.getTime() / 1000);
      }
      await saveDraftPoll({
        title: trimmedTitle,
        description: description.value.trim(),
        options: trimmedOptions.length >= 2 ? trimmedOptions : ["", ""],
        closes_at: closesAtTs,
        poll_type: pollType.value,
      });
      draftSaved.value = true;
      setTimeout(() => { draftSaved.value = false; }, 3000);
    } catch (e: any) {
      error.value = e.message || "Failed to save draft";
    } finally {
      savingDraft.value = false;
    }
  });

  const submit = $(async () => {
    error.value = null;

    const trimmedTitle = title.value.trim();
    if (!trimmedTitle) { error.value = "Title is required"; return; }

    const trimmedOptions = options.value.map((o) => o.trim()).filter((o) => o.length > 0);
    if (trimmedOptions.length < 2) { error.value = "At least 2 options are required"; return; }

    let closesAtTs: number | null = null;
    if (!noExpiry.value) {
      if (!closesAtDate.value) { error.value = "Pick a closing date or check 'No expiry'"; return; }
      // Expire at 23:59:59 local time on the chosen day
      const d = new Date(closesAtDate.value + "T23:59:59");
      closesAtTs = Math.floor(d.getTime() / 1000);
      if (closesAtTs <= Math.floor(Date.now() / 1000)) {
        error.value = "Closing date must be in the future";
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
      await nav(`/poll/#${hash}`);
    } catch (e: any) {
      error.value = e.message || String(e) || "Failed to create poll";
      submitting.value = false;
    }
  });

  if (!linked.value) {
    return (
      <div class="max-w-xl mx-auto text-center py-16">
        <h1 class="text-2xl font-bold mb-4">Create Poll</h1>
        <p class="text-gray-400 mb-6">Sign in with Flowsta to create polls with verified identity.</p>
        <a href="/identity/?link=true&returnTo=/create/">
          <img src="/assets/flowsta-signin.svg" alt="Sign in with Flowsta" width={158} height={36} class="hover:opacity-80 transition-opacity mx-auto" />
        </a>
      </div>
    );
  }

  return (
    <div class="max-w-xl mx-auto">
      <h1 class="text-2xl font-bold mb-6">Create Poll</h1>

      {error.value && (
        <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-3 rounded-lg mb-4">{error.value}</div>
      )}

      <div class="space-y-5">
        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">Title</label>
          <input
            type="text"
            value={title.value}
            onInput$={(e) => (title.value = (e.target as HTMLInputElement).value)}
            class="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
            placeholder="What should we decide?"
          />
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">Description (optional)</label>
          <textarea
            value={description.value}
            onInput$={(e) => (description.value = (e.target as HTMLTextAreaElement).value)}
            class="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500 h-24 resize-none"
            placeholder="Add more context..."
          />
        </div>

        <div>
          <label class="block text-sm font-medium text-gray-300 mb-2">Options</label>
          <div class="space-y-2">
            {options.value.map((opt, i) => (
              <div key={i} class="flex gap-2">
                <input
                  type="text"
                  value={opt}
                  onInput$={(e) => updateOption(i, (e.target as HTMLInputElement).value)}
                  class="flex-1 bg-gray-900 border border-gray-700 rounded-lg px-3 py-2 text-white focus:outline-none focus:border-indigo-500"
                  placeholder={`Option ${i + 1}`}
                />
                {options.value.length > 2 && (
                  <button type="button" onClick$={() => removeOption(i)} class="text-gray-500 hover:text-red-400 px-2">x</button>
                )}
              </div>
            ))}
          </div>
          {options.value.length < 10 && (
            <button type="button" onClick$={addOption} class="mt-2 text-sm text-indigo-400 hover:text-indigo-300">+ Add option</button>
          )}
        </div>

        {/* Date picker */}
        <div>
          <label class="block text-sm font-medium text-gray-300 mb-1">Closes at</label>
          {!noExpiry.value && (
            <div class="relative">
              {/* Trigger button */}
              <button
                type="button"
                onClick$={() => (showCalendar.value = !showCalendar.value)}
                class="flex items-center gap-2 bg-gray-900 border border-gray-700 hover:border-gray-500 rounded-lg px-3 py-2 text-white text-sm"
              >
                <svg class="w-4 h-4 text-gray-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
                  <path stroke-linecap="round" stroke-linejoin="round" d="M8 7V3m8 4V3m-9 8h10M5 21h14a2 2 0 002-2V7a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
                </svg>
                {displayDate.value || "Pick a date"}
              </button>

              {/* Calendar dropdown */}
              {showCalendar.value && (
                <>
                  {/* Backdrop */}
                  <div class="fixed inset-0 z-10" onClick$={() => (showCalendar.value = false)} />
                  <div class="absolute z-20 mt-1 bg-gray-900 border border-gray-700 rounded-xl shadow-xl p-4 w-72">
                    {/* Month navigation */}
                    <div class="flex items-center justify-between mb-3">
                      <button type="button" onClick$={prevMonth} class="text-gray-400 hover:text-white p-1 rounded">
                        <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
                          <path stroke-linecap="round" stroke-linejoin="round" d="M15 19l-7-7 7-7" />
                        </svg>
                      </button>
                      <span class="text-sm font-medium text-white">
                        {MONTHS[calMonth.value]} {calYear.value}
                      </span>
                      <button type="button" onClick$={nextMonth} class="text-gray-400 hover:text-white p-1 rounded">
                        <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
                          <path stroke-linecap="round" stroke-linejoin="round" d="M9 5l7 7-7 7" />
                        </svg>
                      </button>
                    </div>

                    {/* Day headers */}
                    <div class="grid grid-cols-7 mb-1">
                      {DAYS.map((d) => (
                        <div key={d} class="text-center text-xs text-gray-500 py-1">{d}</div>
                      ))}
                    </div>

                    {/* Day cells */}
                    <div class="grid grid-cols-7 gap-0.5">
                      {calendarDays.value.map((cell) => (
                        <button
                          key={cell.date}
                          type="button"
                          disabled={cell.past}
                          onClick$={() => pickDate(cell.date)}
                          class={`text-center text-sm rounded-lg py-1.5 transition-colors ${
                            cell.selected
                              ? "bg-indigo-600 text-white font-medium"
                              : cell.past
                              ? "text-gray-700 cursor-default"
                              : !cell.current
                              ? "text-gray-600 hover:text-gray-400"
                              : "text-gray-200 hover:bg-gray-700"
                          }`}
                        >
                          {cell.day}
                        </button>
                      ))}
                    </div>
                  </div>
                </>
              )}
            </div>
          )}
          <label class="flex items-center gap-2 mt-2 cursor-pointer w-fit">
            <input
              type="checkbox"
              checked={noExpiry.value}
              onChange$={(e) => (noExpiry.value = (e.target as HTMLInputElement).checked)}
              class="accent-indigo-500"
            />
            <span class="text-sm text-gray-400">No expiry</span>
          </label>
        </div>

        {/* Poll type */}
        <div>
          <label class="block text-sm font-medium text-gray-300 mb-2">Poll type</label>
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

        {draftSaved.value && (
          <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg text-sm">
            Draft saved! View it on the <Link href="/drafts/" class="underline">Drafts page</Link>.
          </div>
        )}

        <div class="flex gap-3">
          <button
            type="button"
            onClick$={saveDraft}
            disabled={savingDraft.value || submitting.value}
            class="flex-1 bg-gray-700 hover:bg-gray-600 disabled:opacity-50 text-gray-200 font-medium py-2.5 rounded-full"
          >
            {savingDraft.value ? "Encrypting..." : "Save as Draft"}
          </button>
          <button
            type="button"
            onClick$={submit}
            disabled={submitting.value || savingDraft.value}
            class="flex-1 bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium py-2.5 rounded-full"
          >
            {submitting.value ? "Creating..." : "Create Poll"}
          </button>
        </div>
      </div>
    </div>
  );
});
