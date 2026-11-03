// A new poll and a vote, signed in (02): the Create page makes a poll with
// two options, its page opens with the title, a vote for one option shows in
// the results, and the app's own records hold that one vote.
import { byId, click, COLD_START_MS, copyProblems, fill, goto, harnessOp, harnessStatus, shot, timed, waitAppShown, whichShows } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };
const title = `E2E poll ${Date.now()}`;
let pollHash = "";

describe("a new poll and a vote", () => {
  it("the app opens signed in", async () => {
    await waitAppShown();
    await (await byId("nav-drafts")).waitForDisplayed({ timeout: COLD_START_MS, timeoutMsg: "the app never showed as signed in (no Drafts link)" });
  });

  it("the Create page takes a title, a description and two options", async () => {
    await goto("nav-create", "/create/");
    await fill('[data-testid="create-title"]', title);
    await fill('[data-testid="create-description"]', "Made by the UI tests.");
    await fill('[data-testid="create-option-0"]', "Yes");
    await fill('[data-testid="create-option-1"]', "No");
    // The closing date starts two days out: nothing to pick.
    const closes = await byId("create-closes");
    await browser.waitUntil(async () => !(await closes.getText()).includes("Pick a date"), { timeout: 10_000, timeoutMsg: "the closing date never filled in" });
    await shot("vote-create-form");
    await note("create form");
  });

  it("creating it opens the poll's page with its title", async () => {
    await click("create-submit");
    const end = await timed("poll created", () => whichShows(["poll-title", "poll-error", "create-error"], 300_000, "creating the poll"));
    if (end !== "poll-title") {
      await shot("vote-create-failed");
      throw new Error(`creating the poll failed: "${(await (await byId(end)).getText()).replace(/\s+/g, " ")}"`);
    }
    expect((await (await byId("poll-title")).getText()).trim()).toBe(title);
    pollHash = await browser.execute(() => location.hash.slice(1));
    expect(pollHash.length).toBeGreaterThan(0);
    const results = await byId("poll-results");
    expect(await results.getText()).toContain("No votes yet");
    await shot("vote-poll-page");
    await note("poll page, before the vote");
  });

  it("voting for an option shows the vote in the results", async () => {
    await (await byId("poll-vote-form")).waitForDisplayed({ timeout: 60_000 });
    await click("poll-option-0");
    await click("poll-vote");
    const end = await timed("vote cast", () => whichShows(["poll-voted", "poll-vote-error"], 300_000, "the vote"));
    if (end === "poll-vote-error") {
      await shot("vote-failed");
      throw new Error(`the vote failed: "${(await (await byId("poll-vote-error")).getText()).replace(/\s+/g, " ")}"`);
    }
    const results = await byId("poll-results");
    await browser.waitUntil(async () => (await results.getText()).includes("(1 total)"), { timeout: 30_000, timeoutMsg: "the results never counted the vote" });
    expect((await (await byId("poll-result-count-0")).getText()).trim()).toBe("1 (100%)");
    expect((await (await byId("poll-result-count-1")).getText()).trim()).toBe("0 (0%)");
    expect(await (await byId("poll-vote-form")).isExisting()).toBe(false);
    await shot("vote-voted");
    await note("poll page, voted");
  });

  it("the app's own records hold the one vote, for the chosen option", async () => {
    const votes = await harnessOp({ op: "votes", poll: pollHash, dna_version: "1.4" });
    expect(votes.error).toBeNull();
    expect(Array.isArray(votes.data)).toBe(true);
    expect(votes.data.length).toBe(1);
    expect(votes.data[0].vote.option_index).toBe(0);
    expect(votes.data[0].author).toBe((await harnessStatus()).app?.agent_pub_key);
  });
});

describe("wording", () => {
  it("no screen showed wording the app must never show", async () => {
    if (problems.length) console.log("WORDING PROBLEMS:\n  " + [...new Set(problems)].join("\n  "));
    expect([...new Set(problems)]).toEqual([]);
  });
});
