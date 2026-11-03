import { component$ } from "@builder.io/qwik";
import { Link, useLocation } from "@builder.io/qwik-city";

const TABS = [
  { href: "/identity/", label: "Identity", id: "settings-tab-identity" },
  { href: "/settings/updates/", label: "Updates", id: "settings-tab-updates" },
];

/** The Settings heading and its tabs, at the top of each settings page. */
export const SettingsTabs = component$(() => {
  const loc = useLocation();
  return (
    <div class="mb-6">
      <h1 class="text-2xl font-bold mb-4">Settings</h1>
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
