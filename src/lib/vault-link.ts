// This install's link in the person's Flowsta Vault: making it (sign-in)
// and removing it (disconnect).
//
// When the Vault links an app it gives the link a private token. The token
// is what tells this ProofPoll from any other app that connects the same
// way (every Tauri app's webview presents the same origin), and what proves
// a disconnect comes from the app that made the link. The SDK presents it
// on every call. ProofPoll keeps it with the profile, on the Rust side
// (`link_token.rs`), because the Rust side calls the Vault too and each
// profile has its own link; `bindOpenProfile` hands the SDK the open
// profile's token.
//
// The token is a secret: it is never logged and never shown.
//
// Forks: keep sign-in and disconnect on these two functions, so the token
// is stored and cleared in one place.
import { invoke } from "@tauri-apps/api/core";
import { commitIdentityLink } from "./holochain";
import { bindOpenProfile, CLIENT_ID, vaultLinkOptions, whileLinkChanges } from "./signin";

const APP_NAME = "ProofPoll";

/**
 * Sign in with Flowsta: the person approves the link in their Vault, the
 * link is written to the network and saved for this profile, and the
 * profile keeps the link's token.
 *
 * Also the way to connect again when the Vault no longer has the link: the
 * same identity signs the same key, and the profile gets a new token.
 *
 * @returns the identity's key, or null when the Vault did not complete it
 * @throws what the SDK and `commit_identity_link` throw
 */
export async function signInWithVault(localAgentPubKey: string): Promise<string | null> {
  const pin = await vaultLinkOptions();
  const { linkFlowstaIdentity } = await import("@flowsta/holochain");
  return whileLinkChanges(async () => {
    const result = await linkFlowstaIdentity({
      appName: APP_NAME,
      clientId: CLIENT_ID,
      localAgentPubKey,
      ...pin,
    });
    if (!result.success) return null;
    // The commit refuses an identity other than the one this profile is
    // signed in as. Only a link that was committed has its token kept:
    // the profile's earlier token, if any, stays until then.
    await commitIdentityLink(result.payload.vaultAgentPubKey, result.payload.vaultSignature);
    if (result.linkToken) {
      await invoke("save_vault_link_token", { token: result.linkToken });
    } else {
      // A Vault from before link tokens. A new link ends any earlier token.
      await invoke("clear_vault_link_token");
    }
    return result.payload.vaultAgentPubKey;
  });
}

/** What the Vault did when ProofPoll disconnected. */
export type VaultDisconnect =
  /** The Vault answered and removed ProofPoll. */
  | "removed"
  /** The Vault answered and had no connection of this ProofPoll's to remove. */
  | "not_listed"
  /** No Vault answered: it still lists ProofPoll. */
  | "unreachable"
  /** The Vault has another identity open, and was not asked: the identity
   * that was signed in here still lists ProofPoll. */
  | "other_identity";

/**
 * Tell the Vault that this profile disconnected, and wait for what it did
 * (up to 10 seconds). Call it after the link is revoked on the network and
 * deleted here (`revokeIdentityLink`): that is what ends the sign-in. This
 * only settles the Vault's own list, and never throws.
 *
 * Whatever the Vault did, the profile's link token is forgotten on both
 * sides afterwards: this ProofPoll no longer uses the link.
 *
 * @param linkedVaultKey the identity this profile was signed in as
 */
export async function disconnectFromVault(
  localAgentPubKey: string | null,
  linkedVaultKey: string | null,
): Promise<VaultDisconnect> {
  // The SDK presents the profile's token: make sure it holds it.
  await bindOpenProfile();
  return whileLinkChanges(async () => {
    let outcome: VaultDisconnect = "unreachable";
    try {
      // The Vault the Rust side talks to: this account's own, never
      // another account's on the same computer.
      const vault = await invoke<{ url: string; agent_pub_key: string | null } | null>("probe_vault");
      if (vault?.agent_pub_key && linkedVaultKey && vault.agent_pub_key !== linkedVaultKey) {
        // A Vault holds several identities and answers as the one that is
        // open. With another one open, its list is not the one this link
        // is in: leave it alone.
        outcome = "other_identity";
      } else if (vault && localAgentPubKey) {
        const { revokeFlowstaIdentity } = await import("@flowsta/holochain");
        const answer = await revokeFlowstaIdentity({
          appName: APP_NAME,
          clientId: CLIENT_ID,
          localAgentPubKey,
          ipcUrl: vault.url,
        });
        outcome = !answer.reachable ? "unreachable" : answer.removed ? "removed" : "not_listed";
      }
    } catch {
      // Counted as not reached: the notice says the Vault still lists it.
    }
    await forgetLinkToken(localAgentPubKey);
    return outcome;
  });
}

/**
 * The profile has no link any more: forget its token here and on the Rust
 * side. For a disconnect that does not ask the Vault (the layout's, made
 * while the Vault holds another identity).
 */
export async function forgetLinkToken(localAgentPubKey: string | null): Promise<void> {
  try {
    if (localAgentPubKey) {
      const { clearVaultLinkToken } = await import("@flowsta/holochain");
      clearVaultLinkToken(CLIENT_ID, localAgentPubKey);
    }
    await invoke("clear_vault_link_token");
  } catch {
    // Not in the app, or the app is closing.
  }
}

/** What the Identity page says after a disconnect. */
export function disconnectNotice(outcome: VaultDisconnect): string {
  switch (outcome) {
    case "removed":
      return "Flowsta account disconnected. Your Vault no longer lists ProofPoll.";
    case "not_listed":
      return "Flowsta account disconnected. Your Vault had no connection from this ProofPoll to remove.";
    case "unreachable":
      return (
        "Disconnected here. Your Vault was not reachable and still lists ProofPoll. " +
        "You can disconnect it there, under Connected Apps."
      );
    case "other_identity":
      return (
        "Disconnected here. Your Vault has a different identity open, so the one you " +
        "were signed in as still lists ProofPoll. You can disconnect it in your Vault, " +
        "under Connected Apps, after switching to that identity."
      );
  }
}
