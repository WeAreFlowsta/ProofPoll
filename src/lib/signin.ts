// Sign-in flow params (autoLink trigger + post-signin returnTo path) are
// passed through sessionStorage rather than URL query params because the
// Qwik static adapter strips ?foo=bar in our Tauri setup. Hash works for
// the poll route but can't nest (you can't put a hash inside a hash), so
// it's not usable here.
//
// Callers set the intent then nav("/identity/"); the identity page reads
// + clears the intent in its useVisibleTask$.

const AUTO_LINK_KEY = "proofpoll.signin.autoLink";
const RETURN_TO_KEY = "proofpoll.signin.returnTo";

export interface SignInIntent {
  autoLink: boolean;
  returnTo: string | null;
}

export function setSignInIntent(intent: { autoLink?: boolean; returnTo?: string | null }): void {
  if (intent.autoLink) {
    sessionStorage.setItem(AUTO_LINK_KEY, "true");
  } else {
    sessionStorage.removeItem(AUTO_LINK_KEY);
  }
  if (intent.returnTo) {
    sessionStorage.setItem(RETURN_TO_KEY, intent.returnTo);
  } else {
    sessionStorage.removeItem(RETURN_TO_KEY);
  }
}

export function readAndClearSignInIntent(): SignInIntent {
  const autoLink = sessionStorage.getItem(AUTO_LINK_KEY) === "true";
  const returnTo = sessionStorage.getItem(RETURN_TO_KEY);
  sessionStorage.removeItem(AUTO_LINK_KEY);
  sessionStorage.removeItem(RETURN_TO_KEY);
  return {
    autoLink,
    returnTo: returnTo && returnTo.startsWith("/") ? returnTo : null,
  };
}

/**
 * Tell the Flowsta SDK which identity the open profile is signed in as.
 *
 * The SDK keeps one "bound identity" per app in the page's storage, set
 * when a link is made, and refuses every Vault call (backups among them)
 * while the Vault holds another identity. ProofPoll keeps a profile per
 * identity and opens the one the Vault has unlocked, so that one value
 * must follow the profile: without this, the profile that was not the
 * last to sign in had every backup refused as "a different identity".
 * A profile with no link clears the value, so its sign-in starts clean.
 */
export async function bindOpenProfile(): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const link = await invoke<{ vault_agent_pub_key: string } | null>("get_identity_link");
    const { bindVaultIdentity, clearBoundIdentity, getBoundIdentity } = await import("@flowsta/holochain");
    const bound = getBoundIdentity();
    const linked = link?.vault_agent_pub_key ?? null;
    if (linked === bound) return;
    if (linked) {
      bindVaultIdentity(linked);
    } else {
      clearBoundIdentity();
    }
    console.log(`[ProofPoll] the Vault binding follows the open profile (${linked ? "linked" : "not linked"})`);
  } catch (e) {
    // Not in the app (a browser preview), or the app is still starting:
    // the next call binds.
    console.warn("[ProofPoll] could not set the Vault binding from the profile", e);
  }
}

/** What every call to the Vault through the SDK takes first: the binding
 * follows the open profile (`bindOpenProfile`), and a test build pins the
 * Vault it talks to. A release build pins none, and the SDK finds the
 * Vault itself. */
export async function vaultLinkOptions(): Promise<{ ipcUrl?: string }> {
  await bindOpenProfile();
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const url = await invoke<string | null>("test_vault_url");
    return url ? { ipcUrl: url } : {};
  } catch {
    return {};
  }
}
