// Take the DNA out of the pack the app ships.
//
//   node scripts/extract-shipped-dna.mjs <pack.happ> <resource name> <out file>
//
// The write-rule tests (dna/v1.5/tests) load `workdir/proofpoll_v1_5.dna`,
// which a local build makes and the repository does not hold. The pack that
// ships (src-tauri/resources/proofpoll_v1_5_happ.happ) is committed and
// holds that same file: a pack is gzip over MessagePack
// `{ manifest, resources: { <name>: <bytes> } }`. CI tests the DNA from the
// pack, so what is proven is what people run, never a fresh rebuild (a
// rebuild is another DNA hash, another network).
//
// No dependencies: it reads only as much MessagePack as a pack uses.
import { gunzipSync } from "node:zlib";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname } from "node:path";

const [pack, wanted, out] = process.argv.slice(2);
if (!pack || !wanted || !out) {
  console.error("usage: extract-shipped-dna.mjs <pack.happ> <resource name> <out file>");
  process.exit(2);
}
const buf = gunzipSync(readFileSync(pack));
let at = 0;
const u = (n) => { const v = n === 1 ? buf.readUInt8(at) : n === 2 ? buf.readUInt16BE(at) : buf.readUInt32BE(at); at += n; return v; };
const take = (n) => { const b = buf.subarray(at, at + n); at += n; return b; };

/** Read one value. Maps come back as arrays of [key, value]; byte strings as Buffers. */
function read() {
  const t = u(1);
  if (t <= 0x7f) return t;
  if (t >= 0xe0) return t - 0x100;
  if (t >= 0x80 && t <= 0x8f) return map(t & 0x0f);
  if (t >= 0x90 && t <= 0x9f) return arr(t & 0x0f);
  if (t >= 0xa0 && t <= 0xbf) return take(t & 0x1f).toString("utf8");
  switch (t) {
    case 0xc0: return null;
    case 0xc2: return false;
    case 0xc3: return true;
    case 0xc4: return take(u(1));
    case 0xc5: return take(u(2));
    case 0xc6: return take(u(4));
    case 0xca: at += 4; return 0;
    case 0xcb: at += 8; return 0;
    case 0xcc: return u(1);
    case 0xcd: return u(2);
    case 0xce: return u(4);
    case 0xcf: at += 8; return 0;
    case 0xd0: at += 1; return 0;
    case 0xd1: at += 2; return 0;
    case 0xd2: at += 4; return 0;
    case 0xd3: at += 8; return 0;
    case 0xd9: return take(u(1)).toString("utf8");
    case 0xda: return take(u(2)).toString("utf8");
    case 0xdb: return take(u(4)).toString("utf8");
    case 0xdc: return arr(u(2));
    case 0xdd: return arr(u(4));
    case 0xde: return map(u(2));
    case 0xdf: return map(u(4));
    default: throw new Error(`a MessagePack type this reader does not know: 0x${t.toString(16)} at ${at - 1}`);
  }
}
function arr(n) { const a = []; for (let i = 0; i < n; i++) a.push(read()); return a; }
function map(n) { const m = []; for (let i = 0; i < n; i++) m.push([read(), read()]); return m; }

const top = read();
const resources = (Array.isArray(top) ? top : []).find(([k]) => k === "resources")?.[1];
const bytes = Array.isArray(resources) ? resources.find(([k]) => k === wanted)?.[1] : null;
if (!Buffer.isBuffer(bytes)) {
  const names = Array.isArray(resources) ? resources.map(([k]) => k).join(", ") : "none";
  console.error(`${pack} holds no resource named ${wanted} (it holds: ${names})`);
  process.exit(1);
}
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, bytes);
console.log(`${wanted}: ${bytes.length} bytes, sha256 ${createHash("sha256").update(bytes).digest("hex")} -> ${out}`);
