// The Identity page after a relaunch, signed in (02): it shows the linked
// identity's name, that it is signed in, the backups panel and the keys,
// and the app stays signed in. The link is left in place (no disconnect).
// Then Settings: this copy's version, and the Updates tab's automatic-updates switch.
import { byId, click, COLD_START_MS, copyProblems, E2E_NAME, goto, harnessStatus, shot, timed, visibleText, waitAppShown, whichShows } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };

describe("the Identity page, signed in", () => {
  it("the app opens signed in as the Vault's identity", async () => {
    await waitAppShown();
    const name = await byId("nav-profile-name");
    await timed("header name", () => browser.waitUntil(async () => (await name.isDisplayed().catch(() => false)) && (await name.getText()).trim() === E2E_NAME, {
      timeout: COLD_START_MS, interval: 3_000, timeoutMsg: `the header never showed "${E2E_NAME}"`,
    }));
    expect((await harnessStatus()).identity_link).toBeTruthy();
  });

  it("shows the identity's name, that it is signed in, and the backups panel", async () => {
    await goto("nav-settings", "/identity/");
    const state = await timed("identity page settled", () => whichShows(["identity-linked", "identity-signed-out"], COLD_START_MS, "the Identity page"));
    expect(state).toBe("identity-linked");
    const name = await byId("identity-display-name");
    await browser.waitUntil(async () => (await name.getText()).trim() === E2E_NAME, { timeout: 60_000, timeoutMsg: `the Identity page never showed "${E2E_NAME}"` });
    expect(await (await byId("identity-linked")).getText()).toContain("Signed in with Flowsta");
    expect(await (await byId("identity-backups")).isDisplayed()).toBe(true);
    // Disconnecting is offered; this suite leaves the link in place.
    expect(await (await byId("identity-disconnect")).isDisplayed()).toBe(true);
    expect(await (await byId("identity-error")).isExisting()).toBe(false);
    await shot("identity-linked");
    await note("identity, signed in");
  });

  it("stays signed in: no other-identity banner and no sign-in button", async () => {
    await browser.pause(20_000); // one run of the header's 15 s link check
    expect(await (await byId("nav-mismatch-banner")).isExisting()).toBe(false);
    expect(await (await byId("nav-relink-banner")).isExisting()).toBe(false);
    expect(await (await byId("nav-signin")).isExisting()).toBe(false);
    expect(await (await byId("identity-linked")).isDisplayed()).toBe(true);
    expect(await visibleText()).not.toContain("signed in as someone else");
    await shot("identity-still-linked");
  });

  it("its technical details show this app's key and the linked Vault key", async () => {
    // The page's wording was read above, before the keys show: a key is not wording.
    await click("identity-details-toggle");
    const agent = await byId("identity-agent-key");
    await agent.waitForDisplayed({ timeout: 10_000 });
    const status = await harnessStatus();
    expect((await agent.getText()).trim()).toBe(status.app?.agent_pub_key);
    expect((await (await byId("identity-vault-key")).getText()).trim()).toBe(status.identity_link?.vault_agent_pub_key);
    await shot("identity-details");
  });

  // Two identities on one install: the SDK's one stored binding was left
  // at the identity that signed in last, and the other profile's backups
  // were refused as "a different identity" (found on a real machine,
  // 0.6.0-beta.4). The binding follows the open profile.
  it("a binding left by another identity's profile is set back to this profile's", async () => {
    const linked = (await harnessStatus()).identity_link?.vault_agent_pub_key as string;
    const other = "uhCAkWuA-Ehm9EVzcsPE2Wo_vpc9ImljHN8NMMEXpbtG5BdooOV9a";
    expect(linked).not.toBe(other);
    await browser.execute((k) => localStorage.setItem("flowsta:vault-bound-identity", k), other);
    await browser.refresh();
    await waitAppShown();
    await browser.waitUntil(async () => (await browser.execute(() => localStorage.getItem("flowsta:vault-bound-identity"))) === linked, {
      timeout: 60_000,
      timeoutMsg: "the binding stayed at the other identity",
    });
    await goto("nav-settings", "/identity/");
    expect(await (await byId("nav-mismatch-banner")).isExisting()).toBe(false);
  });

  it("Settings shows this version; the Updates tab has updates on by default, and goes back to Identity", async () => {
    const version = await byId("settings-version");
    await version.waitForDisplayed({ timeout: 10_000 });
    expect((await version.getText()).trim()).toMatch(/^ProofPoll \d+\.\d+\.\d+\S*$/);
    await goto("settings-tab-updates", "/settings/updates/");
    await (await byId("updates-auto")).waitForDisplayed({ timeout: 10_000 });
    expect(await (await byId("updates-auto")).getAttribute("aria-checked")).toBe("true");
    expect((await (await byId("settings-version")).getText()).trim()).toMatch(/^ProofPoll \d+\.\d+\.\d+\S*$/);
    await shot("settings-updates");
    await note("settings, updates");
    await goto("settings-tab-identity", "/identity/");
    expect(await whichShows(["identity-linked", "identity-signed-out"], 60_000, "the Identity page")).toBe("identity-linked");
  });
});

describe("wording", () => {
  it("no screen showed wording the app must never show", async () => {
    if (problems.length) console.log("WORDING PROBLEMS:\n  " + [...new Set(problems)].join("\n  "));
    expect([...new Set(problems)]).toEqual([]);
  });
});
