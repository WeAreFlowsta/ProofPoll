// Runs WebdriverIO against the test build, the same way on every platform.
//   node e2e/run.mjs              fresh profile: first launch onwards
// Anything after the mode goes to wdio (e.g. --spec e2e/specs/fresh/01-first-launch.e2e.ts).
import { spawnSync } from "node:child_process";

const [mode = "fresh", ...rest] = process.argv.slice(2);
const env = { ...process.env, PROOFPOLL_E2E_MODE: mode };
const r = spawnSync("npx", ["wdio", "run", "e2e/wdio.conf.ts", ...rest], { stdio: "inherit", env, shell: process.platform === "win32" });
process.exit(r.status ?? 1);
