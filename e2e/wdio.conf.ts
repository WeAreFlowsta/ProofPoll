// WebdriverIO against the REAL app (see e2e/README.md), ported from Flowsta
// Vault's suite. `npm run e2e:build` builds the app with the `e2e` cargo
// feature (the embedded WebDriver server and the harness, never in a release
// build) against staging; `npm run e2e` starts a Vault test instance with a
// staging identity, wipes the scratch data folder and runs the specs.
// Screenshots land in e2e/shots.
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { SevereServiceError } from "webdriverio";

const here = resolve(import.meta.dirname ?? ".");
export const SHOTS = resolve(here, "shots");
const PROFILE = resolve(here, "profile");
const launcher = resolve(here, "launch-app.sh");
const appBin = resolve(here, "..", "src-tauri", "target", "debug", "proofpoll");
const vaultRepo = resolve(here, "..", "..", "flowsta-vault");
const vaultBin = resolve(vaultRepo, "src-tauri", "target", "debug", "flowsta-vault");
const VAULT_SUFFIX = "pp-e2e-vault";

function ps(): RegExpMatchArray[] {
  const rows = execFileSync("ps", ["-eo", "pid=,ppid=,args="], { encoding: "utf8" }).split("\n");
  return rows.map((r) => r.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/)).filter(Boolean) as RegExpMatchArray[];
}

/** Stop, by pid, every running copy of a test build (never an installed app:
 *  that is a different program). */
function stopBinary(bin: string, what: string) {
  if (process.platform === "win32") return;
  try {
    for (const [, pid, , args] of ps()) {
      if (args === bin || args.startsWith(bin + " ")) {
        try { process.kill(Number(pid), "SIGTERM"); } catch { /* gone */ }
        execFileSync("sleep", ["3"]);
        try { process.kill(Number(pid), 0); process.kill(Number(pid), "SIGKILL"); } catch { /* it left */ }
        console.log(`e2e: stopped a running ${what} ${pid}`);
      }
    }
  } catch (e) {
    console.warn(`e2e: ${what} check skipped:`, (e as Error).message);
  }
}

/** The service ends the app between spec files but not its sidecars: stop
 *  every sidecar of the test profile whose parent is not a live test app. */
function reapSidecars() {
  if (process.platform === "win32") return;
  try {
    const rows = ps();
    const liveApps = new Set(rows.filter((m) => m[3] === appBin || m[3].startsWith(appBin + " ")).map((m) => m[1]));
    for (const [, pid, ppid, args] of rows) {
      if (/proofpoll-(holochain|lair)/.test(args) && args.includes(PROFILE) && !liveApps.has(ppid)) {
        try { process.kill(Number(pid), "SIGTERM"); console.log(`e2e: stopped a leftover sidecar ${pid}`); } catch { /* gone */ }
      }
    }
  } catch (e) {
    console.warn("e2e: sidecar check skipped:", (e as Error).message);
  }
}

/** The window must be ACTIVE on the desktop, or the compositor withholds
 *  frame callbacks and Qwik's visible tasks never run. Linux desktop only. */
function activateWindow() {
  if (process.platform !== "linux" || !process.env.DISPLAY) return;
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      const ids = execFileSync("xdotool", ["search", "--onlyvisible", "--name", "^ProofPoll$"], { encoding: "utf8", timeout: 5_000 }).trim().split("\n").filter(Boolean);
      if (ids.length) {
        execFileSync("xdotool", ["windowactivate", "--sync", ids[ids.length - 1]], { timeout: 10_000, stdio: "ignore" });
        return;
      }
    } catch { /* not mapped yet */ }
    execFileSync("sleep", ["1"]);
  }
  console.warn("e2e: could not activate the app window (no visible 'ProofPoll' window in 60 s)");
}

let idleInhibitor: ChildProcess | null = null;
function screenLocked(): boolean {
  if (process.platform !== "linux") return false;
  try {
    const out = execFileSync("gdbus", ["call", "--session", "--dest", "org.gnome.ScreenSaver", "--object-path", "/org/gnome/ScreenSaver", "--method", "org.gnome.ScreenSaver.GetActive"], { encoding: "utf8", timeout: 5_000 });
    return out.includes("true");
  } catch {
    return false;
  }
}

/** A Vault test instance (the Vault repo beside this one, its test build)
 *  with a staging identity: the Vault the sign-in specs talk to. */
