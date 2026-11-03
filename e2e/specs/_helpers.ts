// Shared helpers for the UI specs (ported from Flowsta Vault's e2e).
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
