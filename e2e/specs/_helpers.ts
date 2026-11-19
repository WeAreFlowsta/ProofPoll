// Shared helpers for the UI specs (ported from Flowsta Vault's e2e).
import { senseProblems, type Shown } from "../../src/lib/sense";
import { resolve } from "node:path";
import { SHOTS } from "../wdio.conf";

let step = 0;
/** A numbered picture of the window as it is now. */
export const shot = (name: string) => browser.saveScreenshot(resolve(SHOTS, `${String(++step).padStart(2, "0")}-${name}.png`));

export const byId = (id: string) => $(`[data-testid="${id}"]`);

/** Type into an input the way a person's typing reaches the page: set the
 *  value and send the input event the front end listens for. */
export async function fill(selector: string, text: string) {
  const el = await $(selector);
  await el.waitForDisplayed({ timeout: 30_000 });
  await browser.execute((sel: string, t: string) => {
    const input = document.querySelector(sel) as HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set;
    input.focus();
    setter ? setter.call(input, t) : (input.value = t);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }, selector, text);
}

export async function click(id: string, timeout = 30_000) {
  const el = await byId(id);
  await el.waitForDisplayed({ timeout });
  await el.waitForEnabled({ timeout });
  await el.click();
}

/** Everything a person can read in the window right now. */
export const visibleText = () => browser.execute(() => document.body.innerText);

/** Wording the app must never show (the list grows with what testing finds). */
const BANNED: [RegExp, string][] = [
  [/—/, "has an em dash"],
  [/\b[a-z]+(?:_[a-z]+){1,}\b(?: \[\d{3}\])?/, "shows a raw code"],
  [/\[\d{3}\]/, "shows an HTTP status"],
  [/undefined|NaN|\[object Object\]/, "shows a programming leftover"],
];

export function copyProblemsIn(text: string, where: string): string[] {
  return BANNED.filter(([re]) => re.test(text)).map(([re, why]) => `${where} ${why}: "${text.match(re)?.[0]}"`);
}

export async function copyProblems(where: string): Promise<string[]> {
  return copyProblemsIn(await visibleText(), where);
}

/** A first start installs both lines and restarts each conductor once; a
 *  fresh conductor's first network reads can take minutes. */
export const COLD_START_MS = 10 * 60_000;
export async function timed<T>(what: string, run: () => Promise<T>): Promise<T> {
  const t0 = Date.now();
  const out = await run();
  console.log(`[timed] ${what}: ${Math.round((Date.now() - t0) / 1000)}s`);
  return out;
}

/** The test instance's harness (launch-app.sh's PROOFPOLL_HARNESS_PORT):
 *  status and the same operations the matrix uses, for set-up and checks. */
export const HARNESS = `http://127.0.0.1:${process.env.PROOFPOLL_E2E_HARNESS_PORT || 28811}`;
export async function harnessStatus(): Promise<any> {
  const r = await fetch(`${HARNESS}/dev/status`, { signal: AbortSignal.timeout(10_000) });
  return r.json();
}
export async function harnessOp(body: Record<string, unknown>, secs = 180): Promise<{ ok: boolean; data: any; error: string | null }> {
  const r = await fetch(`${HARNESS}/dev/op`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(secs * 1000),
  });
  const data = await r.json().catch(() => null);
  return { ok: r.ok, data, error: r.ok ? null : data?.error || `HTTP ${r.status}` };
}
/** Until the instance's conductors are ready (both lines, after the first
 *  start's restarts). */
export async function waitReady(timeout = COLD_START_MS) {
  await browser.waitUntil(async () => {
    try { return (await harnessStatus()).app?.ready === true; } catch { return false; }
  }, { timeout, interval: 3000, timeoutMsg: "the app never became ready" });
}

/** The display name wdio.conf.ts gives the Vault test instance's identity. */
export const E2E_NAME = "ProofPoll E2E";

