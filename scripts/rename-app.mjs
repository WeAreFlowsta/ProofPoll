#!/usr/bin/env node
// Give a fork its own name, identifier, sidecar names and ports, in every
// place that carries them. Run from the repository root:
//
//   node scripts/rename-app.mjs --name "My App" --identifier org.example.myapp \
//        --slug myapp [--port 4500] [--dry-run]
//
//   --name        what people see (window title, dialogs, the Vault's name for the app)
//   --identifier  the bundle identifier, reverse-DNS. It names the data folder
//                 on every system. It must not end in ".app": macOS treats
//                 that ending as an application bundle.
//   --slug        lowercase letters, digits and dashes: the prefix of the four
//                 sidecars, the package name, the log file
//   --port        the 0.6 line's admin port; the 0.7 line takes the next one
//                 (default: keep 4466 and 4467)
//
// What it changes: src-tauri/src/names.rs (the one place the Rust code reads
// its names from), src-tauri/tauri.conf.json, src-tauri/Cargo.toml,
// package.json, and the sidecar names, identifier and binary name in the
// scripts and workflows. What it leaves for you, and lists at the end: the
// DNA and its bundle names (dna/, src-tauri/src/dna.rs), the icons, the
// README, your Flowsta client id and your own bootstrap server.
import { readFileSync, writeFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const args = Object.fromEntries(
  process.argv.slice(2).flatMap((a, i, all) => (a.startsWith("--") ? [[a.slice(2), all[i + 1]?.startsWith("--") || all[i + 1] === undefined ? true : all[i + 1]]] : [])),
);
const fail = (why) => { console.error(`rename-app: ${why}`); process.exit(1); };
const dry = args["dry-run"] === true;
const name = args.name, identifier = args.identifier, slug = args.slug;
if (typeof name !== "string" || !name.trim()) fail('--name "My App" is needed');
if (typeof identifier !== "string" || !/^[a-z0-9]+(\.[a-z0-9-]+){2,}$/i.test(identifier)) fail("--identifier must be reverse-DNS with at least three parts, like org.example.myapp");
if (/\.app$/i.test(identifier)) fail('--identifier must not end in ".app" (macOS reads that ending as an application bundle)');
if (typeof slug !== "string" || !/^[a-z][a-z0-9-]{1,30}$/.test(slug)) fail("--slug must be lowercase letters, digits and dashes, starting with a letter");
const port = args.port === undefined ? null : Number(args.port);
if (port !== null && !(Number.isInteger(port) && port >= 1024 && port <= 65000)) fail("--port must be a port number between 1024 and 65000");

if (!existsSync("src-tauri/src/names.rs")) fail("run it from the repository root (src-tauri/src/names.rs not found)");
const names = readFileSync("src-tauri/src/names.rs", "utf8");
const now = (key) => {
  const m = new RegExp(`pub const ${key}: (?:&str|u16) = "?([^";]+)"?;`).exec(names);
  if (!m) fail(`names.rs has no ${key}`);
  return m[1];
};
const old = {
  name: now("APP_NAME"), identifier: now("APP_IDENTIFIER"), logFile: now("LOG_FILE_NAME"), logFolder: now("LOG_FOLDER_NAME"),
  lair06: now("LAIR_06"), conductor06: now("CONDUCTOR_06"), lair07: now("LAIR_07"), conductor07: now("CONDUCTOR_07"),
  port06: now("ADMIN_PORT_06"), port07: now("ADMIN_PORT_07"), seedTag: now("SEED_TAG"), recovery: now("RECOVERY_FILE"),
};
const oldSlug = old.lair06.replace(/-lair-keystore$/, "");
const next = {
  name: name.trim(), identifier, logFile: slug, logFolder: name.trim(),
  lair06: `${slug}-lair-keystore`, conductor06: `${slug}-holochain-06`, lair07: `${slug}-lair-keystore-07`, conductor07: `${slug}-holochain`,
  port06: String(port ?? old.port06), port07: String(port === null ? old.port07 : port + 1),
  seedTag: `${slug}-device-seed`, recovery: `${slug}-recovery.json`,
};

const changed = [];
function rewrite(path, change) {
  if (!existsSync(path)) return;
  const before = readFileSync(path, "utf8");
  const after = change(before);
  if (after !== before) {
    changed.push(path);
    if (!dry) writeFileSync(path, after);
  }
}
const setConst = (text, key, value, quoted = true) =>
  text.replace(new RegExp(`(pub const ${key}: (?:&str|u16) = )"?[^";]+"?;`), (_, head) => `${head}${quoted ? `"${value}"` : value};`);

// 1. The one place the Rust code reads its names from.
rewrite("src-tauri/src/names.rs", (t) => {
  for (const [key, value] of [
    ["APP_NAME", next.name], ["APP_IDENTIFIER", next.identifier], ["LOG_FILE_NAME", next.logFile], ["LOG_FOLDER_NAME", next.logFolder],
    ["LAIR_06", next.lair06], ["CONDUCTOR_06", next.conductor06], ["LAIR_07", next.lair07], ["CONDUCTOR_07", next.conductor07],
    ["SEED_TAG", next.seedTag], ["RECOVERY_FILE", next.recovery],
  ]) t = setConst(t, key, value);
  t = setConst(t, "ADMIN_PORT_06", next.port06, false);
  return setConst(t, "ADMIN_PORT_07", next.port07, false);
});

// 2. The configs that cannot read names.rs.
rewrite("src-tauri/tauri.conf.json", (t) => {
  const conf = JSON.parse(t);
  conf.productName = next.name;
  conf.identifier = next.identifier;
  for (const w of conf.app?.windows ?? []) if (w.title === old.name) w.title = next.name;
  if (conf.bundle?.externalBin) conf.bundle.externalBin = conf.bundle.externalBin.map((b) => b.replace(`binaries/${oldSlug}-`, `binaries/${slug}-`));
  return JSON.stringify(conf, null, 2) + "\n";
});
rewrite("src-tauri/Cargo.toml", (t) =>
  t.replace(/^name = "[^"]+"/m, `name = "${slug}"`).replace(/^description = "[^"]*"/m, `description = "${next.name}"`).replace(/^authors = \[[^\]]*\]/m, `authors = ["${next.name}"]`));
