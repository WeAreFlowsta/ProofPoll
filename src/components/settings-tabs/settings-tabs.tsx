import { component$, useSignal, useVisibleTask$ } from "@builder.io/qwik";
import { Link, useLocation } from "@builder.io/qwik-city";
import { getVersion } from "@tauri-apps/api/app";

const TABS = [
  { href: "/identity/", label: "Identity", id: "settings-tab-identity" },
  { href: "/settings/updates/", label: "Updates", id: "settings-tab-updates" },
];

/** The Settings heading, this copy's version and the tabs, at the top of
 *  each settings page. The version tells a tester on a beta from one on
 *  the release (asked for 2026-10-25). */
export const SettingsTabs = component$(() => {
  const loc = useLocation();
  const version = useSignal<string | null>(null);
  // eslint-disable-next-line qwik/no-use-visible-task
  useVisibleTask$(async () => {
    version.value = await getVersion().catch(() => null);
  });
  return (
    <div class="mb-6">
      <div class="mb-4 flex items-baseline justify-between gap-4">
        <h1 class="text-2xl font-bold">Settings</h1>
        {version.value && (
          <p class="text-xs text-gray-500" data-testid="settings-version">
            ProofPoll {version.value}
          </p>
        )}
      </div>
      <nav class="flex gap-6 border-b border-gray-800">
        {TABS.map((tab) => {
          const active = loc.url.pathname === tab.href;
          return (
            <Link
              key={tab.href}
              href={tab.href}
              data-testid={tab.id}
              aria-current={active ? "page" : undefined}
              class={`-mb-px border-b-2 pb-2 text-sm ${active ? "border-indigo-400 text-indigo-400 font-medium" : "border-transparent text-gray-400 hover:text-gray-200"}`}
            >
              {tab.label}
            </Link>
          );
        })}
      </nav>
    </div>
  );
});
