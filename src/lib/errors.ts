import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import {
  AppAmbiguousError,
  LinkTokenError,
  OriginNotRegisteredError,
  VaultRequestRefusedError,
} from "@flowsta/holochain";

/// Format an error caught from a Tauri `invoke()` call.
///
/// Tauri rejects with the raw string when the Rust command returns
/// `Result<_, String>`, so `e` is typically a string, not an Error
/// object. The old `e.message || "fallback"` pattern always falls
/// through to the fallback in that case, hiding the actual backend
/// error from the user.
export function formatInvokeError(e: unknown, fallback: string): string {
  if (typeof e === "string") return e || fallback;
  if (e instanceof Error) return e.message || fallback;
  if (e && typeof e === "object" && "message" in e) {
    const msg = (e as { message?: unknown }).message;
    if (typeof msg === "string" && msg) return msg;
  }
  return fallback;
}

/**
 * Sent when the Vault refused a call over this profile's link: by the Rust
 * side for its own calls, and by `vaultRefusalMessage` for the SDK's. The
 * layout shows the offer to connect again.
 */
export const LINK_REFUSED_EVENT = "vault-link-refused";

/**
 * A refusal from the Vault about which app is calling, in ProofPoll's
 * words; null for any other error (the caller keeps its own wording).
 *
 * A refused link token also raises the offer to connect again, and the
 * Rust side forgets its copy of a token the Vault no longer knows (the SDK
 * has already forgotten its own).
 */
export function vaultRefusalMessage(e: unknown): string | null {
  if (e instanceof LinkTokenError) {
    if (e.code === "invalid_link_token") {
      void invoke("clear_vault_link_token").catch(() => {});
    }
    void emit(LINK_REFUSED_EVENT).catch(() => {});
    return "Your Flowsta Vault no longer recognizes this ProofPoll's connection. Connect ProofPoll to your Vault again.";
  }
  if (e instanceof AppAmbiguousError) {
    return (
      "Another app on this computer connects to Flowsta Vault the same way, and your Vault " +
      "could not tell which one was asking. Sign in with Flowsta again to connect ProofPoll."
    );
  }
  if (e instanceof OriginNotRegisteredError) {
    return (
      "This ProofPoll is not registered to connect to Flowsta Vault from here. " +
      "Its maker registers where the app connects from at dev.flowsta.com."
    );
  }
  if (e instanceof VaultRequestRefusedError) {
    return "Flowsta Vault refused the request because of how it reached it. Close ProofPoll, open it again, and try again.";
  }
  return null;
}