/** The first of these test ids on screen, waiting until one is. */
export async function whichShows(ids: string[], timeout: number, what: string, interval = 1000): Promise<string> {
  let seen = "";
  await browser.waitUntil(async () => {
    seen = await browser.execute((list: string[]) => list.find((id) => {
      const el = document.querySelector(`[data-testid="${id}"]`);
      return !!el && el.getClientRects().length > 0;
    }) ?? "", ids);
    return seen !== "";
  }, { timeout, interval, timeoutMsg: `${what}: none of ${ids.join(", ")} showed in ${Math.round(timeout / 1000)} s` });
  return seen;
}

/** A spec's first step (the service starts a fresh app per spec file): the
 *  window shows the app's header, never the Connection Lost screen, and the
 *  harness agrees the app is ready. */
export async function waitAppShown(timeout = COLD_START_MS) {
  const seen = await timed("app shown", () => whichShows(["nav-polls", "nav-connection-lost"], timeout, "the app getting ready", 3000));
  if (seen === "nav-connection-lost") {
    throw new Error(`the app showed Connection Lost: "${(await (await byId("nav-connection-lost")).getText()).replace(/\s+/g, " ")}"`);
  }
  await waitReady(60_000);
}

/** Open a page from the header and wait until the window is on it. */
export async function goto(navId: string, path: string) {
  await click(navId);
  await browser.waitUntil(async () => (await browser.execute(() => location.pathname)) === path, { timeout: 30_000, timeoutMsg: `the window never reached ${path}` });
}

/** The position of the first row with this test id whose text contains `text`, or -1. */
export const rowIndex = (id: string, text: string) =>
  browser.execute((i: string, t: string) => [...document.querySelectorAll(`[data-testid="${i}"]`)].findIndex((r) => (r as HTMLElement).innerText.includes(t)), id, text);

/** copyProblems for a screen that also shows other people's words (poll
 *  titles and descriptions from the network): those elements are left out. */
export async function copyProblemsOutside(where: string, userContentIds: string[]): Promise<string[]> {
  const text = await browser.execute((ids: string[]) => {
    let t = document.body.innerText;
    for (const id of ids) document.querySelectorAll(`[data-testid="${id}"]`).forEach((el) => { t = t.replace((el as HTMLElement).innerText, ""); });
    return t;
  }, userContentIds);
  return copyProblemsIn(text, where);
}


// ── Does what is on the screen make sense? ──────────────────────────────
// The rulebook is src/lib/sense.ts. Here it reads the real window: the
// header, the banners and each page read the same facts through separate
// reads and timers, so they can tell two stories at once.