rewrite("package.json", (t) => t.replace(/^(\s*"name":\s*)"[^"]+"/m, `$1"${slug}"`));

// 3. Scripts and workflows: the sidecar names, the identifier and the
//    program's own file name, as whole words.
const word = (s) => new RegExp(`(?<![A-Za-z0-9_-])${s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![A-Za-z0-9_-])`, "g");
const inScripts = (t) => {
  // Longest first: the 0.6 conductor's name starts with the 0.7 one's.
  for (const [from, to] of [[old.conductor06, next.conductor06], [old.lair07, next.lair07], [old.lair06, next.lair06], [old.conductor07, next.conductor07]]) t = t.replace(word(from), to);
  t = t.replace(word(old.identifier), next.identifier);
  // `pgrep -x proofpoll`, `target/debug/proofpoll`: the binary is the package name.
  return t.replace(new RegExp(`(pgrep -x |target/debug/|target/release/)${oldSlug}(?![A-Za-z0-9_-])`, "g"), `$1${slug}`);
};
const scriptFiles = [];
const walk = (dir, depth = 0) => {
  if (!existsSync(dir)) return;
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    const st = statSync(path);
    if (st.isDirectory()) { if (depth < 2 && !["node_modules", "target", "dist", "gen", "profile", "shots"].includes(entry)) walk(path, depth + 1); }
    else if (/\.(sh|mjs|yml|yaml|cmd|ps1)$/.test(entry) && entry !== "rename-app.mjs") scriptFiles.push(path);
  }
};
for (const dir of ["scripts", "src-tauri/scripts", ".github/workflows", "e2e", "node"]) walk(dir);
for (const top of ["build-all.sh", "download-holochain-binaries.sh"]) if (existsSync(top)) scriptFiles.push(top);
for (const path of scriptFiles) rewrite(path, inScripts);

console.log(`${dry ? "Would change" : "Changed"} ${changed.length} file(s):`);
for (const path of changed) console.log(`  ${path}`);
console.log(`
Still yours to do:
  - The sidecar files in src-tauri/binaries/ are named after the slug: fetch or rename them (${next.conductor07}-<target>, ...).
  - The DNA: dna/, and the bundle names and installed app ids in src-tauri/src/dna.rs and kits.rs.
  - Icons (src-tauri/icons/), the README, the front end's own wording (src/).
  - Your Flowsta client id, and your own bootstrap server and always-on nodes (see the README).
  - If the app was ever released under the old identifier, its data folder is named after it: do not change the identifier of a released app.`);
