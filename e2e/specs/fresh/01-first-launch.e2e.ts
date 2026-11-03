// A person's first launch, signed out: the app starts its node without a
// Connection Lost screen, the home screen settles on the poll list or says
// there are none yet, and the screens a signed-out person can reach read
// right. Creating a poll before signing in asks to sign in first.
import { byId, click, COLD_START_MS, copyProblems, copyProblemsOutside, goto, harnessStatus, shot, timed, waitReady, whichShows } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };
// The poll list shows other people's titles and descriptions: not the app's words.
const noteHome = async (where: string) => { problems.push(...(await copyProblemsOutside(where, ["home-poll-row"]))); };
const HOME_STATES = ["home-poll-list", "home-syncing", "home-empty", "home-filtered-empty", "home-error"];

describe("the first launch", () => {
  it("the window opens and the app gets ready without a Connection Lost screen", async () => {
    const first = await whichShows(["nav-connecting", "nav-starting", "nav-polls", "nav-connection-lost"], 120_000, "the first paint");
    await shot("launch-first-paint");
    await note(`first paint (${first})`);
    const seen = await timed("ready", () => whichShows(["nav-polls", "nav-connection-lost"], COLD_START_MS, "the app getting ready", 3000));
    if (seen === "nav-connection-lost") {
      await shot("launch-connection-lost");
      throw new Error(`the app showed Connection Lost: "${(await (await byId("nav-connection-lost")).getText()).replace(/\s+/g, " ")}"`);
    }
    await waitReady(60_000);
    const status = await harnessStatus();
    expect(status.app?.ready).toBe(true);
    expect(typeof status.app?.agent_pub_key).toBe("string");
    expect(status.identity_link ?? null).toBeNull();
    await shot("launch-ready");
  });

  it("the home screen shows the poll list or says there are none yet", async () => {
    if (await (await byId("home-loading")).isDisplayed().catch(() => false)) {
      await browser.pause(4_000); // the slow-load line appears after 3 s
      await shot("launch-home-loading");
      await note("home while loading");
    }
    const state = await timed("home settled", () => whichShows(HOME_STATES, COLD_START_MS, "the home screen"));
    await shot(`launch-${state}`);
    if (state === "home-error") {
      throw new Error(`the home screen could not load polls: "${(await (await byId("home-error")).getText()).replace(/\s+/g, " ")}"`);
    }
    await noteHome(`home (${state})`);
  });

  it("the header offers sign-in and no Drafts link", async () => {
    await (await byId("nav-signin")).waitForDisplayed({ timeout: 30_000 });
    expect(await (await byId("nav-drafts")).isExisting()).toBe(false);
    expect(await (await byId("nav-profile")).isExisting()).toBe(false);
    expect(await (await byId("nav-mismatch-banner")).isExisting()).toBe(false);
  });

  it("the Identity page offers to sign in with Flowsta", async () => {
    await goto("nav-identity", "/identity/");
    const state = await timed("identity page settled", () => whichShows(["identity-signed-out", "identity-linked"], COLD_START_MS, "the Identity page"));
    expect(state).toBe("identity-signed-out");
    await (await byId("identity-signin")).waitForEnabled({ timeout: 60_000 });
    await shot("launch-identity");
    await note("identity, signed out");
  });
});

describe("creating a poll before signing in", () => {
  it("the home screen's Create Poll asks to sign in first", async () => {
    await goto("nav-polls", "/");
    await click("home-create");
    const dialog = await byId("home-signin-dialog");
    await dialog.waitForDisplayed({ timeout: 10_000 });
    expect(await dialog.getText()).toContain("Sign in required");
    await shot("launch-home-create-asks-sign-in");
    await note("sign-in dialog from the home screen");
    await click("home-signin-dialog-cancel");
    await dialog.waitForExist({ reverse: true, timeout: 10_000 });
  });

  it("the header's Create asks the same and offers the sign-in button", async () => {
    await click("nav-create");
    const dialog = await byId("nav-signin-dialog");
    await dialog.waitForDisplayed({ timeout: 10_000 });
    expect(await dialog.getText()).toContain("Sign in with Flowsta to create and vote on polls.");
    expect(await (await byId("nav-signin-dialog-signin")).isDisplayed()).toBe(true);
    await shot("launch-header-create-asks-sign-in");
    await note("sign-in dialog from the header");
    // Its sign-in button goes to the Identity page and signs in at once;
    // 02 signs in from the Identity page itself, so this one is left.
    await click("nav-signin-dialog-cancel");
    await dialog.waitForExist({ reverse: true, timeout: 10_000 });
    expect(await browser.execute(() => location.pathname)).toBe("/");
  });
});

describe("wording", () => {
  it("no signed-out screen showed wording the app must never show", async () => {
    if (problems.length) console.log("WORDING PROBLEMS:\n  " + [...new Set(problems)].join("\n  "));
    expect([...new Set(problems)]).toEqual([]);
  });
});
