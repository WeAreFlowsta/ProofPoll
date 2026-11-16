// Disconnect, last in the suite (it ends the sign-in the earlier specs
// use): the person asks on the Identity page and confirms; the link is
// revoked on the network and removed here; the Vault is told with the
// link's own token and its answer is waited for; the page says what the
// Vault did. With the test Vault running and holding this install's link,
// that is "Your Vault no longer lists ProofPoll".
import { byId, click, COLD_START_MS, copyProblems, goto, harnessStatus, shot, timed, waitAppShown, whichShows } from "../_helpers";

const problems: string[] = [];

describe("disconnecting the Flowsta account", () => {
  it("is offered on the Identity page of a signed-in app", async () => {
    await waitAppShown();
    await goto("nav-settings", "/identity/");
    const state = await timed("identity page settled", () => whichShows(["identity-linked", "identity-signed-out"], COLD_START_MS, "the Identity page"));
    expect(state).toBe("identity-linked");
    expect((await harnessStatus()).identity_link).toBeTruthy();
  });

  it("asks once more, and Cancel leaves the sign-in as it was", async () => {
    await click("identity-disconnect");
    const confirm = await byId("identity-disconnect-confirm");
    await confirm.waitForDisplayed({ timeout: 10_000 });
    await shot("disconnect-confirm");
    problems.push(...(await copyProblems("disconnect, the question")));
    const cancel = await $("button=Cancel");
    await cancel.click();
    await (await byId("identity-disconnect")).waitForDisplayed({ timeout: 10_000 });
    expect((await harnessStatus()).identity_link).toBeTruthy();
  });

  it("disconnects, and says the Vault no longer lists ProofPoll", async () => {
    await click("identity-disconnect");
    await click("identity-disconnect-confirm");
    // The network write, then the Vault's answer (the SDK waits up to 10 s).
    const state = await timed("signed out", () => whichShows(["identity-signed-out", "identity-error"], 180_000, "the Identity page after Disconnect"));
    if (state === "identity-error") console.log(`e2e: disconnect failed with: ${await (await byId("identity-error")).getText()}`);
    expect(state).toBe("identity-signed-out");
    const notice = (await (await byId("identity-success")).getText()).trim();
    console.log(`e2e: after Disconnect the page says: ${notice}`);
    await shot("disconnected");
    problems.push(...(await copyProblems("disconnected")));
    expect(notice).toBe("Flowsta account disconnected. Your Vault no longer lists ProofPoll.");
  });

  it("is signed out here: no link kept, the sign-in button is back", async () => {
    await browser.waitUntil(async () => !(await harnessStatus()).identity_link, { timeout: 30_000, timeoutMsg: "the install still holds its identity link" });
    expect(await (await byId("identity-signin")).isDisplayed()).toBe(true);
    expect(await (await byId("identity-linked")).isExisting()).toBe(false);
  });

  it("no screen showed wording the app must never show", () => {
    expect(problems).toEqual([]);
  });
});
