import { component$, useSignal, useVisibleTask$, $ } from "@builder.io/qwik";
import { invoke } from "@tauri-apps/api/core";
import {
  getLinkedAgents,
  commitIdentityLink,
  getIdentityLink,
  revokeIdentityLink,
} from "~/lib/holochain";

export default component$(() => {
  const agentKey = useSignal<string | null>(null);
  const linkedVaultKey = useSignal<string | null>(null);
  const loading = useSignal(true);
  const linking = useSignal(false);
  const unlinking = useSignal(false);
  const error = useSignal<string | null>(null);
  const success = useSignal<string | null>(null);

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
          success.value = "Identity link was revoked from Flowsta Vault.";
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
          await checkVaultRevoke(status.agent_pub_key);
        }
      }
    } catch (e) {
      console.error("Failed to get agent key:", e);
    } finally {
      loading.value = false;
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
      success.value = "Identity linked successfully!";
    } catch (e: any) {
      const msg = e.message || String(e);
      if (msg.includes("VaultNotFound") || msg.includes("ECONNREFUSED")) {
        error.value = "Flowsta Vault is not running. Please start it first.";
      } else if (msg.includes("VaultLocked")) {
        error.value = "Flowsta Vault is locked. Please unlock it first.";
      } else if (msg.includes("UserDenied") || msg.includes("denied")) {
        error.value = "You declined the identity link request in Vault.";
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
      success.value = "Identity unlinked successfully.";
    } catch (e: any) {
      error.value = e.message || String(e);
    } finally {
      unlinking.value = false;
    }
  });

  return (
    <div class="max-w-xl mx-auto">
      <h1 class="text-2xl font-bold mb-6">Flowsta Identity</h1>

      {loading.value ? (
        <div class="text-gray-400">Loading...</div>
      ) : (
        <div class="space-y-6">
          {/* Agent key */}
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-5">
            <h2 class="text-sm font-medium text-gray-300 mb-2">
              Your Agent Key
            </h2>
            <p class="font-mono text-xs text-gray-400 break-all">
              {agentKey.value || "Not available"}
            </p>
            <p class="text-xs text-gray-500 mt-2">
              This is your Holochain agent public key for ProofPoll.
            </p>
          </div>

          {/* Identity linking */}
          <div class="bg-gray-900 border border-gray-800 rounded-lg p-5">
            <h2 class="text-sm font-medium text-gray-300 mb-2">
              Flowsta Identity Link
            </h2>

            {linkedVaultKey.value && (
              <div class="mb-4">
                <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg text-sm mb-3">
                  Identity linked
                </div>
                <p class="text-xs text-gray-500 mb-1">Linked Vault key:</p>
                <p class="font-mono text-xs text-gray-400 break-all">
                  {linkedVaultKey.value}
                </p>
              </div>
            )}

            {!linkedVaultKey.value && (
              <p class="text-sm text-gray-400 mb-4">
                Link your Flowsta Vault identity to enable verified voting.
                When linked, your votes are tied to your real identity for
                sybil-resistant results.
              </p>
            )}

            {error.value && (
              <div class="bg-red-900/50 border border-red-700 text-red-300 px-4 py-2 rounded-lg text-sm mb-3">
                {error.value}
              </div>
            )}

            {success.value && (
              <div class="bg-green-900/20 border border-green-800 text-green-300 px-4 py-2 rounded-lg text-sm mb-3">
                {success.value}
              </div>
            )}

            <div class="flex gap-3">
              {!linkedVaultKey.value && (
                <button
                  type="button"
                  onClick$={linkIdentity}
                  disabled={linking.value || !agentKey.value}
                  class="bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-lg text-sm"
                >
                  {linking.value ? "Linking..." : "Link Flowsta Identity"}
                </button>
              )}

              {linkedVaultKey.value && (
                <>
                  <button
                    type="button"
                    onClick$={linkIdentity}
                    disabled={linking.value || !agentKey.value}
                    class="bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-lg text-sm"
                  >
                    {linking.value ? "Linking..." : "Re-link Identity"}
                  </button>
                  <button
                    type="button"
                    onClick$={unlinkIdentity}
                    disabled={unlinking.value}
                    class="bg-red-700 hover:bg-red-600 disabled:opacity-50 text-white font-medium px-4 py-2 rounded-lg text-sm"
                  >
                    {unlinking.value ? "Unlinking..." : "Unlink Identity"}
                  </button>
                </>
              )}
            </div>
            <p class="text-xs text-gray-500 mt-2">
              Requires Flowsta Vault to be running and unlocked.
            </p>
          </div>
        </div>
      )}
    </div>
  );
});
