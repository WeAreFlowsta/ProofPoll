import { createContextId, type Signal } from "@builder.io/qwik";

export const linkedContext = createContextId<Signal<boolean>>("app.linked");
export const displayNameContext = createContextId<Signal<string | null>>("app.displayName");
export const profilePictureContext = createContextId<Signal<string | null>>("app.profilePicture");
