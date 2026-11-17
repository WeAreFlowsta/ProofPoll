// The Vault switches to an identity ProofPoll has never been connected
// under. The Vault names its identity only to an app connected under it,
// so its status then reads "unlocked" and names nobody: ProofPoll must
// still say that the Vault is signed in as someone else, and stop saying it
// when the Vault holds the first identity again. (What "Open as this
// identity" does next is a restart: scripts/another-identity.sh.)
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { byId, copyProblems, harnessStatus, shot, waitAppShown } from "../_helpers";

const PROFILE = resolve(import.meta.dirname ?? ".", "../../profile");
const VAULT = `http://127.0.0.1:${readFileSync(resolve(PROFILE, "vault-port"), "utf8").trim()}`;
const PASSWORD = readFileSync(resolve(PROFILE, "vault-password"), "utf8").trim();
const dev = async (path: string, body?: unknown) => {
  const r = await fetch(`${VAULT}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json", origin: "https://ourtest.flowsta.com" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(300_000),
  });
  return { status: r.status, data: (await r.json().catch(() => null)) as any };
};
const asTheApp = async () => (await (await fetch(`${VAULT}/status`, { headers: { origin: "tauri://localhost" } })).json()) as any;

const problems: string[] = [];
let first = "";

describe("the Vault holds an identity ProofPoll was never connected under", () => {
  it("starts signed in, with no banner", async () => {
    await waitAppShown();
    expect((await harnessStatus()).identity_link).toBeTruthy();
    first = ((await dev("/dev/identities")).data?.identities ?? []).find((i: any) => i.active)?.key ?? "";
    expect(first).not.toBe("");
    expect(await (await byId("nav-mismatch-banner")).isExisting()).toBe(false);
  });

  it("says the Vault is signed in as someone else, though the Vault names nobody", async function () {
    this.timeout(600_000);
    expect((await dev("/dev/lock", {})).status).toBe(200);
    const second = await dev("/dev/setup-identity", { api_url: "https://auth-api-staging.flowsta.com", password: PASSWORD, email: `pp-e2e-second-${Date.now()}@example.com`, display_name: "Second Identity" });
    expect(second.status).toBe(200);
    const seen = await asTheApp();
    console.log(`e2e: the Vault to the app's origin: unlocked ${seen.unlocked}, identity ${seen.agent_pub_key ?? seen.active_identity ?? "not named"}`);
    expect(seen.unlocked).toBe(true);
    expect(seen.agent_pub_key ?? seen.active_identity ?? null).toBeNull();
    const banner = await byId("nav-mismatch-banner");
    await banner.waitForDisplayed({ timeout: 120_000, timeoutMsg: "no banner said the Vault holds another identity" });
    const text = (await banner.getText()).replace(/\s+/g, " ");
    console.log(`e2e: the banner says: ${text.slice(0, 200)}`);
    await shot("another-identity-banner");
    problems.push(...(await copyProblems("another identity")));
    expect(text).toContain("Your Vault is signed in as someone else");
    expect(text).toContain("Open as this identity");
    // Nothing of the first identity's was let go.
    expect((await harnessStatus()).identity_link).toBeTruthy();
  });

  it("stops saying so when the Vault holds the first identity again", async function () {
    this.timeout(600_000);
    expect((await dev("/dev/lock", {})).status).toBe(200);
    expect((await dev("/dev/select-identity", { key: first })).status).toBe(200);
    expect((await dev("/dev/unlock-with-password", { password: PASSWORD })).status).toBe(200);
    await browser.waitUntil(async () => !(await (await byId("nav-mismatch-banner")).isExisting()), { timeout: 180_000, interval: 2_000, timeoutMsg: "the banner stayed after the Vault held the first identity again" });
    await browser.waitUntil(async () => !!(await asTheApp()).agent_pub_key, { timeout: 60_000, timeoutMsg: "the Vault did not name the first identity to the app again" });
    expect((await harnessStatus()).identity_link).toBeTruthy();
    await shot("first-identity-again");
  });

  it("no screen showed wording the app must never show", () => {
    expect(problems).toEqual([]);
  });
});
