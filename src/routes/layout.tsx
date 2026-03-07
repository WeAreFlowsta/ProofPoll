import { component$, Slot, useContextProvider, useSignal, useVisibleTask$ } from "@builder.io/qwik";
import { Link, useLocation } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { linkedContext, displayNameContext, profilePictureContext } from "~/lib/context";
import { getLinkedAgents } from "~/lib/holochain";


interface AppStatus {
  ready: boolean;
  agent_pub_key: string | null;
  app_port: number | null;
  conductor_status:
    | { status: "stopped" }
    | { status: "starting"; message: string }
    | { status: "ready"; admin_port: number; app_port: number }
    | { status: "error"; message: string };
}

export default component$(() => {
  const status = useSignal<AppStatus | null>(null);
  const displayName = useSignal<string | null>(null);
  const profilePicture = useSignal<string | null>(null);
  const linked = useSignal(false);
  useContextProvider(linkedContext, linked);
  useContextProvider(displayNameContext, displayName);
  useContextProvider(profilePictureContext, profilePicture);
  const loc = useLocation();
  const showSignIn = useSignal(false);

  useVisibleTask$(({ cleanup }) => {
    let active = true;
    let stopAutoBackup: (() => void) | null = null;

    const startBackup = async () => {
      if (stopAutoBackup) return; // Already running
      try {
        const { startAutoBackup } = await import("@flowsta/holochain");
        stopAutoBackup = startAutoBackup({
          clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
          appName: "ProofPoll",
          intervalMinutes: 60,
          getData: () => invoke("get_export_data"),
          onSuccess: (r) => console.log(`[ProofPoll] Vault backup: ${r.dataSize} bytes`),
          onError: (e) => console.warn("[ProofPoll] Vault backup skipped:", e.message),
        });
      } catch {
        // SDK import failed — ignore
      }
    };

    const stopBackup = () => {
      if (stopAutoBackup) {
        stopAutoBackup();
        stopAutoBackup = null;
      }
    };

    const poll = async () => {
      while (active) {
        try {
          const s = await invoke<AppStatus>("get_app_status");
          status.value = s;
          if (s.ready) {
            // Check DHT link status + verify with Vault
            if (s.agent_pub_key) {
              try {
                const agents = await getLinkedAgents(s.agent_pub_key);
                if (agents.length > 0) {
                  // DHT says linked — verify Vault still agrees
                  try {
                    const linkResp = await fetch(
                      `http://127.0.0.1:27777/link-status?app_agent_pub_key=${encodeURIComponent(s.agent_pub_key)}`,
                      { signal: AbortSignal.timeout(2000) },
                    );
                    if (linkResp.ok) {
                      const linkData = await linkResp.json();
                      linked.value = linkData.linked === true;
                    } else {
                      // Vault running but endpoint error — trust DHT
                      linked.value = true;
                    }
                  } catch {
                    // Vault not running — trust DHT
                    linked.value = true;
                  }
                }
              } catch {
                // Not linked or zome call failed
              }
            }

            // Try to get profile from Vault (only needed if linked)
            if (linked.value) {
              try {
                const resp = await fetch("http://127.0.0.1:27777/status", {
                  signal: AbortSignal.timeout(2000),
                });
                if (resp.ok) {
                  const vault = await resp.json();
                  if (vault.display_name) displayName.value = vault.display_name;
                  if (vault.profile_picture)
                    profilePicture.value = vault.profile_picture;
                }
              } catch {
                // Vault not running — use fallback
              }
              startBackup();
            }
            break;
          }
        } catch (e) {
          console.error("Status poll failed:", e);
        }
        await new Promise((r) => setTimeout(r, 1000));
      }
    };

    poll();

    // Poll link status so header updates after link/unlink on identity page
    const linkPoll = setInterval(async () => {
      const s = status.value;
      if (!s?.ready || !s.agent_pub_key) return;
      try {
        const agents = await getLinkedAgents(s.agent_pub_key);
        const wasLinked = linked.value;
        let nowLinked = agents.length > 0;

        // Verify with Vault if DHT says linked
        if (nowLinked) {
          try {
            const linkResp = await fetch(
              `http://127.0.0.1:27777/link-status?app_agent_pub_key=${encodeURIComponent(s.agent_pub_key)}`,
              { signal: AbortSignal.timeout(2000) },
            );
            if (linkResp.ok) {
              const linkData = await linkResp.json();
              if (linkData.linked === false) nowLinked = false;
            }
          } catch {
            // Vault not running — trust DHT
          }
        }

        linked.value = nowLinked;

        // Start/stop auto-backup based on link status
        if (nowLinked && !wasLinked) startBackup();
        if (!nowLinked && wasLinked) stopBackup();

        // Fetch profile when linked but profile is missing
        if (nowLinked && !displayName.value) {
          try {
            const resp = await fetch("http://127.0.0.1:27777/status", {
              signal: AbortSignal.timeout(2000),
            });
            if (resp.ok) {
              const vault = await resp.json();
              if (vault.display_name) displayName.value = vault.display_name;
              if (vault.profile_picture)
                profilePicture.value = vault.profile_picture;
            }
          } catch {
            // Vault not running
          }
        }

        // Clear profile when unlinked
        if (wasLinked && !nowLinked) {
          displayName.value = null;
          profilePicture.value = null;
        }
      } catch {
        // Ignore errors
      }
    }, 3000);

    cleanup(() => {
      active = false;
      clearInterval(linkPoll);
      stopBackup();
    });
  });

  const isActive = (path: string) => loc.url.pathname === path;

  return (
    <div class="min-h-screen flex flex-col">
      <header class="bg-gray-900 border-b border-gray-800 px-6 py-3 flex items-center justify-between">
        <div class="flex items-center gap-6">
          <Link href="/" class="text-xl font-bold text-white hover:text-indigo-400">
            ProofPoll
          </Link>
          {status.value?.ready && (
            <nav class="flex gap-4">
              <Link
                href="/"
                class={`text-sm ${isActive("/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
              >
                Polls
              </Link>
              {linked.value ? (
                <Link
                  href="/create/"
                  class={`text-sm ${isActive("/create/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Create
                </Link>
              ) : (
                <button
                  type="button"
                  onClick$={() => (showSignIn.value = true)}
                  class={`text-sm ${isActive("/create/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Create
                </button>
              )}
              <Link
                href="/identity/"
                class={`text-sm ${isActive("/identity/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
              >
                Identity
              </Link>
            </nav>
          )}
        </div>
        {status.value?.ready &&
          status.value.agent_pub_key &&
          (linked.value && displayName.value ? (
            <div class="flex items-center gap-2">
              <span class="text-sm text-gray-300">{displayName.value}</span>
              {profilePicture.value ? (
                <img
                  src={profilePicture.value}
                  alt="Profile"
                  class="h-8 w-8 rounded-full object-cover border border-gray-600"
                  width={32}
                  height={32}
                />
              ) : (
                <div class="flex h-8 w-8 items-center justify-center rounded-full bg-indigo-600 text-sm font-medium text-white">
                  {displayName.value.charAt(0).toUpperCase()}
                </div>
              )}
            </div>
          ) : (
            <a href="/identity/?link=true">
              <img
                src="/assets/flowsta-signin.svg"
                alt="Sign in with Flowsta"
                width={158}
                height={36}
                class="hover:opacity-80 transition-opacity"
              />
            </a>
          ))}
      </header>

      <main class="flex-1 p-6">
        {!status.value ? (
          <div class="flex items-center justify-center h-64">
            <div class="text-gray-400">Connecting...</div>
          </div>
        ) : !status.value.ready ? (
          <div class="flex flex-col items-center justify-center h-64 gap-4">
            <div class="w-8 h-8 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
            <div class="text-gray-400">
              {status.value.conductor_status.status === "starting"
                ? status.value.conductor_status.message
                : status.value.conductor_status.status === "error"
                  ? `Error: ${status.value.conductor_status.message}`
                  : "Starting conductor..."}
            </div>
          </div>
        ) : (
          <Slot />
        )}
      </main>

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
            <a
              href="/identity/?link=true&returnTo=/create/"
              class="inline-block"
            >
              <img
                src="/assets/flowsta-signin.svg"
                alt="Sign in with Flowsta"
                width={158}
                height={36}
                class="hover:opacity-80 transition-opacity mx-auto"
              />
            </a>
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
    </div>
  );
});
