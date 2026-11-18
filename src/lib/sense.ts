// What a person would call "that does not make sense": two things on the
// screen that cannot both be true, or a sentence no person would write.
// It reads only what is SHOWN, so the same checks run in a unit test over
// screens written out by hand (sense.test.ts) and over the real window in
// the UI suite (e2e/specs/_helpers.ts). The header, the banners and each
// page read the same facts through separate reads and timers; this is where
// they are held to one story.
//
// Each check came from something a person saw or from two reads of one
// fact found in the code. Add to it the same way.

export interface Shown {
  /** The header. `name`: the profile name shown; `nameStruck`: shown struck through (another identity is in the Vault). */
  header?: { signIn: boolean; name: string | null; nameStruck: boolean; drafts: boolean; restartToUpdate: boolean };
  /** Banners under the header. `migrationPolls`: the count in "N polls so far", or null when that banner is not up. */
  banners?: { mismatch: boolean; relink: boolean; reconnect: boolean; confirm: boolean; migrationPolls: number | null };
  /** The Identity page, when it is the page shown. */
  identity?: { signedIn: boolean; signedOut: boolean; name: string | null };
  /** The home page. */
  home?: { rows: number; loading: boolean; syncing: boolean; empty: boolean };
  /** A poll's page. `total`: the N of "Results (N total)". */
  poll?: { total: number | null; noVotesYet: boolean; counts: number[]; voted: boolean; voteForm: boolean };
  /** The Drafts page. */
  drafts?: { signedOutLine: boolean; rows: number; empty: boolean };
  /** Settings > Updates: the automatic-updates switch. */
  updates?: { autoOn: boolean };
  /** Status text on the screen that is the app's own wording (never what a person typed). */
  texts?: string[];
}

/** Sentences no person would write. */
const ODD_TEXT: [RegExp, string][] = [
  [/\b1 (options|votes|flags|polls|records|minutes|drafts|devices)\b/, "a plural after 1"],
  [/\(0 KB\)/, "a size shown as 0"],
  [/\b0 of 0\b/, '"0 of 0"'],
  [/after 0 minutes/, "a wait of 0 minutes"],
  [/(^|[^\w.])-\d/, "a negative number"],
  [/Invalid Date|\bnull\b|\bundefined\b|\bNaN\b/, "a programming leftover"],
  [/—/, "an em dash"],
];

export function senseProblems(s: Shown): string[] {
  const out: string[] = [];
  const h = s.header;
  const signedInHeader = !!h && h.name !== null && !h.nameStruck;

  if (h) {
    if (h.signIn && h.name !== null) out.push(`the header offers to sign in and shows a signed-in name ("${h.name}")`);
    if (h.signIn && h.drafts) out.push("the header offers to sign in and links to Drafts, which need a sign-in");
    if (h.signIn && s.banners && (s.banners.reconnect || s.banners.confirm)) out.push("the header offers to sign in while a banner speaks of this ProofPoll's connection to the Vault");
    if (h.nameStruck && s.banners && !s.banners.mismatch) out.push("the header strikes through the name while no banner says the Vault holds another identity");
    if (s.identity) {
      if (h.signIn && s.identity.signedIn) out.push("the header offers to sign in while the Identity page says signed in with Flowsta");
      if (signedInHeader && s.identity.signedOut) out.push(`the header shows "${h.name}" as signed in while the Identity page asks to connect a Flowsta account`);
      if (signedInHeader && s.identity.signedIn && s.identity.name && s.identity.name !== "Flowsta Account" && s.identity.name !== h.name) {
        out.push(`the header names "${h.name}" and the Identity page names "${s.identity.name}"`);
      }
    }
    if (s.drafts && h.drafts && s.drafts.signedOutLine) out.push("the header links to Drafts while the Drafts page asks to sign in");
    if (s.updates && h.restartToUpdate && !s.updates.autoOn) out.push('the header offers "Restart to update" while automatic updates are switched off (the waiting download was dropped)');
  }
  if (s.identity && s.identity.signedIn && s.identity.signedOut) out.push("the Identity page shows signed in and signed out at once");
  if (s.banners?.mismatch && s.identity?.signedIn) out.push("a banner says the Vault is signed in as someone else while the Identity page says this identity is signed in and verified");

  if (s.home) {
    const states = [s.home.loading && "loading", s.home.syncing && "syncing from the network", s.home.empty && "no polls yet"].filter(Boolean) as string[];
    if (s.home.rows > 0 && states.length > 0) out.push(`the home page lists ${s.home.rows} poll${s.home.rows === 1 ? "" : "s"} and says ${states.join(" and ")}`);
    if (states.length > 1) out.push(`the home page says ${states.join(" and ")} at once`);
    if (s.home.empty && (s.banners?.migrationPolls ?? 0) > 0) out.push(`a banner counts ${s.banners!.migrationPolls} polls brought over while the home page says there are no polls yet`);
  }

  if (s.poll) {
    const p = s.poll;
    const sum = p.counts.reduce((a, b) => a + b, 0);
    if (p.voted && p.total === 0) out.push("the poll says you have voted while its results count no votes");
    if (p.voted && p.voteForm) out.push("the poll says you have voted and still shows the form to cast a vote");
    if (p.noVotesYet && (p.total ?? 0) > 0) out.push(`the poll says "No votes yet" beside a total of ${p.total}`);
    if (p.total !== null && sum > p.total) out.push(`the poll's options add up to ${sum} votes, more than its total of ${p.total}`);
    if (p.total !== null && p.total > 0 && p.counts.length > 0 && sum === 0) out.push(`the poll counts ${p.total} votes and shows none against any option`);
  }

  if (s.drafts && s.drafts.empty && s.drafts.rows > 0) out.push("the Drafts page lists drafts and says there are none");

  for (const t of s.texts ?? []) {
    for (const [re, what] of ODD_TEXT) {
      if (re.test(t)) out.push(`${what}: "${t.length > 140 ? `${t.slice(0, 140)}...` : t}"`);
    }
  }
  return [...new Set(out)];
}
