#!/usr/bin/env node
// The bundled hApp files must agree in four places, or a fresh install
// fails with "hApp bundle not found" or restores a kit that is not the one
// shipped:
//   1. src-tauri/tauri.conf.json  bundle.resources   what the installer ships
//   2. src-tauri/resources/       the files themselves
//   3. src-tauri/src/dna.rs       HAPP_FILE_* constants: what the app installs
//   4. src-tauri/src/kits.rs      KitFile names and SHA-256: what the kit cache keeps
// Run from the repository root. Exit 1 on any difference.
import { readFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";

const conf = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
const bundled = (conf.bundle?.resources || []).filter((r) => r.endsWith(".happ")).map((r) => r.replace(/^resources\//, "")).sort();
const dna = readFileSync("src-tauri/src/dna.rs", "utf8");
const constants = [...dna.matchAll(/^(?:pub )?const HAPP_FILE_[A-Z0-9_]+: &str = "([^"]+)";/gm)].map((m) => m[1]);
const kits = readFileSync("src-tauri/src/kits.rs", "utf8");
const kitFiles = [...kits.matchAll(/KitFile \{ name: "([^"]+)", sha256: "([0-9a-f]{64})" \}/g)].map((m) => ({ name: m[1], sha256: m[2] }));

const problems = [];
if (bundled.length === 0) problems.push("tauri.conf.json bundles no .happ file");
for (const name of bundled) {
  const path = `src-tauri/resources/${name}`;
  if (!existsSync(path)) problems.push(`${name}: bundled, but not in src-tauri/resources`);
  if (!constants.includes(name)) problems.push(`${name}: bundled, but no HAPP_FILE_* constant in dna.rs names it`);
  const kit = kitFiles.find((k) => k.name === name);
  if (!kit) problems.push(`${name}: bundled, but in no kit (kits.rs)`);
  else if (existsSync(path)) {
    const actual = createHash("sha256").update(readFileSync(path)).digest("hex");
    if (actual !== kit.sha256) problems.push(`${name}: kits.rs says ${kit.sha256}, the file is ${actual}`);
  }
}
for (const k of kitFiles) if (!bundled.includes(k.name)) problems.push(`${k.name}: in a kit, but not bundled (tauri.conf.json)`);

if (problems.length) {
  console.error("bundle-sync: the bundled hApp files do not agree:\n  " + problems.join("\n  "));
  process.exit(1);
}
console.log(`bundle-sync OK: ${bundled.join(", ")}`);
