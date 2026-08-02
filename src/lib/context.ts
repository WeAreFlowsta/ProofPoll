import { createContextId, type Signal } from "@builder.io/qwik";

/**
 * The four states an app can be in vs. Flowsta Vault. This is the canonical
 * pattern third-party Holochain apps should adopt — collapsing it down to a
 * boolean leaves users in the lurch when the Vault is closed or when they
 * change Flowsta accounts.
 *
 * - `linked`   — Vault is running, unlocked under the SAME identity this
 *                install is bound to, and recognises this app's agent.
 *                Full feature access.
 * - `offline`  — Vault isn't reachable (or is locked) but we have a local
 *                link record. READ-ONLY: everything on this device stays
 *                visible, but writes to the shared network wait until the
 *                identity can be confirmed. Publishing under an identity
 *                needs proof of presence; reading your own data does not.
 *                (The Rust write gates enforce this regardless of the UI.)
 * - `mismatch` — Vault is running but is unlocked under a DIFFERENT
 *                identity than the one linked here, or doesn't recognise
 *                this app's agent. Surface a banner asking the user to
 *                unlock the matching Vault or disconnect deliberately.
 *                Don't auto-revoke; the user might have switched accounts
 *                temporarily or restored from a different recovery phrase.
 * - `unlinked` — No DHT entry, no local record. Show the Flowsta sign-in CTA.
 */
export type LinkState = "linked" | "offline" | "mismatch" | "unlinked";

export const linkStateContext = createContextId<Signal<LinkState>>("app.linkState");

/**
 * Boolean derived from `linkStateContext`: true when the user can take
 * actions (write polls, vote, comment). Equivalent to `state === 'linked'`
 * — a CONFIRMED identity, nothing less. Read-only surfaces should key on
 * the rich state, not this.
 */
export const linkedContext = createContextId<Signal<boolean>>("app.linked");

export const displayNameContext = createContextId<Signal<string | null>>("app.displayName");
export const profilePictureContext = createContextId<Signal<string | null>>("app.profilePicture");
