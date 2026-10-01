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
