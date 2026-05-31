import { component$, Slot, useContextProvider, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { Link, useLocation, useNavigate } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { linkedContext, linkStateContext, displayNameContext, profilePictureContext, type LinkState } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import { setSignInIntent } from "~/lib/signin";
import {
  getLinkedAgents,
  getIdentityLink,
  commitIdentityLink,
  revokeIdentityLink,
  getMigrationStatus,
  getCachedProfile,
  saveProfileCache,
  type MigrationState,
} from "~/lib/holochain";
import { getFlowstaLinkStatus } from "@flowsta/holochain";


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
  // Rich link state: 'linked' / 'offline' / 'mismatch' / 'unlinked'.
  // `linked.value` is permissive (linked || offline); the layout's banner
  // (further down) keys off `linkState.value === 'mismatch'` so individual
  // pages don't need to know about the account-changed flow.
  const linkState = useSignal<LinkState>("unlinked");
  useContextProvider(linkedContext, linked);
  useContextProvider(linkStateContext, linkState);
  useContextProvider(displayNameContext, displayName);
  useContextProvider(profilePictureContext, profilePicture);
  const loc = useLocation();
  const nav = useNavigate();
  const showSignIn = useSignal(false);
  const migration = useSignal<MigrationState | null>(null);
  const migrationDismissed = useSignal(false);
  const disconnecting = useSignal(false);

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
        // Use the canonical-shape payload (v0.2.0+ — see
        // build-docs/current/GENERIC_BACKUP_PLAN.md). The Rust side queries
        // the user's polls and votes, builds the canonical payload, returns
        // it as JSON. Vault recognises the shape and renders per-entry-type
        // counts in the UI plus inlines human_readable views in CAL §4.2.1
        // exports — see backup.rs::is_canonical_backup over in flowsta-vault.
        stopAutoBackup = startAutoBackup({
          clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
          appName: "ProofPoll",
          intervalMinutes: 60,
          getData: () => invoke("build_canonical_backup"),
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
            // Compute the rich link state in one pass — see context.ts for
            // why we don't just use a boolean.
            //
            // Three inputs:
            //   • DHT entry — `getLinkedAgents` returns a non-empty list if
            //     we previously committed an `IsSamePersonEntry` to our DHT.
            //   • Local file — `getIdentityLink()` returns a record if we
            //     stored the Vault's signature locally (survives DNA migration
            //     and app restarts).
            //   • Vault opinion — `getFlowstaLinkStatus()` returns one of
            //     `linked` / `unlinked` / `offline`, distinguishing "Vault
            //     says no" from "Vault not running".
            if (s.agent_pub_key) {
              const dhtLinked = await getLinkedAgents(s.agent_pub_key)
                .then((a) => a.length > 0)
                .catch(() => false);
              const hasLocalLink = await getIdentityLink()
                .then((l) => !!l)
                .catch(() => false);

              if (!dhtLinked && !hasLocalLink) {
                // No evidence either way — user has never linked.
                linkState.value = "unlinked";
                linked.value = false;
              } else {
                const vaultStatus = await getFlowstaLinkStatus({
                  clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
                  localAgentPubKey: s.agent_pub_key,
                });

                if (vaultStatus.state === "linked") {
                  linkState.value = "linked";
                  linked.value = true;

                  // Migration race: Vault confirms the link but the new
                  // DNA's DHT doesn't have an entry yet. Recreate it in
                  // the background so peers can verify this identity link.
                  if (!dhtLinked) {
                    const agentPubKey = s.agent_pub_key;
                    import("@flowsta/holochain")
                      .then(async ({ linkFlowstaIdentity }) => {
                        try {
                          const result = await linkFlowstaIdentity({
                            appName: "ProofPoll",
                            clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
                            localAgentPubKey: agentPubKey,
                          });
                          if (result.success) {
                            await commitIdentityLink(
                              result.payload.vaultAgentPubKey,
                              result.payload.vaultSignature,
                            );
                            console.log("[ProofPoll] DHT identity link re-created after migration");
                          }
                        } catch {
                          // Vault dialog dismissed — link still works locally
                        }
                      })
                      .catch(() => {});
                  }
                } else if (vaultStatus.state === "offline") {
                  // Vault not running. Trust local state — features stay
                  // enabled. The user is still themselves.
                  linkState.value = "offline";
                  linked.value = true;
                } else {
                  // Vault is running and says no link for this app's agent.
                  // Could mean the user unlinked deliberately OR switched
                  // Flowsta accounts. Either way, surface the choice to the
                  // user via the banner — DON'T auto-revoke their data.
                  linkState.value = "mismatch";
                  linked.value = false;
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
        // Same state-machine as the initial poll — recompute on every tick.
        // The user could have locked / unlocked Vault, switched accounts,
        // or unlinked from the Vault UI at any moment, and the layout
        // banner needs to reflect it within the 3s polling cadence.
        const wasLinked = linked.value;
        const dhtLinked = await getLinkedAgents(s.agent_pub_key)
          .then((a) => a.length > 0)
          .catch(() => false);
        const hasLocalLink = await getIdentityLink()
          .then((l) => !!l)
          .catch(() => false);

        let nextState: LinkState;
        if (!dhtLinked && !hasLocalLink) {
          nextState = "unlinked";
        } else {
          const vaultStatus = await getFlowstaLinkStatus({
            clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
            localAgentPubKey: s.agent_pub_key,
          });
          if (vaultStatus.state === "linked") nextState = "linked";
          else if (vaultStatus.state === "offline") nextState = "offline";
          else nextState = "mismatch";
        }

        linkState.value = nextState;
        const nowLinked = nextState === "linked" || nextState === "offline";
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

        // Clear profile when going to a fully-unlinked state. In `mismatch`
        // we deliberately keep the cached display name/picture so the user
        // can see who they used to be signed in as — but the header renders
        // them with a "stale" treatment.
        if (wasLinked && nextState === "unlinked") {
          displayName.value = null;
          profilePicture.value = null;
          await saveProfileCache(null, null).catch(() => {});
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

  /**
   * Disconnect handler: user has decided that the Vault account change is
   * permanent and they want to clean up. Revokes the DHT entry, clears the
   * profile cache, and drops to `unlinked` so the layout shows the standard
   * sign-in CTA again. Does NOT delete past polls/votes — those remain
   * attributable to ProofPoll's local agent.
   */
  const handleDisconnect = $(async () => {
    if (disconnecting.value) return;
    disconnecting.value = true;
    try {
      await revokeIdentityLink().catch(() => {});
      await saveProfileCache(null, null).catch(() => {});
      displayName.value = null;
      profilePicture.value = null;
      linkState.value = "unlinked";
      linked.value = false;
    } finally {
      disconnecting.value = false;
    }
  });

  const handleReconnect = $(() => {
    setSignInIntent({ autoLink: true });
    nav("/identity/");
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
          (linkState.value === "unlinked" ? (
            <button
              type="button"
              onClick$={() => {
                setSignInIntent({ autoLink: true });
                nav("/identity/");
              }}
              class="bg-transparent border-0 p-0 cursor-pointer"
            >
              <img
                src="/assets/flowsta-signin.svg"
                alt="Sign in with Flowsta"
                width={158}
                height={36}
                class="hover:opacity-80 transition-opacity"
              />
            </button>
          ) : (
            // linked / offline / mismatch — render the profile chip. In the
            // `mismatch` case it's grayed out with a tooltip; the banner
            // below explains the situation.
            <div
              class="flex items-center gap-2"
              title={
                linkState.value === "mismatch"
                  ? "From a previous Flowsta connection. Reconnect or disconnect via the banner below."
                  : undefined
              }
            >
              {displayName.value && (
                <span
                  class={[
                    "text-sm",
                    linkState.value === "mismatch"
                      ? "text-gray-500 line-through"
                      : "text-gray-300",
                  ].join(" ")}
                >
                  {displayName.value}
                </span>
              )}
              {sanitizeImageSrc(profilePicture.value) ? (
                <img
                  src={sanitizeImageSrc(profilePicture.value)!}
                  alt="Profile"
                  class={[
                    "h-8 w-8 rounded-full object-cover border border-gray-600",
                    linkState.value === "mismatch" ? "opacity-40 grayscale" : "",
                  ].join(" ")}
                  width={32}
                  height={32}
                />
              ) : (
                <div
                  class={[
                    "flex h-8 w-8 items-center justify-center rounded-full text-sm font-medium text-white",
                    linkState.value === "mismatch" ? "bg-gray-700" : "bg-indigo-600",
                  ].join(" ")}
                >
                  {displayName.value
                    ? displayName.value.charAt(0).toUpperCase()
                    : "F"}
                </div>
              )}
            </div>
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

            {/* Account-changed banner — shown when Vault is running but
                doesn't recognize this app's agent. The user can reconnect
                with their current Vault account or deliberately disconnect.
                We never auto-revoke; the user's polls + votes stay theirs. */}
            {linkState.value === "mismatch" && (
              <div class="bg-amber-900/30 border border-amber-800/50 rounded-lg px-4 py-3 mb-4">
                <div class="flex items-start gap-3">
                  <svg
                    class="h-5 w-5 shrink-0 text-amber-400 mt-0.5"
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                    stroke-width={2}
                    aria-hidden="true"
                  >
                    <path
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z"
                    />
                  </svg>
                  <div class="flex-1 min-w-0">
                    <p class="text-sm font-medium text-amber-200">
                      Your Flowsta account has changed
                    </p>
                    <p class="mt-1 text-xs text-amber-300/90">
                      ProofPoll was connected to a Flowsta account that no
                      longer matches the one in your Vault. Existing polls
                      and votes are still yours, but you'll need to
                      reconnect to create or vote on new ones.
                    </p>
                    <div class="mt-3 flex flex-wrap gap-2">
                      <button
                        type="button"
                        onClick$={handleReconnect}
                        class="inline-flex items-center rounded-md bg-amber-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-amber-500"
                      >
                        Connect with current account
                      </button>
                      <button
                        type="button"
                        disabled={disconnecting.value}
                        onClick$={handleDisconnect}
                        class="inline-flex items-center rounded-md border border-amber-700 px-3 py-1.5 text-xs font-medium text-amber-200 hover:bg-amber-900/40 disabled:opacity-50"
                      >
                        {disconnecting.value ? "Disconnecting..." : "Disconnect"}
                      </button>
                    </div>
                  </div>
                </div>
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

    </div>
  );
});
