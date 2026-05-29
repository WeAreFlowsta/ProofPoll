import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { useNavigate } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { linkedContext, linkStateContext, displayNameContext, profilePictureContext } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import { readAndClearSignInIntent } from "~/lib/signin";
import {
  getLinkedAgents,
  commitIdentityLink,
  revokeIdentityLink,
  saveProfileCache,
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
    } catch (e) {
      console.error("Failed to get agent key:", e);
    } finally {
      loading.value = false;
    }

    // Auto-trigger linking when navigated with ?link=true
    if (autoLink.value && !linkedVaultKey.value && agentKey.value) {
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
          linkStateCtx.value = "linked";
          linkedCtx.value = true;
          await fetchVaultProfile();
          success.value = "Signed in successfully!";
          const target = safeReturnTo.value;
          if (target) {
            setTimeout(() => nav(target), 1000);
          }
        }
      } catch (e: any) {
        const msg = e.message || String(e);
        if (msg.includes("VaultNotFound") || msg.includes("ECONNREFUSED")) {
          error.value = "Flowsta Vault is not running. Please start it first.";
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
      linkStateCtx.value = "linked";
      linkedCtx.value = true;
      await fetchVaultProfile();
      success.value = "Signed in successfully!";
      const target = safeReturnTo.value;
      if (target) {
        setTimeout(() => nav(target), 1000);
      }
    } catch (e: any) {
      const msg = e.message || String(e);
      if (msg.includes("VaultNotFound") || msg.includes("ECONNREFUSED")) {
        error.value = "Flowsta Vault is not running. Please start it first.";
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
      linkedVaultKey.value = null;
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
      ) : linkedVaultKey.value ? (
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
