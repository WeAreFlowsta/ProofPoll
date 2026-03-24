import { component$, Slot, useContextProvider, useSignal, useVisibleTask$ } from "@builder.io/qwik";
import { Link, useLocation } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { linkedContext, displayNameContext, profilePictureContext } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import { getLinkedAgents, getIdentityLink, commitIdentityLink, getMigrationStatus, getCachedProfile, saveProfileCache, type MigrationState } from "~/lib/holochain";


interface AppStatus {
  ready: boolean;
  agent_pub_key: string | null;
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
  const migration = useSignal<MigrationState | null>(null);
  const migrationDismissed = useSignal(false);

  useVisibleTask$(({ cleanup }) => {
    let active = true;
    let stopAutoBackup: (() => void) | null = null;
    let unlistenStatus: (() => void) | null = null;

    // Listen for conductor-status events from the health monitor
    listen<AppStatus["conductor_status"]>("conductor-status", (event) => {
      const cs = event.payload;
      if (cs.status === "error") {
        status.value = {
          ready: false,
          agent_pub_key: status.value?.agent_pub_key ?? null,
          conductor_status: cs,
        };
      }
    }).then((unlisten) => {
      unlistenStatus = unlisten;
    });

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

              // If DHT doesn't show a link yet, check local persistence.
              // The local identity-link.json survives across DNA migrations
              // and app restarts. If it exists AND the Vault confirms the
              // link, trust it immediately — the DHT entry will catch up.
              if (!linked.value && s.agent_pub_key) {
                try {
                  const localLink = await getIdentityLink();
                  if (localLink) {
                    // Ask the Vault if it still recognizes this link
                    try {
                      const vaultResp = await fetch(
                        `http://127.0.0.1:27777/link-status?app_agent_pub_key=${encodeURIComponent(s.agent_pub_key)}`,
                        { signal: AbortSignal.timeout(2000) },
                      );
                      if (vaultResp.ok) {
                        const vaultData = await vaultResp.json();
                        if (vaultData.linked === true) {
                          // Vault confirms link — trust it, re-create DHT entry silently
                          linked.value = true;
                          console.log("[ProofPoll] Restored link from local state + Vault confirmation");

                          // Re-create the DHT entry in the background (non-blocking)
                          // so other peers can verify this identity link
                          import("@flowsta/holochain").then(async ({ linkFlowstaIdentity }) => {
                            try {
                              const result = await linkFlowstaIdentity({
                                appName: "ProofPoll",
                                clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
                                localAgentPubKey: s.agent_pub_key!,
                              });
                              if (result.success) {
                                await commitIdentityLink(
                                  result.payload.vaultAgentPubKey,
                                  result.payload.vaultSignature,
                                );
                                console.log("[ProofPoll] DHT identity link re-created after migration");
                              }
                            } catch {
                              // Vault dialog dismissed or not shown — link still works locally
                            }
                          }).catch(() => {});
                        }
                      }
                    } catch {
                      // Vault not running — if we have local file, trust it
                      // (user was previously linked, Vault just isn't open right now)
                      linked.value = true;
                      console.log("[ProofPoll] Restored link from local state (Vault not running)");
                    }
                  }
                } catch {
                  // No local link data — user genuinely not linked
                }
              }
            }

            // Load profile: cache first, then Vault refresh.
            // The Vault only needs to be running for the FIRST identity link.
            // After that, profile-cache.json has the display name and picture.
            // If the Vault is running, we refresh the cache in case the user
            // changed their name or picture. If not, cached data is fine.
            if (linked.value) {
              // 1. Load from local cache (works without Vault)
              try {
                const cached = await getCachedProfile();
                if (cached) {
                  if (cached.display_name) displayName.value = cached.display_name;
                  if (cached.profile_picture) profilePicture.value = cached.profile_picture;
                }
              } catch {
                // No cache yet
              }

              // 2. Try to refresh from Vault (may be locked or closed)
              try {
                const resp = await fetch("http://127.0.0.1:27777/status", {
                  signal: AbortSignal.timeout(2000),
                });
                if (resp.ok) {
                  const vault = await resp.json();
                  if (vault.display_name) {
                    displayName.value = vault.display_name;
                    if (vault.profile_picture)
                      profilePicture.value = vault.profile_picture;
                    // Save to cache for next startup
                    saveProfileCache(vault.display_name, vault.profile_picture || null);
                  }
                }
              } catch {
                // Vault not running — cached profile (if any) is already loaded
              }
              startBackup();
            }

            // Check migration status
            try {
              const ms = await getMigrationStatus();
              if (ms.status === "InProgress" || (ms.status === "Complete" && ms.votes_pending.length > 0)) {
                migration.value = ms;
              }
            } catch {
              // Migration status unavailable — ignore
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

        // Fallback: during DNA migration the new DHT has no entry yet.
        // Trust the local identity-link.json until the background re-creation
        // completes and getLinkedAgents starts returning results.
        if (!nowLinked) {
          try {
            const localLink = await getIdentityLink();
            if (localLink) nowLinked = true;
          } catch {
            // No local file — genuinely not linked
          }
        }

        linked.value = nowLinked;

        // Start/stop auto-backup based on link status
        if (nowLinked && !wasLinked) startBackup();
        if (!nowLinked && wasLinked) stopBackup();

        // Fetch profile when linked but profile is missing
        if (nowLinked && !displayName.value) {
          // Try cache first
          try {
            const cached = await getCachedProfile();
            if (cached?.display_name) {
              displayName.value = cached.display_name;
              if (cached.profile_picture) profilePicture.value = cached.profile_picture;
            }
          } catch {}
          // Then try Vault
          if (!displayName.value) {
            try {
              const resp = await fetch("http://127.0.0.1:27777/status", {
                signal: AbortSignal.timeout(2000),
              });
              if (resp.ok) {
                const vault = await resp.json();
                if (vault.display_name) {
                  displayName.value = vault.display_name;
                  if (vault.profile_picture)
                    profilePicture.value = vault.profile_picture;
                  saveProfileCache(vault.display_name, vault.profile_picture || null);
                }
              }
            } catch {
              // Vault not running
            }
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
      if (unlistenStatus) unlistenStatus();
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
              {linked.value && (
                <Link
                  href="/drafts/"
                  class={`text-sm ${isActive("/drafts/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Drafts
                </Link>
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
          (linked.value ? (
            <div class="flex items-center gap-2">
              {displayName.value && (
                <span class="text-sm text-gray-300">{displayName.value}</span>
              )}
              {sanitizeImageSrc(profilePicture.value) ? (
                <img
                  src={sanitizeImageSrc(profilePicture.value)!}
                  alt="Profile"
                  class="h-8 w-8 rounded-full object-cover border border-gray-600"
                  width={32}
                  height={32}
                />
              ) : (
                <div class="flex h-8 w-8 items-center justify-center rounded-full bg-indigo-600 text-sm font-medium text-white">
                  {displayName.value
                    ? displayName.value.charAt(0).toUpperCase()
                    : "F"}
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
            {status.value.conductor_status.status === "error" ? (
              <>
                <div class="w-12 h-12 rounded-full bg-red-900/40 flex items-center justify-center">
                  <svg class="w-6 h-6 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
                    <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                  </svg>
                </div>
                <div class="text-center max-w-md">
                  <h2 class="text-lg font-semibold text-white mb-1">Connection Lost</h2>
                  <p class="text-gray-400 text-sm mb-4">
                    {status.value.conductor_status.message}
                  </p>
                  <button
                    type="button"
                    onClick$={() => window.close()}
                    class="bg-indigo-600 hover:bg-indigo-500 text-white px-5 py-2 rounded-full text-sm font-medium"
                  >
                    Close App
                  </button>
                  <p class="text-gray-600 text-xs mt-2">Reopen ProofPoll after closing to reconnect.</p>
                </div>
              </>
            ) : (
              <>
                <div class="w-8 h-8 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
                <div class="text-gray-400">
                  {status.value.conductor_status.status === "starting"
                    ? status.value.conductor_status.message
                    : "Starting conductor..."}
                </div>
                <p class="text-gray-600 text-xs max-w-xs text-center">
                  The local Holochain node is starting up. This usually takes a few seconds.
                </p>
              </>
            )}
          </div>
        ) : (
          <>
            {migration.value && !migrationDismissed.value && (
              <div class="bg-indigo-900/30 border border-indigo-800/50 rounded-lg px-4 py-2 mb-4 flex items-center justify-between">
                <div class="text-sm text-indigo-300">
                  {migration.value.status === "InProgress" ? (
                    <span>Migrating your data to v1.1... ({migration.value.polls_migrated.length} polls migrated)</span>
                  ) : migration.value.votes_pending.length > 0 ? (
                    <span>{migration.value.votes_pending.length} vote{migration.value.votes_pending.length !== 1 ? "s" : ""} waiting for poll authors to upgrade</span>
                  ) : null}
                </div>
                <button
                  type="button"
                  onClick$={() => (migrationDismissed.value = true)}
                  class="text-indigo-400 hover:text-indigo-300 text-xs ml-4"
                >
                  Dismiss
                </button>
              </div>
            )}
            <Slot />
          </>
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
