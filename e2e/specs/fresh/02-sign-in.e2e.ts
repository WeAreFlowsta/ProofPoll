// Signing in: from the Identity page, the sign-in button links this app to
// the Vault test instance (the test build pins the app's Vault calls to it,
// and it approves on its own). The page then shows the linked identity, the
// header shows its name, and the app's own record of the link is set.
import { byId, click, COLD_START_MS, copyProblems, E2E_NAME, goto, harnessOp, harnessStatus, shot, timed, visibleText, waitAppShown, whichShows } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };

describe("signing in with Flowsta", () => {
  it("the app opens, not signed in yet", async () => {
    await waitAppShown();
    await (await byId("nav-signin")).waitForDisplayed({ timeout: 60_000 });
    expect((await harnessStatus()).identity_link ?? null).toBeNull();
  });

  it("the Identity page's sign-in button connects to the Vault", async () => {
    await goto("nav-settings", "/identity/");
    const state = await timed("identity page settled", () => whichShows(["identity-signed-out", "identity-linked"], COLD_START_MS, "the Identity page"));
    expect(state).toBe("identity-signed-out");
    await shot("signin-identity-signed-out");
    await click("identity-signin", 60_000);
    // While the Vault answers, the page says what is happening.
    if (await (await byId("identity-connecting")).isDisplayed().catch(() => false)) {
      await shot("signin-connecting");
      await note("identity, connecting");
    }
    const end = await timed("sign-in", () => whichShows(["identity-linked", "identity-error"], 300_000, "the sign-in"));
    if (end === "identity-error") {
      await shot("signin-error");
      throw new Error(`the sign-in failed: "${(await (await byId("identity-error")).getText()).replace(/\s+/g, " ")}"`);
    }
    expect(await (await byId("identity-success")).getText()).toContain("Signed in successfully");
  });

  it("the page shows the linked identity's name", async () => {
    const name = await byId("identity-display-name");
    await browser.waitUntil(async () => (await name.getText()).trim() === E2E_NAME, {
      timeout: 60_000, timeoutMsg: `the Identity page never showed "${E2E_NAME}"`,
    });
    expect(await visibleText()).toContain("Signed in with Flowsta");
    await shot("signin-identity-linked");
    await note("identity, signed in");
  });

  it("the header shows the name, and the Drafts link appears", async () => {
    const name = await byId("nav-profile-name");
    await browser.waitUntil(async () => (await name.isDisplayed().catch(() => false)) && (await name.getText()).trim() === E2E_NAME, {
      timeout: 60_000, timeoutMsg: `the header never showed "${E2E_NAME}"`,
    });
    expect(await (await byId("nav-signin")).isExisting()).toBe(false);
    expect(await (await byId("nav-drafts")).isDisplayed()).toBe(true);
    await shot("signin-header");
  });

  it("the app's own record of the link is set", async () => {
    const link = (await harnessStatus()).identity_link;
    expect(link).toBeTruthy();
    expect(typeof link.vault_agent_pub_key).toBe("string");
    expect(link.vault_agent_pub_key.length).toBeGreaterThan(0);
    const identity = await harnessOp({ op: "identity" });
    expect(identity.error).toBeNull();
    expect(identity.data?.vault_agent_pub_key).toBe(link.vault_agent_pub_key);
  });
});

describe("wording", () => {
  it("no screen showed wording the app must never show", async () => {
    if (problems.length) console.log("WORDING PROBLEMS:\n  " + [...new Set(problems)].join("\n  "));
    expect([...new Set(problems)]).toEqual([]);
  });
});
