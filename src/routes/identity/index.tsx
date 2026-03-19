import { component$, useContext, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { useLocation, useNavigate } from "@builder.io/qwik-city";
import { invoke } from "@tauri-apps/api/core";
import { linkedContext, displayNameContext, profilePictureContext } from "~/lib/context";
import { sanitizeImageSrc } from "~/lib/sanitize";
import {
  getLinkedAgents,
  commitIdentityLink,
  revokeIdentityLink,
} from "~/lib/holochain";

export default component$(() => {
  const loc = useLocation();
  const nav = useNavigate();
  const linkedCtx = useContext(linkedContext);
  const displayName = useContext(displayNameContext);
  const profilePicture = useContext(profilePictureContext);
  const returnTo = loc.url.searchParams.get("returnTo");
  const safeReturnTo = returnTo && returnTo.startsWith("/") ? returnTo : null;
  const agentKey = useSignal<string | null>(null);
  const linkedVaultKey = useSignal<string | null>(null);
  const loading = useSignal(true);
  const linking = useSignal(false);
  const unlinking = useSignal(false);
  const error = useSignal<string | null>(null);
  const success = useSignal<string | null>(null);
  const autoLink = useSignal(loc.url.searchParams.get("link") === "true");
  const showDetails = useSignal(false);
  const confirmUnlink = useSignal(false);

  // Fetch Vault profile and update context for header + this page.
  const fetchVaultProfile = $(async () => {
    try {
      const resp = await fetch("http://127.0.0.1:27777/status", {
        signal: AbortSignal.timeout(2000),
      });
      if (resp.ok) {
        const vault = await resp.json();
        if (vault.display_name) displayName.value = vault.display_name;
        if (vault.profile_picture) profilePicture.value = vault.profile_picture;
      }
    } catch {
      // Vault not running — profile will load from layout poll
    }
  });

  // Check if Vault revoked the link. Returns true if revoked.
  const checkVaultRevoke = $(async (pubKey: string): Promise<boolean> => {
    try {
      const { checkFlowstaLinkStatus } = await import("@flowsta/holochain");
      const vaultStatus = await checkFlowstaLinkStatus({
        localAgentPubKey: pubKey,
      });

      if (!vaultStatus.linked) {
        // Verify Vault is actually running (SDK returns linked:false when unreachable)
        const statusResp = await fetch("http://127.0.0.1:27777/status", {
          signal: AbortSignal.timeout(2000),
        }).catch(() => null);

        if (statusResp && statusResp.ok) {
          // Vault IS running and says not linked — revoke on DHT
          try {
            await revokeIdentityLink();
          } catch (revokeErr: any) {
            console.error("DHT revoke failed:", revokeErr);
          }
          linkedVaultKey.value = null;
          linkedCtx.value = false;
          displayName.value = null;
          profilePicture.value = null;
          success.value = "Your Flowsta account was disconnected from Vault.";
          return true;
        }
      }
    } catch {
      // SDK import or fetch failed — ignore
    }
    return false;
  });

  useVisibleTask$(async ({ cleanup }) => {
    try {
      const status = await invoke<{
        agent_pub_key: string | null;
      }>("get_app_status");
      agentKey.value = status.agent_pub_key;

      // Check if already linked on DHT.
      if (status.agent_pub_key) {
        const linked = await getLinkedAgents(status.agent_pub_key);
        if (linked.length > 0) {
          linkedVaultKey.value = linked[0];
          const revoked = await checkVaultRevoke(status.agent_pub_key);
          if (!revoked && !displayName.value) {
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
          linkedCtx.value = true;
          await fetchVaultProfile();
          success.value = "Signed in successfully!";
          if (safeReturnTo) {
            setTimeout(() => nav(safeReturnTo), 1000);
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

    // Poll Vault link status every 5s while linked
    const interval = setInterval(async () => {
      if (!linkedVaultKey.value || !agentKey.value) return;
      await checkVaultRevoke(agentKey.value);
    }, 5000);

    cleanup(() => clearInterval(interval));
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
      linkedCtx.value = true;
      await fetchVaultProfile();
      success.value = "Signed in successfully!";
      if (safeReturnTo) {
        setTimeout(() => nav(safeReturnTo), 1000);
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
