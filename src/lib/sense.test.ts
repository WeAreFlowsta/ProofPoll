// The rulebook over screens written out by hand: each fault it must catch,
// and the ordinary screens it must leave alone.
import { test } from "node:test";
import assert from "node:assert/strict";
import { senseProblems, type Shown } from "./sense";

const header = (over: Partial<NonNullable<Shown["header"]>> = {}) => ({ signIn: false, name: "Ada", nameStruck: false, drafts: true, restartToUpdate: false, ...over });
const signedOutHeader = () => header({ signIn: true, name: null, drafts: false });
const noBanners = { mismatch: false, relink: false, reconnect: false, confirm: false, migrationPolls: null };
const has = (s: Shown, re: RegExp) => assert.ok(senseProblems(s).some((p) => re.test(p)), `expected ${re} in:\n${senseProblems(s).join("\n") || "(nothing found)"}`);

test("ordinary screens make sense", () => {
  const ok: Shown[] = [
    { header: signedOutHeader(), banners: noBanners, identity: { signedIn: false, signedOut: true, name: null }, texts: ["Connect your Flowsta account"] },
    { header: header(), banners: noBanners, identity: { signedIn: true, signedOut: false, name: "Ada" }, texts: ["Last backup saved at 10:42 (117 KB)."] },
    { header: header(), banners: noBanners, home: { rows: 3, loading: false, syncing: false, empty: false }, texts: ["2 options", "1 flag"] },
    { header: signedOutHeader(), banners: noBanners, home: { rows: 0, loading: false, syncing: true, empty: false } },
    { header: header(), banners: noBanners, poll: { total: 1, noVotesYet: false, counts: [1, 0], voted: true, voteForm: false }, texts: ["1 vote", "Results (1 total)"] },
    { header: header(), banners: noBanners, poll: { total: 0, noVotesYet: true, counts: [], voted: false, voteForm: true } },
    { header: header(), banners: noBanners, drafts: { signedOutLine: false, rows: 0, empty: true } },
    { header: header({ restartToUpdate: true }), banners: noBanners, updates: { autoOn: true } },
    // The Vault holds another identity: the name is struck through and the banner says why.
    { header: header({ nameStruck: true }), banners: { ...noBanners, mismatch: true }, home: { rows: 2, loading: false, syncing: false, empty: false } },
    { header: header(), banners: { ...noBanners, migrationPolls: 0 }, home: { rows: 0, loading: false, syncing: true, empty: false } },
  ];
  for (const s of ok) assert.deepEqual(senseProblems(s), [], JSON.stringify(s));
});

test("signed in on one part of the screen and signed out on another", () => {
  has({ header: signedOutHeader(), identity: { signedIn: true, signedOut: false, name: "Ada" } }, /offers to sign in while the Identity page says signed in/);
  has({ header: header(), identity: { signedIn: false, signedOut: true, name: null } }, /as signed in while the Identity page asks to connect/);
  has({ header: header({ signIn: true }) }, /offers to sign in and shows a signed-in name/);
  has({ header: header(), identity: { signedIn: true, signedOut: false, name: "Grace" } }, /names "Ada" and the Identity page names "Grace"/);
  has({ header: header({ nameStruck: true }), banners: { ...noBanners, mismatch: true }, identity: { signedIn: true, signedOut: false, name: "Ada" } }, /signed in as someone else while the Identity page says/);
  has({ header: header(), drafts: { signedOutLine: true, rows: 0, empty: false } }, /links to Drafts while the Drafts page asks to sign in/);
  has({ header: { ...signedOutHeader(), drafts: true } }, /links to Drafts, which need a sign-in/);
});

test("lists, counts and states that disagree", () => {
  has({ home: { rows: 2, loading: false, syncing: false, empty: true } }, /lists 2 polls and says no polls yet/);
  has({ home: { rows: 0, loading: false, syncing: true, empty: true } }, /syncing from the network and no polls yet at once/);
  has({ banners: { ...noBanners, migrationPolls: 4 }, home: { rows: 0, loading: false, syncing: false, empty: true } }, /counts 4 polls brought over while the home page says there are no polls yet/);
  has({ poll: { total: 0, noVotesYet: true, counts: [], voted: true, voteForm: false } }, /you have voted while its results count no votes/);
  has({ poll: { total: 1, noVotesYet: false, counts: [1, 1], voted: false, voteForm: true } }, /add up to 2 votes, more than its total of 1/);
  has({ poll: { total: 3, noVotesYet: false, counts: [0, 0], voted: false, voteForm: true } }, /counts 3 votes and shows none against any option/);
  has({ poll: { total: 1, noVotesYet: false, counts: [1, 0], voted: true, voteForm: true } }, /voted and still shows the form/);
});

test("an update offered that cannot be installed", () => {
  has({ header: header({ restartToUpdate: true }), updates: { autoOn: false } }, /Restart to update" while automatic updates are switched off/);
});

test("sentences no person would write", () => {
  has({ texts: ["1 options"] }, /a plural after 1/);
  has({ texts: ["Last backup saved at 10:42 (0 KB)."] }, /a size shown as 0/);
  has({ texts: ["Still restoring after 0 minutes."] }, /a wait of 0 minutes/);
  has({ texts: ["Recovering from corrupted state — full reset..."] }, /an em dash/);
  has({ texts: ["Closes Invalid Date"] }, /a programming leftover/);
  assert.deepEqual(senseProblems({ texts: ["12 options", "1 option", "21 votes", "(1 KB)", "Still restoring after 3 minutes."] }), []);
});
