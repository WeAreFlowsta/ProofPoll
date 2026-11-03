// Drafts, signed in (02): a poll saved as a draft shows on the Drafts page,
// publishing it opens the new poll, the poll list shows it, and the draft
// leaves the Drafts page.
import { byId, click, COLD_START_MS, copyProblems, copyProblemsOutside, fill, goto, harnessOp, rowIndex, shot, timed, waitAppShown, whichShows } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };
const noteHome = async (where: string) => { problems.push(...(await copyProblemsOutside(where, ["home-poll-row"]))); };
const title = `E2E draft ${Date.now()}`;
const DRAFTS_STATES = ["drafts-list", "drafts-empty", "drafts-error"];

describe("a draft poll", () => {
  it("the app opens signed in", async () => {
    await waitAppShown();
    await (await byId("nav-drafts")).waitForDisplayed({ timeout: COLD_START_MS, timeoutMsg: "the app never showed as signed in (no Drafts link)" });
  });

  it("the Create page saves a poll as a draft", async () => {
    await goto("nav-create", "/create/");
    await fill('[data-testid="create-title"]', title);
    await fill('[data-testid="create-description"]', "A draft from the UI tests.");
    await fill('[data-testid="create-option-0"]', "Red");
    await fill('[data-testid="create-option-1"]', "Blue");
    await shot("drafts-create-form");
    await click("create-save-draft");
    // "Draft saved" shows for three seconds only: look often.
    const end = await timed("draft saved", () => whichShows(["create-draft-saved", "create-error"], 300_000, "saving the draft", 250));
    await shot("drafts-saved");
    if (end === "create-error") {
      throw new Error(`saving the draft failed: "${(await (await byId("create-error")).getText()).replace(/\s+/g, " ")}"`);
    }
    await note("create, draft saved");
  });

  it("the Drafts page lists it", async () => {
    await goto("nav-drafts", "/drafts/");
    const state = await timed("drafts page settled", () => whichShows(DRAFTS_STATES, 300_000, "the Drafts page"));
    if (state === "drafts-error") {
      await shot("drafts-error");
      throw new Error(`the Drafts page could not load drafts: "${(await (await byId("drafts-error")).getText()).replace(/\s+/g, " ")}"`);
    }
    expect(state).toBe("drafts-list");
    expect(await rowIndex("drafts-row", title)).toBeGreaterThanOrEqual(0);
    await shot("drafts-list");
    await note("drafts page");
    const drafts = await harnessOp({ op: "drafts" });
    expect(drafts.error).toBeNull();
    expect((drafts.data || []).some((d: { title?: string }) => d.title === title)).toBe(true);
  });

  it("publishing it opens the new poll", async () => {
    const i = await rowIndex("drafts-row", title);
    const row = (await $$('[data-testid="drafts-row"]'))[i];
    const publish = await row.$('[data-testid="drafts-publish"]');
    await publish.waitForEnabled({ timeout: 30_000 });
    await publish.click();
    const end = await timed("draft published", () => whichShows(["poll-title", "poll-error", "drafts-error"], 300_000, "publishing the draft"));
    if (end !== "poll-title") {
      await shot("drafts-publish-failed");
      throw new Error(`publishing the draft failed: "${(await (await byId(end)).getText()).replace(/\s+/g, " ")}"`);
    }
    expect((await (await byId("poll-title")).getText()).trim()).toBe(title);
    await shot("drafts-published-poll");
    await note("published poll");
  });

  it("the poll list shows the new poll", async () => {
    await goto("nav-polls", "/");
    // The list loads on opening and refreshes every 30 s.
    await timed("published poll listed", () => browser.waitUntil(async () => (await rowIndex("home-poll-row", title)) >= 0, {
      timeout: 180_000, interval: 2_000, timeoutMsg: "the published poll never showed in the poll list",
    }));
    await shot("drafts-poll-list");
    await noteHome("home with the published poll");
  });

  it("the draft is no longer on the Drafts page", async () => {
    await goto("nav-drafts", "/drafts/");
    const state = await whichShows(DRAFTS_STATES, 300_000, "the Drafts page");
    expect(state).not.toBe("drafts-error");
    expect(await rowIndex("drafts-row", title)).toBe(-1);
    await shot("drafts-after-publish");
    await note("drafts page, after publishing");
  });
});

describe("wording", () => {
  it("no screen showed wording the app must never show", async () => {
    if (problems.length) console.log("WORDING PROBLEMS:\n  " + [...new Set(problems)].join("\n  "));
    expect([...new Set(problems)]).toEqual([]);
  });
});