/** What the window shows right now, as far as the rulebook reads it. */
export async function shownNow(): Promise<Shown> {
  return (await browser.execute(() => {
    const q = (id: string, root: ParentNode = document) => root.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;
    const all = (id: string) => Array.from(document.querySelectorAll(`[data-testid="${id}"]`)) as HTMLElement[];
    const text = (el: Element | null | undefined) => ((el as HTMLElement | null)?.innerText || "").replace(/\s+/g, " ").trim();
    const up = (id: string) => !!q(id);
    const out: Record<string, unknown> = {};
    const name = q("nav-profile-name");
    // The header is there once the app is past its start screens.
    if (up("nav-settings")) {
      out.header = { signIn: up("nav-signin"), name: name ? text(name) : null, nameStruck: !!name && /line-through/.test(name.className), drafts: up("nav-drafts"), restartToUpdate: up("nav-restart-to-update") };
      const migration = q("nav-migration-banner");
      out.banners = { mismatch: up("nav-mismatch-banner"), relink: up("nav-relink-banner"), reconnect: up("nav-reconnect-banner"), confirm: up("nav-link-banner"), migrationPolls: migration ? Number(migration.getAttribute("data-polls")) : null };
    }
    if (up("identity-linked") || up("identity-signed-out")) {
      out.identity = { signedIn: up("identity-linked"), signedOut: up("identity-signed-out"), name: q("identity-display-name") ? text(q("identity-display-name")) : null };
    }
    if (up("home-poll-list") || up("home-loading") || up("home-syncing") || up("home-empty")) {
      out.home = { rows: all("home-poll-row").length, loading: up("home-loading"), syncing: up("home-syncing"), empty: up("home-empty") };
    }
    const results = q("poll-results");
    if (results) {
      const total = /Results \((\d+) total\)/.exec(text(results));
      const counts = Array.from(results.querySelectorAll('[data-testid^="poll-result-count-"]')).map((el) => Number(/^(\d+)/.exec(text(el))?.[1] ?? 0));
      out.poll = { total: total ? Number(total[1]) : null, noVotesYet: /No votes yet/.test(text(results)), counts, voted: up("poll-voted"), voteForm: up("poll-vote-form") };
    }
    if (up("drafts-list") || up("drafts-empty") || up("drafts-signed-out")) {
      out.drafts = { signedOutLine: up("drafts-signed-out"), rows: all("drafts-row").length, empty: up("drafts-empty") };
    }
    const auto = q("updates-auto");
    if (auto) out.updates = { autoOn: auto.getAttribute("aria-checked") === "true" };
    // The app's own wording only (a poll's title or a name is the person's).
    const own = ["nav-starting", "nav-connecting", "nav-connection-lost", "nav-restoring", "nav-restoring-short", "nav-restoring-slow", "nav-restoring-retry-wait", "nav-migration-banner", "nav-mismatch-banner", "nav-relink-banner", "nav-reconnect-banner", "nav-link-banner", "nav-updated", "identity-backups", "poll-counts", "updates-card", "home-syncing", "home-empty"];
    out.texts = [...own.flatMap((id) => all(id)), ...all("home-poll-meta")].map((el) => text(el)).filter(Boolean);
    if (results) (out.texts as string[]).push(...Array.from(results.querySelectorAll("h2, [data-testid^=\"poll-result-count-\"]")).map((el) => text(el)));
    return out;
  })) as Shown;
}

/** What does not make sense and stays that way: a problem counts only when
 *  every look over the next while still shows it (the header re-reads the
 *  connection every 15 s, the home page its polls every 30 s). */
export async function senseProblemsOn(where: string): Promise<string[]> {
  const first = await shownNow();
  const h = first.header;
  console.log(`e2e: sense check ${where}: ${h ? (h.signIn ? "signed out" : `signed in as "${h.name}"${h.nameStruck ? " (struck)" : ""}`) : "a start screen"}${first.identity ? ", Identity page" : ""}${first.home ? `, home with ${first.home.rows} poll(s)` : ""}${first.poll ? `, a poll with ${first.poll.total} vote(s)` : ""}${first.drafts ? `, Drafts with ${first.drafts.rows}` : ""}${first.updates ? ", Updates" : ""}`);
  let standing = senseProblems(first);
  for (let look = 0; look < 8 && standing.length > 0; look++) {
    await browser.pause(5_000);
    const now = senseProblems(await shownNow());
    standing = standing.filter((p) => now.includes(p));
  }
  return standing.map((p) => `${where}: ${p}`);
}

// After every test of every spec the window is checked where the test left
// it; the spec fails at its end if anything did not make sense.
const senseSeen: string[] = [];
if (typeof afterEach === "function" && typeof after === "function") {
  afterEach(async function () {
    try {
      senseSeen.push(...(await senseProblemsOn(`after "${this.currentTest?.title ?? "a test"}"`)));
    } catch {
      /* the window is gone or mid-navigation: nothing to read */
    }
  });
  after(function () {
    // PROOFPOLL_E2E_SENSE_SELFTEST=1 proves the spec fails when something is found.
    if (process.env.PROOFPOLL_E2E_SENSE_SELFTEST) senseSeen.push("self-test: a made-up problem");
    if (senseSeen.length > 0) throw new Error(`The screen showed things that do not make sense:\n  ${[...new Set(senseSeen)].join("\n  ")}`);
  });
}