async function startVault(): Promise<number> {
  const dir = `${process.env.HOME}/.local/share/flowsta-vault-${VAULT_SUFFIX}`;
  rmSync(dir, { recursive: true, force: true });
  const child = spawn("dbus-run-session", ["--", "scripts/run-test-instance.sh", VAULT_SUFFIX], { cwd: vaultRepo, detached: true, stdio: "ignore" });
  child.unref();
  for (let i = 0; i < 40; i++) {
    for (const p of [27777, 27778, 27779]) {
      try {
        const r = await fetch(`http://127.0.0.1:${p}/dev/status`, { signal: AbortSignal.timeout(2000) });
        const d = await r.json();
        // Every other Vault test build was stopped first: the one harness
        // answering is ours (an installed Vault has no /dev routes).
        if (d?.harness === true) {
          const created = await fetch(`http://127.0.0.1:${p}/dev/setup-identity`, {
            method: "POST",
            headers: { "content-type": "application/json", origin: "https://ourtest.flowsta.com" },
            body: JSON.stringify({ api_url: "https://auth-api-staging.flowsta.com", password: `E2e-${Date.now()}!`, email: `pp-e2e-${Date.now()}@example.com`, display_name: "ProofPoll E2E" }),
            signal: AbortSignal.timeout(300_000),
          });
          if (!created.ok) throw new SevereServiceError(`e2e: the Vault test instance did not create an identity (${created.status})`);
          return p;
        }
      } catch (e) {
        if (e instanceof SevereServiceError) throw e;
      }
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
  throw new SevereServiceError("e2e: the Vault test instance never answered (build it: flowsta-vault scripts/build-test-binary.sh)");
}

export const config: WebdriverIO.Config = {
  runner: "local",
  specs: [`./specs/${process.env.PROOFPOLL_E2E_MODE || "fresh"}/**/*.e2e.ts`],
  maxInstances: 1,
  // "tauri:options" is the Tauri service's own capability, not in WebdriverIO's types.
  capabilities: [{ browserName: "tauri", "tauri:options": { application: launcher } } as WebdriverIO.Capabilities],
  services: [
    [
      "@wdio/tauri-service",
      { driverProvider: "embedded", appBinaryPath: launcher, captureBackendLogs: true, captureFrontendLogs: true, startTimeout: 120_000 },
    ],
  ],
  framework: "mocha",
  // A first start installs both lines and restarts each conductor once.
  mochaOpts: { ui: "bdd", timeout: 1_500_000, bail: true },
  reporters: ["spec"],
  logLevel: "warn",
  waitforTimeout: 30_000,
  async onPrepare() {
    if (screenLocked()) throw new SevereServiceError("e2e: the screen is locked - the app gets no frames while it is. Unlock it and run again.");
    try {
      idleInhibitor = spawn("gnome-session-inhibit", ["--inhibit", "idle", "--reason", "ProofPoll UI tests", "sleep", "infinity"], { stdio: "ignore" });
      idleInhibitor.on("error", () => { idleInhibitor = null; });
    } catch { /* not GNOME */ }
    stopBinary(appBin, "test app");
    stopBinary(vaultBin, "Vault test instance");
    reapSidecars();
    rmSync(PROFILE, { recursive: true, force: true });
    rmSync(SHOTS, { recursive: true, force: true });
    mkdirSync(SHOTS, { recursive: true });
    mkdirSync(PROFILE, { recursive: true });
    const port = await startVault();
    process.env.PROOFPOLL_E2E_VAULT_PORT = String(port);
    writeFileSync(resolve(PROFILE, "vault-port"), String(port));
    console.log(`e2e: Vault test instance on ${port}`);
  },
  before: async function () { activateWindow(); },
  beforeTest: async function () { activateWindow(); },
  onComplete() {
    idleInhibitor?.kill();
    stopBinary(vaultBin, "Vault test instance");
  },
  afterSession() {
    stopBinary(appBin, "test app");
    reapSidecars();
  },
  afterTest: async function (test, _context, { passed }) {
    const name = `${test.parent} - ${test.title}`.replace(/[^a-z0-9]+/gi, "_").slice(0, 80);
    await browser.saveScreenshot(resolve(SHOTS, `${passed ? "ok" : "FAIL"}_${name}.png`));
  },
};
