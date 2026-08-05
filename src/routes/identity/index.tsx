import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { useNavigate } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { linkedContext, linkStateContext, displayNameContext, profilePictureContext } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import { readAndClearSignInIntent } from "~/lib/signin";
import { focusSelf } from "~/lib/window";
import {
  getLinkedAgents,
  getIdentityLink,
  commitIdentityLink,
  revokeIdentityLink,
  saveProfileCache,
  getSeedEscrowState,
  adoptEscrowedSeed,
  rekeyDeviceAgent,
  type SeedEscrowReport,
} from "~/lib/holochain";

export default component$(() => {
  const nav = useNavigate();
  const linkedCtx = useContext(linkedContext);
  const linkStateCtx = useContext(linkStateContext);
  const displayName = useContext(displayNameContext);
  const profilePicture = useContext(profilePictureContext);
  const safeReturnTo = useSignal<string | null>(null);
  const agentKey = useSignal<string | null>(null);
  const linkedVaultKey = useSignal<string | null>(null);
  const loading = useSignal(true);
  const linking = useSignal(false);
  const unlinking = useSignal(false);
  const error = useSignal<string | null>(null);
  const success = useSignal<string | null>(null);
  const autoLink = useSignal(false);
  const showDetails = useSignal(false);
  const confirmUnlink = useSignal(false);
  // This install's own binding (identity-link.json). The DHT can show a
  // link for the adopted agent while the local binding is gone (right
  // after a key restore) - the local file, not the DHT, is what decides
  // whether THIS install is signed in and whether auto-link may proceed.
  const hasLocalLink = useSignal(false);
  // ── Backups & recovery (agent-seed escrow) ──
  const seedReport = useSignal<SeedEscrowReport | null>(null);
  const escrowedHex = useSignal<string | null>(null);
  const escrowCheckFailed = useSignal(false);
  const confirmSeed = useSignal<"adopt" | "rekey" | null>(null);
  const seedBusy = useSignal(false);
  const seedError = useSignal<string | null>(null);
  const seedDismissed = useSignal(false);

  // Fetch the escrowed seed from the Vault backup (from the WEBVIEW - the
  // Vault authorizes backup reads by Origin) and compare against this
  // install's seed. A failed backup read never invents "synced": the
  // comparison still runs for the local half (legacy detection) and the
  // panel says the backup couldn't be checked.
  const loadSeedState = $(async () => {
    seedError.value = null;
    escrowCheckFailed.value = false;
    let escrowed: string | null = null;
    try {
      const { retrieveFromVault } = await import("@flowsta/holochain");
      const res = await retrieveFromVault({
        clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
      });
      const keys = (res?.data as { app_keys?: { device_seed_hex?: unknown } } | undefined)
        ?.app_keys;
      escrowed =
        typeof keys?.device_seed_hex === "string" ? keys.device_seed_hex : null;
    } catch {
      escrowCheckFailed.value = true;
    }
    escrowedHex.value = escrowed;
    try {
      seedReport.value = await getSeedEscrowState(escrowed);
    } catch (e: any) {
      seedError.value = e.message || String(e);
    }
  });

  const handleAdopt = $(async () => {
    if (!escrowedHex.value) return;
    seedBusy.value = true;
    seedError.value = null;
    try {
      await adoptEscrowedSeed(escrowedHex.value);
      // On success the app restarts (dev builds exit instead).
    } catch (e: any) {
      seedError.value = e.message || String(e);
      seedBusy.value = false;
      confirmSeed.value = null;
    }
  });

  const handleRekey = $(async () => {
    seedBusy.value = true;
    seedError.value = null;
    try {
      await rekeyDeviceAgent();
      // On success the app restarts (dev builds exit instead).
    } catch (e: any) {
      seedError.value = e.message || String(e);
      seedBusy.value = false;
      confirmSeed.value = null;
    }
  });

  // Fetch Vault profile and update context for header + this page.
  // Also persists to profile-cache.json so the Rust side (e.g. cast_vote on
  // public polls, which needs display_name) can read it immediately after a
  // fresh link without waiting for an app restart.
  const fetchVaultProfile = $(async () => {
    try {
      const resp = await fetch("http://127.0.0.1:27777/status", {
        signal: AbortSignal.timeout(2000),
      });
      if (resp.ok) {
        const vault = await resp.json();
        if (vault.display_name) displayName.value = vault.display_name;
        if (vault.profile_picture) profilePicture.value = vault.profile_picture;
        if (vault.display_name || vault.profile_picture) {
          await saveProfileCache(
            vault.display_name ?? null,
            vault.profile_picture ?? null,
          );
        }
      }
    } catch {
      // Vault not running — profile will load from layout poll
    }
  });

  // Check if Vault still recognises this link. Returns the rich state so
  // callers can react appropriately:
  //   - 'linked'   — Vault is running and confirms the link.
  //   - 'offline'  — Vault is not reachable; trust local state for now.
  //   - 'mismatch' — Vault is running but doesn't know this app's agent.
  //
  // Critically, this no longer auto-revokes the DHT entry on a `mismatch`.
  // The layout's banner gives the user the choice to reconnect or
  // deliberately disconnect — silently revoking the link surprises users
  // who briefly switched Flowsta accounts in Vault.
  const fetchVaultLinkState = $(
    async (pubKey: string): Promise<"linked" | "offline" | "mismatch"> => {
      try {
        const { getFlowstaLinkStatus } = await import("@flowsta/holochain");
        const result = await getFlowstaLinkStatus({
          clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
          localAgentPubKey: pubKey,
        });
        if (result.state === "linked") return "linked";
        if (result.state === "offline") return "offline";
        return "mismatch";
      } catch {
        return "offline";
      }
    },
  );

  useVisibleTask$(async ({ cleanup }) => {
    // Pull autoLink + returnTo from sessionStorage (set by the caller
    // before nav). See ~/lib/signin.ts.
    const intent = readAndClearSignInIntent();
    autoLink.value = intent.autoLink;
    safeReturnTo.value = intent.returnTo;
    try {
      const status = await invoke<{
        agent_pub_key: string | null;
      }>("get_app_status");
      agentKey.value = status.agent_pub_key;

      // Check if already linked on DHT, then ask Vault for the canonical
      // state. We never auto-revoke from here — the layout banner gives the
      // user a clear choice if Vault disagrees with our local state.
      hasLocalLink.value = (await getIdentityLink().catch(() => null)) !== null;
      if (status.agent_pub_key) {
        const linked = await getLinkedAgents(status.agent_pub_key);
        if (linked.length > 0) {
          linkedVaultKey.value = linked[0];
          const vaultState = await fetchVaultLinkState(status.agent_pub_key);
          if (vaultState !== "mismatch" && !displayName.value) {
            await fetchVaultProfile();
          }
        }
      }
      if (hasLocalLink.value) {
        // Signed in: surface the seed-escrow state (restore offer, legacy
        // re-key offer, or the synced checkmark).
        void loadSeedState();
      }
    } catch (e) {
      console.error("Failed to get agent key:", e);
    } finally {
      loading.value = false;
    }

    // Auto-trigger linking when navigated with ?link=true. Keyed off the
    // LOCAL binding, not the DHT: right after a key restore the DHT shows
    // the adopted agent's link while this install's binding is gone, and
    // step 2 of the restore IS this sign-in.
    if (autoLink.value && !hasLocalLink.value && agentKey.value) {
      autoLink.value = false;
      linking.value = true;
      try {
        const { linkFlowstaIdentity } = await import("@flowsta/holochain");
        const result = await linkFlowstaIdentity({
          appName: "ProofPoll",
          clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
          localAgentPubKey: agentKey.value,
        });
        if (!result.success) {
          error.value = "Identity linking was not completed";
        } else {
          await commitIdentityLink(
            result.payload.vaultAgentPubKey,
            result.payload.vaultSignature,
          );
          linkedVaultKey.value = result.payload.vaultAgentPubKey;
          hasLocalLink.value = true;
          linkStateCtx.value = "linked";
          linkedCtx.value = true;
          await fetchVaultProfile();
          void loadSeedState();
          success.value = "Signed in successfully!";
          await focusSelf();
          const target = safeReturnTo.value;
          if (target) {
            setTimeout(() => nav(target), 1000);
          }
        }
      } catch (e: any) {
        const msg = e.message || String(e);
        if (msg.includes("VaultNotFound") || msg.includes("ECONNREFUSED")) {
          // Vault isn't running. Best-effort: try to open it for the user.
          void invoke("launch_vault").catch(() => {});
          error.value =
            "Flowsta Vault isn't running — opening it now. Once it's ready, click Sign in with Flowsta again.";
        } else if (msg.includes("VaultLocked")) {
          error.value = "Flowsta Vault is locked. Please unlock it first.";
        } else if (msg.includes("UserDenied") || msg.includes("denied")) {
          error.value = "You declined the link request in Flowsta Vault.";
        } else {
          error.value = msg;
        }
      } finally {
        linking.value = false;
      }
    } else if (autoLink.value && hasLocalLink.value) {
      // A stale link exists - the auto-link intent used to be dropped here
      // with no message at all, leaving "Connect with current account" a
      // silent no-op. Rebinding is deliberately a two-step: disconnect the
      // old identity first (the backend refuses silent rebinds too).
      autoLink.value = false;
      error.value =
        "This install is already connected to a Flowsta identity. To connect " +
        "a different account, disconnect the current one below first.";
    }

    // No local polling needed — the layout's linkPoll watches link state
    // every 3 seconds and updates `linkStateContext` accordingly. When the
    // user takes action (link / unlink / disconnect via the banner) the
    // layout state flows down to this page reactively.
    cleanup(() => {});
  });

  const linkIdentity = $(async () => {
    if (!agentKey.value) return;
    error.value = null;
    success.value = null;
    linking.value = true;

    try {
      const { linkFlowstaIdentity } = await import("@flowsta/holochain");

      const result = await linkFlowstaIdentity({
        appName: "ProofPoll",
        clientId: import.meta.env.VITE_FLOWSTA_CLIENT_ID,
        localAgentPubKey: agentKey.value,
      });

      if (!result.success) {
        error.value = "Identity linking was not completed";
        return;
      }

      await commitIdentityLink(
        result.payload.vaultAgentPubKey,
        result.payload.vaultSignature,
      );

      linkedVaultKey.value = result.payload.vaultAgentPubKey;
      hasLocalLink.value = true;
      linkStateCtx.value = "linked";
      linkedCtx.value = true;
      await fetchVaultProfile();
      void loadSeedState();
      success.value = "Signed in successfully!";
      await focusSelf();
      const target = safeReturnTo.value;
      if (target) {
        setTimeout(() => nav(target), 1000);
      }
    } catch (e: any) {
      const msg = e.message || String(e);
      if (msg.includes("VaultNotFound") || msg.includes("ECONNREFUSED")) {
        // Vault isn't running. Best-effort: try to open it for the user.
        void invoke("launch_vault").catch(() => {});
        error.value =
          "Flowsta Vault isn't running — opening it now. Once it's ready, click Sign in with Flowsta again.";
      } else if (msg.includes("VaultLocked")) {
        error.value = "Flowsta Vault is locked. Please unlock it first.";
      } else if (msg.includes("UserDenied") || msg.includes("denied")) {
        error.value = "You declined the link request in Flowsta Vault.";
      } else {
        error.value = msg;
      }
    } finally {
      linking.value = false;
    }
  });

  const unlinkIdentity = $(async () => {
    error.value = null;
    success.value = null;
    unlinking.value = true;

    try {
      await revokeIdentityLink();
      // Tell the Vault to drop its side of the link too. From the webview
      // (not Rust) so the request carries this app's Origin header - the
      // Vault attributes and authorizes the revocation by it. Best-effort:
      // the local revocation above is what ends authority.
      try {
        await fetch("http://127.0.0.1:27777/revoke-identity", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            app_name: "ProofPoll",
            app_agent_pub_key: agentKey.value,
          }),
          signal: AbortSignal.timeout(3000),
        });
      } catch {
        // Vault not running - its stale entry is cosmetic
      }
      linkedVaultKey.value = null;
      hasLocalLink.value = false;
      seedReport.value = null;
      linkStateCtx.value = "unlinked";
      linkedCtx.value = false;
      displayName.value = null;
      profilePicture.value = null;
      confirmUnlink.value = false;
      success.value = "Flowsta account disconnected.";
    } catch (e: any) {
      error.value = e.message || String(e);
    } finally {
      unlinking.value = false;
    }
  });

  return (
    <div class="max-w-xl mx-auto">
      <h1 class="text-2xl font-bold mb-6">Identity</h1>

      {loading.value ? (
        <div class="text-gray-400">Loading...</div>
      ) : linkedVaultKey.value && hasLocalLink.value ? (
        /* ── Linked state ── */
        <div class="space-y-6">
          {/* Profile card */}
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-6">
            <div class="flex items-center gap-4 mb-4">
              {sanitizeImageSrc(profilePicture.value) ? (
                <img
                  src={sanitizeImageSrc(profilePicture.value)!}
                  alt="Profile"
                  class="h-14 w-14 rounded-full object-cover border border-gray-600"
                  width={56}
                  height={56}
                />
              ) : (
                <div class="flex h-14 w-14 items-center justify-center rounded-full bg-indigo-600 text-xl font-medium text-white">
                  {displayName.value ? displayName.value.charAt(0).toUpperCase() : "?"}
                </div>
              )}
              <div>
                <p class="text-white font-medium text-lg">
                  {displayName.value || "Flowsta Account"}
                </p>
                <div class="flex items-center gap-1.5 mt-0.5">
                  <span class="h-2 w-2 rounded-full bg-green-500" />
                  <span class="text-sm text-green-400">Signed in with Flowsta</span>
                </div>
              </div>
            </div>

            <p class="text-sm text-gray-400">
              Your identity is verified. Each person gets one vote per poll, even across multiple devices.
            </p>
          </div>

          {/* Status messages */}
          {error.value && (
            <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-2 rounded-lg text-sm">
              {error.value}
            </div>
          )}

          {success.value && (
            <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg text-sm">
              {success.value}
            </div>
          )}

          {/* ── Backups & recovery: the agent-seed escrow story ──
              One panel, one voice at a time: restore conflict outranks the
              legacy re-key offer outranks the synced checkmark. */}
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-6">
            <h2 class="text-sm font-semibold text-white mb-1">
              Backups &amp; recovery
            </h2>
            <p class="text-xs text-gray-500 mb-3">
              Your polls and votes back up to your Flowsta Vault
              automatically. The backup also carries the key you author with,
              so a new machine can continue as you.
            </p>

            {seedError.value && (
              <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-2 rounded-lg text-sm mb-3">
                {seedError.value}
              </div>
            )}

            {/* Restore conflict: bringing this device back takes two steps */}
            {seedReport.value?.state === "conflict" && !seedDismissed.value && (
              <div class="rounded-lg border border-amber-700/60 bg-amber-900/20 p-4">
                <p class="text-sm font-medium text-amber-200">
                  Bringing this device back takes two steps
                </p>
                <p class="mt-1 text-xs text-gray-400">
                  Your Vault backup carries a different authorship key than
                  this device is using - usually because this is a fresh
                  install while your backup kept the key from the previous
                  one. Step 1 restores that key (ProofPoll restarts). Step
                  2, after the restart: sign back in, and your polls and
                  votes reappear as the network syncs.
                </p>
                {confirmSeed.value !== "adopt" ? (
                  <div class="mt-3 flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-end">
                    <button
                      type="button"
                      onClick$={() => (seedDismissed.value = true)}
                      class="text-sm text-gray-500 hover:text-gray-300 px-4 py-2"
                    >
                      Not now
                    </button>
                    <button
                      type="button"
                      onClick$={() => (confirmSeed.value = "adopt")}
                      class="bg-amber-600 hover:bg-amber-500 text-white font-medium px-4 py-2 rounded-full text-sm"
                    >
                      Step 1: Restore my key
                    </button>
                  </div>
                ) : (
                  <div class="mt-3 bg-gray-900/60 border border-amber-900/50 rounded-lg p-3">
                    <p class="text-xs text-gray-300 mb-3">
                      Restoring replaces this install's key and restarts
                      ProofPoll. Anything published from THIS install stays
                      on the network but will no longer count as yours, and
                      this install's drafts and private notes are lost. On a
                      machine you just set up, there is nothing to lose.
                    </p>
                    <div class="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
                      <button
                        type="button"
                        onClick$={() => (confirmSeed.value = null)}
                        disabled={seedBusy.value}
                        class="text-sm text-gray-400 hover:text-gray-200 px-4 py-2"
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        onClick$={handleAdopt}
                        disabled={seedBusy.value}
                        class="bg-amber-600 hover:bg-amber-500 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-full text-sm"
                      >
                        {seedBusy.value
                          ? "Restoring - ProofPoll will restart..."
                          : "Restore key and restart"}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            )}

            {/* Legacy install: one-time re-key offer */}
            {seedReport.value?.state === "legacy" && (
              <div class="rounded-lg border border-gray-700 bg-gray-800/40 p-4">
                <p class="text-sm font-medium text-gray-200">
                  Protect your authorship in backups
                </p>
                <p class="mt-1 text-xs text-gray-400">
                  This install's key was created before backups could carry
                  one, so your exports hold your records but not the means
                  to keep authoring as you. A one-time key upgrade fixes
                  that for everything you publish from now on. Polls and
                  votes you've already published stay on the network under
                  the old key and remain verifiable as yours historically -
                  but they'll no longer show as yours in this app.
                </p>
                {confirmSeed.value !== "rekey" ? (
                  <div class="mt-3 flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-end">
                    <button
                      type="button"
                      onClick$={() => (confirmSeed.value = "rekey")}
                      class="bg-indigo-600 hover:bg-indigo-500 text-white font-medium px-4 py-2 rounded-full text-sm"
                    >
                      Upgrade my key
                    </button>
                  </div>
                ) : (
                  <div class="mt-3 bg-gray-900/60 border border-gray-700 rounded-lg p-3">
                    <p class="text-xs text-gray-300 mb-3">
                      ProofPoll restarts with the new key, then asks you to
                      sign back in. This install's drafts and private notes
                      do not carry over. This is a one-time choice - you can
                      also keep things as they are, and your exports stay
                      records-only.
                    </p>
                    <div class="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
                      <button
                        type="button"
                        onClick$={() => (confirmSeed.value = null)}
                        disabled={seedBusy.value}
                        class="text-sm text-gray-400 hover:text-gray-200 px-4 py-2"
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        onClick$={handleRekey}
                        disabled={seedBusy.value}
                        class="bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-full text-sm"
                      >
                        {seedBusy.value
                          ? "Upgrading - ProofPoll will restart..."
                          : "Upgrade key and restart"}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            )}

            {/* Synced: the quiet good state */}
            {seedReport.value?.state === "synced" &&
              (escrowCheckFailed.value ? (
                <p class="text-xs text-amber-300/90">
                  Couldn't check your Vault backup just now - your authorship
                  key rides along with the next successful backup.
                </p>
              ) : (
                <p class="text-xs text-green-400">
                  ✓ Your authorship key rides your Vault backups - an export
                  can restore it on a new machine.
                </p>
              ))}

            {/* Local recovery file unreadable and nothing escrowed to adopt */}
            {seedReport.value?.state === "local_unreadable" && (
              <p class="text-xs text-red-300">
                This install's key recovery file can't be read, and no backup
                holds a key to restore. New backups carry your records only.
                The file is never overwritten automatically - see
                proofpoll.log for details.
              </p>
            )}

            {/* Couldn't check the backup and the local state alone decides
                nothing (conflict detection needs the backup) */}
            {!seedReport.value && escrowCheckFailed.value && (
              <p class="text-xs text-gray-500">
                Couldn't check your Vault backup just now - open your Vault
                and revisit this page to see backup and recovery options.
              </p>
            )}
          </div>

          {/* Actions */}
          <div class="space-y-3">
            {!confirmUnlink.value ? (
              <button
                type="button"
                onClick$={() => (confirmUnlink.value = true)}
                class="text-sm text-gray-500 hover:text-gray-300"
              >
                Disconnect Flowsta account
              </button>
            ) : (
              <div class="bg-gray-900 border border-red-900/50 rounded-lg p-4">
                <p class="text-sm text-gray-300 mb-3">
                  Disconnect your Flowsta account? You can reconnect at any time.
                </p>
                <div class="flex gap-2">
                  <button
                    type="button"
                    onClick$={unlinkIdentity}
                    disabled={unlinking.value}
                    class="bg-red-700 hover:bg-red-600 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-full text-sm"
                  >
                    {unlinking.value ? "Disconnecting..." : "Disconnect"}
                  </button>
                  <button
                    type="button"
                    onClick$={() => (confirmUnlink.value = false)}
                    class="bg-gray-700 hover:bg-gray-600 text-gray-200 font-medium px-4 py-2 rounded-full text-sm"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}
          </div>

          {/* Technical details (collapsed by default) */}
          <div>
            <button
              type="button"
              onClick$={() => (showDetails.value = !showDetails.value)}
              class="text-xs text-gray-500 hover:text-gray-400 flex items-center gap-1"
            >
              <span class={`transition-transform ${showDetails.value ? "rotate-90" : ""}`}>
                &#9654;
              </span>
              Technical details
            </button>
            {showDetails.value && (
              <div class="mt-2 bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
                <div>
                  <p class="text-xs text-gray-500 mb-1">ProofPoll agent key</p>
                  <p class="font-mono text-xs text-gray-400 break-all">
                    {agentKey.value}
                  </p>
                </div>
                <div>
                  <p class="text-xs text-gray-500 mb-1">Linked Vault key</p>
                  <p class="font-mono text-xs text-gray-400 break-all">
                    {linkedVaultKey.value}
                  </p>
                </div>
              </div>
            )}
          </div>
        </div>
      ) : (
        /* ── Not linked state ── */
        <div class="space-y-6">
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-6 text-center">
            <h2 class="text-lg font-semibold text-white mb-2">
              Connect your Flowsta account
            </h2>
            <p class="text-sm text-gray-400 mb-6 max-w-sm mx-auto">
              Signing in proves you're a real person, so each person gets one
              vote — even if they have multiple devices.
            </p>

            {/* Status messages */}
            {error.value && (
              <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-2 rounded-lg text-sm mb-4 text-left">
                {error.value}
                {error.value.includes("not running") && (
                  <p class="mt-2 text-gray-400">
                    Don't have Flowsta Vault?{" "}
                    <a
                      href="https://flowsta.com/vault"
                      target="_blank"
                      rel="noopener noreferrer"
                      class="text-indigo-400 hover:text-indigo-300 underline"
                    >
                      Download it at flowsta.com
                    </a>
                  </p>
                )}
              </div>
            )}

            {success.value && (
              <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg text-sm mb-4">
                {success.value}
              </div>
            )}

            <button
              type="button"
              onClick$={linkIdentity}
              disabled={linking.value || !agentKey.value}
              class="disabled:opacity-50 inline-block"
            >
              {linking.value ? (
                <span class="inline-flex items-center bg-gray-200 text-gray-700 font-medium px-6 py-2 rounded-full text-sm">
                  Connecting...
                </span>
              ) : (
                <img
                  src="/assets/flowsta-signin.svg"
                  alt="Sign in with Flowsta"
                  width={158}
                  height={36}
                  class="hover:opacity-80 transition-opacity"
                />
              )}
            </button>
            {linking.value && (
              <p class="text-sm text-indigo-300 mt-3">
                Check your Flowsta Vault app to approve the connection.
              </p>
            )}

            <p class="text-xs text-gray-600 mt-4">
              Flowsta Vault must be running and unlocked on this computer.{" "}
              <a
                href="https://flowsta.com/vault"
                target="_blank"
                rel="noopener noreferrer"
                class="text-indigo-400 hover:text-indigo-300 underline"
              >
                Get Flowsta Vault
              </a>
            </p>
          </div>

          {/* Technical details (collapsed by default) */}
          <div>
            <button
              type="button"
              onClick$={() => (showDetails.value = !showDetails.value)}
              class="text-xs text-gray-500 hover:text-gray-400 flex items-center gap-1"
            >
              <span class={`transition-transform ${showDetails.value ? "rotate-90" : ""}`}>
                &#9654;
              </span>
              Technical details
            </button>
            {showDetails.value && (
              <div class="mt-2 bg-gray-900 border border-gray-800 rounded-lg p-4">
                <p class="text-xs text-gray-500 mb-1">ProofPoll agent key</p>
                <p class="font-mono text-xs text-gray-400 break-all">
                  {agentKey.value || "Not available"}
                </p>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
});
