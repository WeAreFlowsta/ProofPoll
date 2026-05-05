import { createContextId, type Signal } from "@builder.io/qwik";

/**
 * The four states an app can be in vs. Flowsta Vault. This is the canonical
 * pattern third-party Holochain apps should adopt — collapsing it down to a
 * boolean leaves users in the lurch when the Vault is closed or when they
 * change Flowsta accounts.
 *
 * - `linked`   — Vault is running and recognises this app's agent.
 *                Full feature access.
 * - `offline`  — Vault isn't running but we have a local link record.
 *                Trust local state, all features stay enabled — the user
 *                is still themselves, the Vault just isn't open right now.
 * - `mismatch` — Vault is running but doesn't recognise this app's agent.
 *                Surface a banner asking the user to reconnect with their
 *                current Flowsta account or disconnect deliberately. Don't
 *                auto-revoke; the user might have switched accounts
 *                temporarily or restored from a different recovery phrase.
 * - `unlinked` — No DHT entry, no local record. Show the Flowsta sign-in CTA.
 */
export type LinkState = "linked" | "offline" | "mismatch" | "unlinked";

export const linkStateContext = createContextId<Signal<LinkState>>("app.linkState");

/**
 * Permissive boolean derived from `linkStateContext`: true when the user
 * can read AND take actions (write polls, vote, comment). Equivalent to
 * `state === 'linked' || state === 'offline'`.
 *
 * Existing pages can keep using this for show/hide decisions. The Vault
 * mismatch state is handled by a top-level banner in the layout, so pages
 * don't each need to re-implement the explanatory UI.
 */
export const linkedContext = createContextId<Signal<boolean>>("app.linked");

export const displayNameContext = createContextId<Signal<string | null>>("app.displayName");
export const profilePictureContext = createContextId<Signal<string | null>>("app.profilePicture");
