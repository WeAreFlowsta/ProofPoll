// The start screen while a key's history is restored from the network, in
// each of its states: restoring, restoring for a long time, a restore that
// came back short of the backup (one record, several), and the wait before
// "Try again" is taken. The states are shown through the harness (living
// through them needs four devices: scripts/matrix.sh, the `short` legs);
// what is checked here is what the person sees: one message at a time, the
// right words for one record and for several, the two buttons, and the
// spinner below the header (it once slid under it).
import { byId, copyProblems, harnessOp, harnessStatus, shot, visibleText, waitAppShown } from "../_helpers";

const problems: string[] = [];
const note = async (where: string) => { problems.push(...(await copyProblems(where))); };
const show = async (status: Record<string, unknown>) => {
  const r = await harnessOp({ op: "show_status", status });
  expect(r.ok).toBe(true);
  await browser.pause(700);
};
/** Where the restoring line and its spinner sit, and where the header ends. */
const layout = () =>
  browser.execute(() => {
    const line = document.querySelector('[data-testid="nav-restoring"]') as HTMLElement | null;
    const spinner = line?.previousElementSibling as HTMLElement | null;
    const header = document.querySelector("header, nav") as HTMLElement | null;
    const r = (e: HTMLElement | null) => (e ? { top: Math.round(e.getBoundingClientRect().top), bottom: Math.round(e.getBoundingClientRect().bottom) } : null);
    return { line: r(line), spinner: r(spinner), header: r(header), height: window.innerHeight, spins: !!spinner?.className.includes("animate-spin") };
  });

describe("the start screen while a history is restored", () => {
  let ready: Record<string, unknown> = {};
  after(async () => {
    if (ready.status) await harnessOp({ op: "show_status", status: ready });
  });

  it("the app is open and ready first", async () => {
    await waitAppShown();
    ready = (await harnessStatus()).app.conductor_status;
    expect(ready.status).toBe("ready");
  });

  it("restoring: one line, one explanation, the spinner below the header", async () => {
    await show({ status: "restoring", waited_secs: 12, missing: 0, retry_in_secs: 0 });
    // An app that is ready no longer asks for its status; one that opens
    // does, each second until it is ready. So the window opens again here,
    // as it does on the start this screen belongs to.
    await browser.refresh();
    const line = await byId("nav-restoring");
    await line.waitForDisplayed({ timeout: 20_000 });
    expect(await line.getText()).toContain("Restoring your polls and votes from the network");
    expect(await visibleText()).toContain("opens when it has all of it");
    expect(await (await byId("nav-restoring-short")).isExisting()).toBe(false);
    expect(await (await byId("nav-restoring-slow")).isExisting()).toBe(false);
    const at = await layout();
    console.log(`e2e: restoring layout ${JSON.stringify(at)}`);
    expect(at.spins).toBe(true);
    expect(at.spinner!.top).toBeGreaterThanOrEqual(at.header ? at.header.bottom : 0);
    expect(at.line!.bottom).toBeLessThanOrEqual(at.height);
    await shot("restoring");
    await note("restoring");
  });

  it("after two minutes it says how long, and nothing else changes", async () => {
    await show({ status: "restoring", waited_secs: 135, missing: 0, retry_in_secs: 0 });
    const slow = await byId("nav-restoring-slow");
    await slow.waitForDisplayed({ timeout: 20_000 });
    expect(await slow.getText()).toContain("Still restoring after 2 minutes.");
    expect(await (await byId("nav-restoring-short")).isExisting()).toBe(false);
    await note("restoring, slow");
  });

  it("one record short: says so in the singular, offers both ways on, and Try again waits", async () => {
    await show({ status: "restoring", waited_secs: 200, missing: 1, retry_in_secs: 240 });
    const short = await byId("nav-restoring-short");
    await short.waitForDisplayed({ timeout: 20_000 });
    const text = (await short.getText()).split(/\s+/).join(" ");
    expect(text).toContain("The last record your other device wrote has not reached the network yet.");
    expect(text).toContain("you can continue without it.");
    expect((await (await byId("nav-restoring-go-on")).getText()).trim()).toBe("Continue without it");
    const again = await byId("nav-restoring-try-again");
    expect((await again.getText()).trim()).toBe("Try again");
    expect(await again.isEnabled()).toBe(false);
    expect(await (await byId("nav-restoring-retry-wait")).getText()).toContain("You can try again in 4 min.");
    // One message at a time: the plain restoring explanation and the
    // "still restoring" line give way to this one.
    expect(await visibleText()).not.toContain("opens when it has all of it");
    expect(await (await byId("nav-restoring-slow")).isExisting()).toBe(false);
    const at = await layout();
    expect(at.spinner!.top).toBeGreaterThanOrEqual(at.header ? at.header.bottom : 0);
    await shot("restoring-short-one");
    await note("restoring, one record short");
  });

  it("several records short, the wait over: the plural, and Try again can be pressed", async () => {
    await show({ status: "restoring", waited_secs: 520, missing: 3, retry_in_secs: 0 });
    const short = await byId("nav-restoring-short");
    await browser.waitUntil(async () => (await short.getText()).includes("The last 3 records"), { timeout: 20_000, timeoutMsg: "the screen never said three records" });
    const text = (await short.getText()).split(/\s+/).join(" ");
    expect(text).toContain("The last 3 records your other device wrote have not reached the network yet.");
    expect(text).toContain("you can continue without them.");
    expect((await (await byId("nav-restoring-go-on")).getText()).trim()).toBe("Continue without them");
    expect(await (await byId("nav-restoring-try-again")).isEnabled()).toBe(true);
    expect(await (await byId("nav-restoring-retry-wait")).isExisting()).toBe(false);
    const buttons = await browser.execute(() => {
      const r = (id: string) => (document.querySelector(`[data-testid="${id}"]`) as HTMLElement).getBoundingClientRect();
      return { goOn: Math.round(r("nav-restoring-go-on").left), again: Math.round(r("nav-restoring-try-again").left), bottom: Math.round(r("nav-restoring-try-again").bottom), height: window.innerHeight };
    });
    // The way on that keeps everything is the main one, on the right; both in view.
    expect(buttons.again).toBeGreaterThan(buttons.goOn);
    expect(buttons.bottom).toBeLessThanOrEqual(buttons.height);
    await shot("restoring-short-several");
    await note("restoring, several records short");
  });

  it("ready again: the app is back", async () => {
    await show(ready);
    await waitAppShown(60_000);
    expect(await (await byId("nav-restoring")).isExisting()).toBe(false);
  });

  it("no screen showed wording the app must never show", () => {
    expect(problems).toEqual([]);
  });
});
