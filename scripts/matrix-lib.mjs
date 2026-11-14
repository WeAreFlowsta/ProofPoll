/**
 * What every leg of the matrix uses: the instances' addresses, one /dev/op
 * call, waiting, and the list of checks. Nothing here is about polls.
 */

import { execFile } from 'node:child_process';
export { execFile };

let failures = 0;
const results = [];
/** How the run went, for the runner's last line. */
export const summary = () => ({ checks: results.length, failures });

export const LEG = (process.argv.find((a) => a.startsWith('--leg=')) || '--leg=').split('=')[1];
export const A = process.env.PP_A || 'http://127.0.0.1:28801';
export const B = process.env.PP_B || 'http://127.0.0.1:28802';
export const C = process.env.PP_C || 'http://127.0.0.1:28803';
export const D = process.env.PP_D || 'http://127.0.0.1:28804';
export const VAULT = process.env.VAULT || 'http://127.0.0.1:27778';
export const CLIENT_ID = process.env.CLIENT_ID || '';
export const CARRIER = process.env.PP_CARRIER || new URL('../carrier/target/debug/proofpoll-carrier', import.meta.url).pathname;

export function record(name, ok, detail = '') {
  results.push({ name, ok, detail });
  if (!ok) failures++;
  console.log(`${ok ? '  ✓' : '  ✗ FAIL'} ${name}${detail ? ` — ${String(detail).slice(0, 300)}` : ''}`);
}
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export async function until(fn, secs, every = 3000) {
  const deadline = Date.now() + secs * 1000;
  while (Date.now() < deadline) {
    try { if (await fn()) return true; } catch { /* not yet */ }
    await sleep(every);
  }
  return false;
}

export async function status(base) {
  const r = await fetch(`${base}/dev/status`);
  return r.json();
}
/** One /dev/op call: `{ ok, data }` with the error text when refused. */
export async function op(base, body, secs = 180) {
  const r = await fetch(`${base}/dev/op`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(secs * 1000),
  });
  const data = await r.json().catch(() => null);
  return { ok: r.ok, data, error: r.ok ? null : data?.error || `HTTP ${r.status}` };
}
export async function waitReady(base, secs = 240) {
  return until(async () => (await status(base)).app?.ready === true, secs);
}
export const nowS = () => Math.floor(Date.now() / 1000);

/** Sign in through the Vault test instance. */
export async function signin(base = A, who = 'A') {
  console.log(`\n── Sign-in (${who})`);
  const vs = await fetch(`${VAULT}/status`).then((r) => r.json()).catch(() => null);
  record(`${who}: the Vault test instance is unlocked`, vs?.unlocked === true, JSON.stringify(vs && { unlocked: vs.unlocked, initialized: vs.initialized }));
  // A conductor in its first minute can time a write out (seen 11-13 with
  // four other instances running: `create_external_link` at 60 s). The page
  // then says the request timed out, and the person signs in again.
  let linked = await op(base, { op: 'link', client_id: CLIENT_ID });
  for (let again = 1; again <= 2 && !linked.ok && /timed out/i.test(linked.error || ''); again++) {
    console.log(`    (the sign-in timed out; again in 30 s, try ${again + 1})`);
    await sleep(30000);
    linked = await op(base, { op: 'link', client_id: CLIENT_ID });
  }
  record(`${who}: links to the Vault identity`, linked.ok && !!linked.data?.vault_agent_pub_key, linked.error || linked.data?.vault_agent_pub_key);
  const st = await status(base);
  record(`${who}: the link is kept`, !!st.identity_link?.vault_agent_pub_key && st.identity_link.vault_agent_pub_key === linked.data?.vault_agent_pub_key, JSON.stringify(st.identity_link));
  return linked.data?.vault_agent_pub_key;
}
