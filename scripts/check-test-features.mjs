// The `harness` and `e2e` cargo features compile test servers into the app
// (a localhost /dev server; an embedded WebDriver). They must never reach a
// release: this check fails the build if either is on by default, is not
// built from optional dependencies, or is passed by any workflow.
import { readFileSync, readdirSync } from "node:fs";
const cargo = readFileSync("src-tauri/Cargo.toml", "utf8");
const features = cargo.split("[features]")[1]?.split(/\n\[/)[0] ?? "";
const defaultLine = features.match(/^default\s*=\s*\[(.*)\]/m)?.[1] ?? "";
const bad = [];
for (const f of ["harness", "e2e"]) {
  if (new RegExp(`\\b${f}\\b`).test(defaultLine)) bad.push(`Cargo.toml: \`${f}\` is in the default features`);
}
if (!/^harness\s*=/m.test(features)) bad.push("Cargo.toml: the `harness` feature is missing (its server would be unconditional)");
for (const dep of ["axum", "tauri-plugin-wdio-webdriver"]) {
  const line = cargo.match(new RegExp(`^${dep}\\s*=[^\\n]*`, "m"))?.[0];
  if (line && !/optional = true/.test(line)) bad.push(`Cargo.toml: ${dep} is not optional`);
}
// A workflow that builds the app (tauri build / tauri-action) must never pass
// a test feature; the test workflow may compile them (cargo check only).
for (const f of readdirSync(".github/workflows")) {
  const y = readFileSync(`.github/workflows/${f}`, "utf8");
  const buildsTheApp = /tauri-apps\/tauri-action|tauri build|tauri:build/.test(y);
  if (buildsTheApp && /--features[^\n]*(harness|e2e)|features:[^\n]*(harness|e2e)/.test(y)) bad.push(`.github/workflows/${f}: builds the app with a test feature`);
}
if (bad.length) {
  console.error("check-test-features:\n  " + bad.join("\n  "));
  process.exit(1);
}
console.log("check-test-features: ok (harness and e2e are optional, off by default, and no workflow that builds the app passes them)");
