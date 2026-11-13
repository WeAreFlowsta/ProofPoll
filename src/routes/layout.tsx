import { component$, Slot, useContextProvider, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { Link, useLocation, useNavigate } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { linkedContext, linkStateContext, displayNameContext, profilePictureContext, type LinkState } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import { setSignInIntent, vaultLinkOptions } from "~/lib/signin";
import {
  getLinkedAgents,
  identityLinkedOnActive,
  getIdentityLink,
  commitIdentityLink,
  revokeIdentityLink,
  getMigrationStatus,
  getCachedProfile,
  getSeedEscrowState,
  saveProfileCache,
  type MigrationState,
} from "~/lib/holochain";
import { getFlowstaLinkStatus } from "@flowsta/holochain";

/** What `probe_vault` (Rust) found: this user's Vault, unlocked first. */
interface VaultProbe {
  port: number;
  url: string;
  unlocked: boolean;
  initialized: boolean;
  agent_pub_key: string | null;
  display_name: string | null;
  profile_picture: string | null;
}
import { setBackupTriggersEnabled, reportBackupOutcome } from "~/lib/backup";


interface AppStatus {
  ready: boolean;
  agent_pub_key: string | null;
  /** The key v1.4 runs as; differs only for a conductor-born 0.6 agent. */
  agent_pub_key_07?: string | null;
  conductor_status:
    | { status: "stopped" }
    | { status: "starting"; message: string }
    | { status: "restoring"; waited_secs: number; missing?: number }
    | { status: "ready"; admin_port: number; app_port: number }
    | { status: "error"; message: string };
  relaunched_into_identity?: boolean;
  /** This start's agent links are on v1.5 (the move carried them). */
  links_carried?: boolean;
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
  // "Open as this identity" relaunched us into a profile that may not be
  // signed in yet. Start the sign-in once, so the person is left with only
  // the approval in the Vault (a Windows drive, 2026-09-28).
  const autoSignInDone = useSignal(false);
  useContextProvider(linkedContext, linked);
  useContextProvider(linkStateContext, linkState);
  useContextProvider(displayNameContext, displayName);
  useContextProvider(profilePictureContext, profilePicture);
  const loc = useLocation();
  const nav = useNavigate();
  const showSignIn = useSignal(false);
  const migration = useSignal<MigrationState | null>(null);
  const migrationDismissed = useSignal(false);
  // The app's own updates: one downloaded and checked, waiting for a
  // restart; and, once after an update, what this copy was updated to.
  const updateReady = useSignal<{ version: string; notes_url: string; changes_line: boolean; asks_password: boolean } | null>(null);
  // "Restart to update" under way: the step it is at (set on the click,
  // then from `update-stage`), or "failed" when the install did not happen.
  const updateStage = useSignal<"password" | "installing" | "restarting" | "failed" | null>(null);
  const updatedFrom = useSignal<{ version: string; to: string; notes_url: string } | null>(null);
  const disconnecting = useSignal(false);
  // Step 2 of an adopt/re-key: the key was restored, the sign-in hasn't
  // happened yet (the adopt-relink-pending marker is on disk).
  const relinkPending = useSignal(false);
  // Signed in, but the active network has no link of this install's key to
  // the identity, made there or carried there: other people cannot see
  // that what it writes there is this person's. Asked for from the banner,
  // never in the background (a Vault dialog nobody started reads as the
  // Vault forgetting the app).
  const linkNeeded = useSignal(false);
  const linkConfirming = useSignal(false);
  const linkConfirmFailed = useSignal(false);

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

    // The move to this version reports as it goes: the banner's count rises
    // while it runs, and the banner goes when it is done.
    const unlistenMigration: (() => void)[] = [];
    listen("migration-progress", async () => {
      try {
        const ms = await getMigrationStatus();
        if (ms.status === "InProgress") migration.value = ms;
      } catch {
        // The next event tries again.
      }
    }).then((unlisten) => unlistenMigration.push(unlisten));
    listen("migration-complete", () => {
      migration.value = null;
    }).then((unlisten) => unlistenMigration.push(unlisten));

    invoke<{ ready: typeof updateReady.value; updated: typeof updatedFrom.value }>("update_status")
      .then((st) => {
        updateReady.value = st.ready;
        updatedFrom.value = st.updated;
      })
      .catch(() => {});
    listen<NonNullable<typeof updateReady.value>>("update-ready", (event) => {
      updateReady.value = event.payload;
    }).then((unlisten) => unlistenMigration.push(unlisten));
    listen<"password" | "installing" | "restarting">("update-stage", (event) => {
      updateStage.value = event.payload;
    }).then((unlisten) => unlistenMigration.push(unlisten));

    const startBackup = async () => {
      // Write-triggered backups share the auto-backup's on/off signal: a
      // CONFIRMED identity. The hourly run below covers long sessions; the
      // write triggers (lib/backup.ts) cover "created a poll, closed the
      // laptop" - without them, new data waits for the next launch.
      setBackupTriggersEnabled(true);
      if (stopAutoBackup) return; // Already running
      try {
        const { startAutoBackup } = await import("@flowsta/holochain");
        // Use the canonical-shape payload (v0.2.0+ — see
        // the backup plan notes). The Rust side queries
        // the user's polls and votes, builds the canonical payload, returns
        // it as JSON. Vault recognises the shape and renders per-entry-type
        // counts in the UI plus inlines human_readable views in CAL §4.2.1
        // exports — see backup.rs::is_canonical_backup over in flowsta-vault.
        const pin = await vaultLinkOptions();
        stopAutoBackup = startAutoBackup({
          clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
          appName: "ProofPoll",
          ...pin,
          intervalMinutes: 60,
          // clientId rides along so the Rust side can check the Vault's
          // escrowed seed BEFORE building a payload - without that gate, a
          // fresh install's first backup would overwrite the seed a
          // machine-death restore needs (see seed_adopt::backup_escrow_gate).
          getData: () =>
            invoke("build_canonical_backup", {
              clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
            }),
          onSuccess: (r) => {
            console.log(`[ProofPoll] Vault backup: ${r.dataSize} bytes`);
            reportBackupOutcome(true, "saved", r.dataSize);
          },
          onError: (e) => {
            console.warn("[ProofPoll] Vault backup skipped:", e.message);
            reportBackupOutcome(false, e.message);
          },
        });
      } catch {
        // SDK import failed — ignore
      }
    };

    const stopBackup = () => {
      setBackupTriggersEnabled(false);
      if (stopAutoBackup) {
        stopAutoBackup();
        stopAutoBackup = null;
      }
    };

    // ONE state computation for both poll loops - two copies of this logic
    // is how they drifted apart before.
    //
    // Inputs, in order of authority:
    //   • Local file — `getIdentityLink()` holds the Vault identity this
    //     install is bound to. Its `vault_agent_pub_key` vs the LIVE
    //     /status.agent_pub_key is the PRIMARY signal: /link-status matches
    //     on the app's agent key only and carries no vault identity, so it
    //     cannot detect an account switch (and the Vault's linked-apps file
    //     survives one).
    //   • DHT entry — `getLinkedAgents` non-empty if an `IsSamePersonEntry`
    //     was committed.
    //   • Vault liveness/opinion — `getFlowstaLinkStatus()` demoted to a
    //     liveness + app-link probe.
    const computeLinkState = async (agentPubKey: string): Promise<LinkState> => {
      const dhtLinked = await getLinkedAgents(agentPubKey)
        .then((a) => a.length > 0)
        .catch(() => false);
      const localLink = await getIdentityLink().catch(() => null);
      if (!dhtLinked && !localLink) return "unlinked";

      // Primary: compare the linked identity against whoever is actually
      // unlocked right now.
      // The same probe the Rust identity gate uses: three ports, another
      // OS user's Vault ignored, the unlocked one preferred - the page can
      // never judge against a different Vault than the gate does.
      let vaultLive: VaultProbe | null = null;
      try {
        vaultLive = await invoke<VaultProbe | null>("probe_vault");
      } catch {
        // Vault not reachable
      }
      if (vaultLive?.unlocked && vaultLive.agent_pub_key && localLink) {
        // The Vault's identity against the one this profile is linked to
        // is the whole question. Equal = linked, whatever the Vault's own
        // linked-apps list says: that list is erased by Reset Vault and
        // rebuilt lazily, so a restored Vault "forgets" this app while
        // holding exactly the identity it belongs to (seen 2026-09-28:
        // the "someone else" banner on the matching identity).
        if (vaultLive.agent_pub_key === localLink.vault_agent_pub_key) {
          return "linked";
        }
        return "mismatch";
      }
      if (!vaultLive) return "offline";
      // A Vault that is locked, or holds no identity at all (just reset,
      // mid-setup), cannot vouch for or against anyone. Asking it whether
      // this app is linked would come back "no" and read as "someone else"
      // (seen 2026-09-28: a reset Vault raised the mismatch banner while
      // the Rust gate, correctly, still allowed writes). No banner.
      if (!vaultLive.unlocked || vaultLive.initialized === false || !vaultLive.agent_pub_key) {
        return "offline";
      }

      const vaultStatus = await getFlowstaLinkStatus({
        clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
        localAgentPubKey: agentPubKey,
        ...(await vaultLinkOptions()),
      }).catch(() => ({ state: "offline" as const }));
      if (vaultStatus.state === "linked") return "linked";
      if (vaultStatus.state === "offline") return "offline";
      // Vault is running and says no link for this app's agent - the user
      // unlinked deliberately or switched accounts. Surface the choice via
      // the banner; never auto-revoke their data.
      return "mismatch";
    };

    // Once a session, after this start's agent links are on v1.5 (a check
    // before the move carried them would find none): does the active
    // network link this install's key to the identity? Asked again on the
    // link poll's ticks until the network answers.
    let activeLinkChecked = false;
    let activeLinkChecking = false;
    const checkActiveLink = async (linkKey: string) => {
      if (activeLinkChecked || activeLinkChecking || linkState.value !== "linked") return;
      activeLinkChecking = true;
      try {
        const s = await invoke<AppStatus>("get_app_status");
        if (!s.links_carried) return;
        const ok = await identityLinkedOnActive(linkKey);
        activeLinkChecked = true;
        linkNeeded.value = !ok;
      } catch {
        // The network did not answer: the next tick asks again.
      } finally {
        activeLinkChecking = false;
      }
    };

    const poll = async () => {
      while (active) {
        try {
          const s = await invoke<AppStatus>("get_app_status");
          status.value = s;
          if (s.ready) {
            // Local marker check only (no Vault round-trip): did an
            // adopt/re-key commit without its step-2 sign-in yet?
            try {
              const report = await getSeedEscrowState(null);
              relinkPending.value = report.relink_pending;
            } catch {
              // Marker unreadable — the identity page still offers the flow
            }
            // The key the active network (v1.5) runs as: its link is made
            // for that key (the 0.7 line's, when the 0.6 one was
            // conductor-born).
            const linkKey = s.agent_pub_key_07 ?? s.agent_pub_key;
            if (linkKey) {
              const state = await computeLinkState(linkKey);
              linkState.value = state;
              // Basic writes need a signed-in install, not a live Vault -
              // `offline` (linked, Vault locked or closed) is writable.
              // `mismatch` and `unlinked` stay read-only (the Rust gates
              // enforce this regardless; the UI just tells the truth).
              linked.value = state === "linked" || state === "offline";
              if (
                s.relaunched_into_identity &&
                state === "unlinked" &&
                !autoSignInDone.value
              ) {
                autoSignInDone.value = true;
                try {
                  const live = await invoke<VaultProbe | null>("probe_vault");
                  if (live?.unlocked && live.agent_pub_key) {
                    setSignInIntent({ autoLink: true });
                    nav("/identity/");
                  }
                } catch {
                  // Vault not reachable: the Identity page still offers it.
                }
              }

              // Signed in, but the active network may hold no link of this
              // key (a key that never linked on an older version either):
              // the banner offers it once the move's links are carried.
              checkActiveLink(linkKey);
            }

            // Load profile: cache first, then Vault refresh.
            // The cached name/picture DISPLAY when linked or offline (who
            // this install is connected to), but the cache is only ever
            // REFRESHED in the confirmed-linked state - the Rust side
            // refuses save_profile_cache on an identity mismatch anyway,
            // because this cache is what public votes publish to the DHT.
            if (linkState.value === "linked" || linkState.value === "offline") {
              try {
                const cached = await getCachedProfile();
                if (cached) {
                  if (cached.display_name) displayName.value = cached.display_name;
                  if (cached.profile_picture) profilePicture.value = cached.profile_picture;
                }
              } catch {
                // No cache yet
              }
            }
            if (linkState.value === "linked") {
              try {
                const vault = await invoke<VaultProbe | null>("probe_vault");
                if (vault) {
                  if (vault.display_name) {
                    displayName.value = vault.display_name;
                    if (vault.profile_picture)
                      profilePicture.value = vault.profile_picture;
                    // Save to cache for next startup (Rust re-verifies the
                    // identity before writing)
                    await saveProfileCache(
                      vault.display_name,
                      vault.profile_picture || null,
                    ).catch(() => {});
                  }
                }
              } catch {
                // Vault not running — cached profile (if any) is already loaded
              }
            }
            // Backups run for any signed-in install - "offline" included:
            // the Vault accepts backup posts while locked (once unlocked
            // this session), and every guard fails closed when it can't
            // confirm the slot. Mismatch/unlinked stay stopped.
            if (linkState.value === "linked" || linkState.value === "offline") {
              startBackup();
            }

            // Check migration status
            try {
              const ms = await getMigrationStatus();
              // Only while the move runs. Votes parked for a poll whose author
              // has not moved it yet still show and count where they are, so
              // the app never tells anyone it waits on other people.
              if (ms.status === "InProgress") {
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
      const linkKey = s?.agent_pub_key_07 ?? s?.agent_pub_key;
      if (!s?.ready || !linkKey) return;
      try {
        // Same state-machine as the initial poll — recompute on every tick.
        // The user could have locked / unlocked Vault, switched accounts,
        // or unlinked from the Vault UI at any moment, and the layout
        // banner needs to reflect it within the polling cadence.
        const wasLinked = linked.value;
        const nextState = await computeLinkState(linkKey);

        linkState.value = nextState;
        const nowLinked = nextState === "linked";
        // Signed-in installs are writable without an unlock; only
        // mismatch/unlinked are read-only.
        linked.value = nowLinked || nextState === "offline";

        // Backups follow writability (both are idempotent to call).
        if (linked.value) startBackup();
        else stopBackup();

        // A completed sign-in retires the step-2 restore prompt (the Rust
        // side clears the marker in commit_identity_link).
        if (nowLinked) relinkPending.value = false;

        // The active network's link: asked once linked (again after a
        // switch or a sign-out, which can change the answer).
        if (nowLinked) checkActiveLink(linkKey);
        else {
          activeLinkChecked = false;
          linkNeeded.value = false;
        }

        // Fetch profile when linked but profile is missing. Cache display
        // is fine in `offline` too; refreshing the cache is linked-only
        // (Rust refuses it otherwise).
        if ((nowLinked || nextState === "offline") && !displayName.value) {
          try {
            const cached = await getCachedProfile();
            if (cached?.display_name) {
              displayName.value = cached.display_name;
              if (cached.profile_picture) profilePicture.value = cached.profile_picture;
            }
          } catch {}
          if (nowLinked && !displayName.value) {
            try {
              const vault = await invoke<VaultProbe | null>("probe_vault");
              if (vault) {
                if (vault.display_name) {
                  displayName.value = vault.display_name;
                  if (vault.profile_picture)
                    profilePicture.value = vault.profile_picture;
                  await saveProfileCache(
                    vault.display_name,
                    vault.profile_picture || null,
                  ).catch(() => {});
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
    }, 15000);

    cleanup(() => {
      active = false;
      clearInterval(linkPoll);
      stopBackup();
      if (unlistenStatus) unlistenStatus();
      unlistenMigration.forEach((unlisten) => unlisten());
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

  // Step 2 of the seed-escrow upgrade / restore: the key changed and the
  // new one must be linked to the identity again - the ordinary sign-in.
  const handleRelinkSignIn = $(() => {
    setSignInIntent({ autoLink: true });
    nav("/identity/");
  });
  // The link banner's action: the Vault signs the link of the key the
  // active network runs as, and it is written there. commit_identity_link
  // refuses any identity but the one this install is linked to.
  const handleConfirmLink = $(async () => {
    const s = status.value;
    const linkKey = s?.agent_pub_key_07 ?? s?.agent_pub_key;
    if (!linkKey || linkConfirming.value) return;
    linkConfirming.value = true;
    linkConfirmFailed.value = false;
    try {
      const { linkFlowstaIdentity } = await import("@flowsta/holochain");
      const result = await linkFlowstaIdentity({
        appName: "ProofPoll",
        clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
        localAgentPubKey: linkKey,
        ...(await vaultLinkOptions()),
      });
      if (result.success) {
        await commitIdentityLink(result.payload.vaultAgentPubKey, result.payload.vaultSignature);
        linkNeeded.value = false;
      } else {
        linkConfirmFailed.value = true;
      }
    } catch {
      linkConfirmFailed.value = true;
    } finally {
      linkConfirming.value = false;
    }
  });
  // A switch mid-session: relaunch, and ProofPoll opens on the profile of
  // the identity the Vault holds now (picked at launch) instead of staying
  // read-only under the old name.
  const handleRestartIntoIdentity = $(async () => {
    await invoke("restart_app").catch(() => {});
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
                data-testid="nav-polls"
                class={`text-sm ${isActive("/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
              >
                Polls
              </Link>
              {linked.value ? (
                <Link
                  href="/create/"
                  data-testid="nav-create"
                  class={`text-sm ${isActive("/create/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Create
                </Link>
              ) : (
                <button
                  type="button"
                  data-testid="nav-create"
                  onClick$={() => (showSignIn.value = true)}
                  class={`text-sm ${isActive("/create/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Create
                </button>
              )}
              {linked.value && (
                <Link
                  href="/drafts/"
                  data-testid="nav-drafts"
                  class={`text-sm ${isActive("/drafts/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
                >
                  Drafts
                </Link>
              )}
              <Link
                href="/identity/"
                data-testid="nav-settings"
                class={`text-sm ${loc.url.pathname.startsWith("/identity/") || loc.url.pathname.startsWith("/settings/") ? "text-indigo-400 font-medium" : "text-gray-400 hover:text-gray-200"}`}
              >
                Settings
              </Link>
            </nav>
          )}
        </div>
        <div class="flex items-center gap-3">
        {updateReady.value && (
          <button
            type="button"
            data-testid="nav-restart-to-update"
            disabled={updateStage.value !== null && updateStage.value !== "failed"}
            title={
              updateReady.value.asks_password
                ? `Installs ProofPoll ${updateReady.value.version}, then closes and opens ProofPoll again. Your computer asks for your password first.`
                : `Installs ProofPoll ${updateReady.value.version}, then closes and opens ProofPoll again.`
            }
            class="flex items-center gap-2 rounded-full bg-indigo-600 px-3 py-1 text-sm font-medium text-white transition-colors hover:bg-indigo-500 disabled:cursor-default disabled:bg-indigo-600/80"
            onClick$={async () => {
              // Said at once, before the password prompt or the conductors' stop.
              updateStage.value = updateReady.value!.asks_password ? "password" : "installing";
              try {
                await invoke("restart_to_update");
              } catch (e) {
                console.error("Update did not install:", e);
                updateStage.value = "failed";
              }
            }}
          >
            {updateStage.value !== null && updateStage.value !== "failed" && (
              <span class="h-3.5 w-3.5 animate-spin rounded-full border-2 border-white border-t-transparent" />
            )}
            {updateStage.value === "restarting"
              ? "Reopening…"
              : updateStage.value !== null && updateStage.value !== "failed"
                ? "Installing…"
                : "Restart to update"}
          </button>
        )}
        {status.value?.ready &&
          status.value.agent_pub_key &&
          (linkState.value === "unlinked" ? (
            <button
              type="button"
              data-testid="nav-signin"
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
              data-testid="nav-profile"
              title={
                linkState.value === "mismatch"
                  ? "From a previous Flowsta connection. Reconnect or disconnect via the banner below."
                  : undefined
              }
            >
              {displayName.value && (
                <span
                  data-testid="nav-profile-name"
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
        </div>
      </header>

      <main class="flex-1 p-6">
        {!status.value ? (
          <div class="flex items-center justify-center h-64" data-testid="nav-connecting">
            <div class="text-gray-400">Connecting...</div>
          </div>
        ) : !status.value.ready ? (
          <div class="flex flex-col items-center justify-center min-h-64 py-8 gap-4">
            {status.value.conductor_status.status === "error" ? (
              <>
                <div class="w-12 h-12 rounded-full bg-red-900/40 flex items-center justify-center">
                  <svg class="w-6 h-6 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width={2}>
                    <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                  </svg>
                </div>
                <div class="text-center max-w-md" data-testid="nav-connection-lost">
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
            ) : status.value.conductor_status.status === "restoring" ? (
              <>
                <div class="w-8 h-8 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
                <div class="text-gray-300" data-testid="nav-restoring">
                  Restoring your polls and votes from the network
                </div>
                {(status.value.conductor_status.missing ?? 0) === 0 && (
                  <p class="text-gray-500 text-sm max-w-sm text-center">
                    This device now uses your restored key. ProofPoll is fetching
                    everything that key published from other ProofPoll users and
                    the ProofPoll node, and opens when it has all of it.
                  </p>
                )}
                {(status.value.conductor_status.missing ?? 0) > 0 && (
                  <div class="flex flex-col items-center gap-3 max-w-sm" data-testid="nav-restoring-short">
                    <p class="text-amber-300 text-sm text-center">
                      {status.value.conductor_status.missing === 1
                        ? "The last record your other device wrote has not reached the network yet."
                        : `The last ${status.value.conductor_status.missing} records your other device wrote have not reached the network yet.`}
                    </p>
                    <p class="text-gray-400 text-sm text-center">
                      Open ProofPoll on that device and leave it open for a few
                      minutes, then press Try again here. If it was already
                      open, close it and open it again first. If that device is
                      gone, you can continue without
                      {status.value.conductor_status.missing === 1 ? " it" : " them"}.
                    </p>
                    <div class="flex gap-3">
                      <button
                        data-testid="nav-restoring-go-on"
                        onClick$={async () => {
                          await invoke("restore_go_on");
                        }}
                        class="border border-gray-600 hover:border-gray-400 text-gray-200 px-5 py-2 rounded-full text-sm font-medium"
                      >
                        {status.value.conductor_status.missing === 1 ? "Continue without it" : "Continue without them"}
                      </button>
                      <button
                        data-testid="nav-restoring-try-again"
                        onClick$={async () => {
                          await invoke("restore_try_again");
                        }}
                        class="bg-indigo-600 hover:bg-indigo-500 text-white px-5 py-2 rounded-full text-sm font-medium"
                      >
                        Try again
                      </button>
                    </div>
                  </div>
                )}
                {status.value.conductor_status.waited_secs >= 120 && (status.value.conductor_status.missing ?? 0) === 0 && (
                  <p class="text-gray-500 text-xs max-w-sm text-center" data-testid="nav-restoring-slow">
                    Still restoring after {Math.floor(status.value.conductor_status.waited_secs / 60)} minutes.
                    It needs other ProofPoll users or the ProofPoll node to be
                    reachable, and keeps trying. You can leave ProofPoll open.
                  </p>
                )}
              </>
            ) : (
              <>
                <div class="w-8 h-8 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
                <div class="text-gray-400" data-testid="nav-starting">
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
                  <span>Bringing your polls and votes over to this version... ({migration.value.polls_migrated.length} poll{migration.value.polls_migrated.length !== 1 ? "s" : ""} so far)</span>
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
                doesn't recognize this app's agent. Two ways out: open
                ProofPoll as the identity the Vault holds now (its own
                profile), or deliberately disconnect. Re-binding THIS
                profile's history to the new identity is a two-step on the
                Identity page (disconnect, then connect), never a banner
                button: pressed by mistake it re-labels one identity's polls
                and votes as another's. We never auto-revoke. */}
            {linkState.value === "mismatch" && (
              <div class="bg-amber-900/30 border border-amber-800/50 rounded-lg px-4 py-3 mb-4" data-testid="nav-mismatch-banner">
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
                      Your Vault is signed in as someone else
                    </p>
                    <p class="mt-1 text-xs text-amber-300/90">
                      ProofPoll opened as a different Flowsta identity.
                      Everything here is safe. Open ProofPoll as the identity
                      your Vault holds now (its own polls and votes), or
                      switch your Vault back. To move this ProofPoll's polls
                      and votes to the identity your Vault holds now,
                      disconnect first, then connect from the Identity page.
                    </p>
                    <div class="mt-3 flex flex-wrap gap-2">
                      <button
                        type="button"
                        onClick$={handleRestartIntoIdentity}
                        class="inline-flex items-center rounded-md bg-amber-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-amber-500"
                      >
                        Open as this identity
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

            {/* Step 2 of the restore story: the key was adopted and the app
                restarted; finishing means signing back in. Stands down as
                soon as a sign-in completes (the mismatch banner outranks it
                - one banner voice at a time). */}
            {relinkPending.value &&
              linkState.value !== "linked" &&
              linkState.value !== "mismatch" && (
                <div class="bg-indigo-900/30 border border-indigo-800/50 rounded-lg px-4 py-3 mb-4" data-testid="nav-relink-banner">
                  <p class="text-sm font-medium text-indigo-200">
                    Step 2: sign back in
                  </p>
                  <p class="mt-1 text-xs text-indigo-300/90">
                    Your backup is restored. Sign in with Flowsta to finish.
                    Your polls and votes come back as the network syncs,
                    sometimes a few minutes.
                  </p>
                  <div class="mt-3">
                    <button
                      type="button"
                      onClick$={handleRelinkSignIn}
                      class="inline-flex items-center rounded-md bg-indigo-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-500"
                    >
                      Sign in with Flowsta
                    </button>
                  </div>
                </div>
              )}

            {linkNeeded.value && linkState.value === "linked" && (
              <div class="bg-indigo-900/30 border border-indigo-800/50 rounded-lg px-4 py-3 mb-4" data-testid="nav-link-banner">
                <p class="text-sm font-medium text-indigo-200">
                  Confirm this device in your Vault
                </p>
                <p class="mt-1 text-xs text-indigo-300/90">
                  Once, so other people can see that the polls and votes you
                  make on this device are yours.
                </p>
                {linkConfirmFailed.value && (
                  <p class="mt-1 text-xs text-amber-300/90">
                    Your Vault did not confirm it. Try again with your Vault
                    open and unlocked.
                  </p>
                )}
                <div class="mt-3">
                  <button
                    type="button"
                    onClick$={handleConfirmLink}
                    disabled={linkConfirming.value}
                    data-testid="nav-link-confirm"
                    class="inline-flex items-center rounded-md bg-indigo-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-500 disabled:opacity-60"
                  >
                    {linkConfirming.value ? "Waiting for your Vault..." : "Confirm in Vault"}
                  </button>
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
            data-testid="nav-signin-dialog"
            onClick$={(e) => e.stopPropagation()}
          >
            <h2 class="text-lg font-semibold text-white mb-2">Sign in required</h2>
            <p class="text-gray-400 text-sm mb-6">
              Sign in with Flowsta to create and vote on polls.
            </p>
            <button
              type="button"
              data-testid="nav-signin-dialog-signin"
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
              data-testid="nav-signin-dialog-cancel"
              onClick$={() => (showSignIn.value = false)}
              class="mt-4 text-sm text-gray-500 hover:text-gray-300 block mx-auto"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* The app's messages, one stack at the bottom right, each on a solid
          panel so nothing behind shows through. Dialogs (z-50) cover it. */}
      <div class="pointer-events-none fixed bottom-6 right-6 z-40 flex w-80 flex-col gap-3">
      {/* Once after an update: what changed */}
      {updatedFrom.value && (
        <div data-testid="nav-updated" class="pointer-events-auto flex items-start gap-3 rounded-lg border border-indigo-500/40 bg-indigo-950 px-4 py-3 shadow-xl">
          <div class="min-w-0 flex-1">
            <p class="text-sm text-indigo-200">Updated to ProofPoll {updatedFrom.value.to}</p>
            {updatedFrom.value.notes_url && (
              <button
                type="button"
                class="mt-0.5 text-sm font-medium text-indigo-300 underline hover:text-indigo-200"
                onClick$={async () => {
                  const { open } = await import("@tauri-apps/plugin-shell");
                  await open(updatedFrom.value!.notes_url);
                }}
              >
                What changed
              </button>
            )}
          </div>
          <button
            type="button"
            aria-label="Dismiss"
            class="shrink-0 text-indigo-300 hover:text-white"
            onClick$={async () => {
              updatedFrom.value = null;
              await invoke("dismiss_updated").catch(() => {});
            }}
          >
            ×
          </button>
        </div>
      )}

      {/* "Restart to update" under way: what happens next */}
      {updateReady.value && updateStage.value && (
        <div
          class={`pointer-events-auto flex items-start gap-2.5 rounded-lg border px-4 py-3 shadow-xl ${
            updateStage.value === "failed" ? "border-red-500/40 bg-red-950" : "border-indigo-500/40 bg-gray-800"
          }`}
        >
          {updateStage.value !== "failed" && (
            <div class="mt-0.5 h-4 w-4 shrink-0 animate-spin rounded-full border-2 border-indigo-400 border-t-transparent" />
          )}
          <div class="min-w-0 flex-1">
            {updateStage.value === "password" ? (
              <>
                <p class="text-sm text-gray-100">Installing ProofPoll {updateReady.value.version}</p>
                <p class="mt-0.5 text-xs text-gray-400">Your computer asks for your password to install it. Then ProofPoll closes and opens again.</p>
              </>
            ) : updateStage.value === "installing" ? (
              <>
                <p class="text-sm text-gray-100">Installing ProofPoll {updateReady.value.version}</p>
                <p class="mt-0.5 text-xs text-gray-400">Then ProofPoll closes and opens again.</p>
              </>
            ) : updateStage.value === "restarting" ? (
              <p class="text-sm text-gray-100">Installed. ProofPoll is closing and opening again.</p>
            ) : (
              <>
                <p class="text-sm text-red-200">ProofPoll {updateReady.value.version} was not installed</p>
                <p class="mt-0.5 text-xs text-gray-400">Nothing changed. Press Restart to update to try again.</p>
              </>
            )}
          </div>
          {updateStage.value === "failed" && (
            <button type="button" aria-label="Dismiss" class="shrink-0 text-gray-400 hover:text-white" onClick$={() => (updateStage.value = null)}>
              ×
            </button>
          )}
        </div>
      )}
      </div>
    </div>
  );
});
