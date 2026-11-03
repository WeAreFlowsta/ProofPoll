#!/usr/bin/env node
/**
 * ProofPoll's headless matrix: drives test instances (scripts/build-test-binary.sh,
 * the `harness` feature) through their /dev routes, beside a Flowsta Vault test
 * instance. scripts/matrix.sh starts everything and runs the legs in order.
 *
 * Usage: node scripts/pp-matrix.mjs --leg=<fresh|signin|polls|devices|migration>
 * Env:
 *   PP_A, PP_B     harness URLs of instances A and B (B only for `devices`)
 *   VAULT          the Vault test instance's URL (http://127.0.0.1:27778)
 *   CLIENT_ID      a client id the staging API knows (the staging fixture app)
 */

const LEG = (process.argv.find((a) => a.startsWith('--leg=')) || '--leg=').split('=')[1];
const A = process.env.PP_A || 'http://127.0.0.1:28801';
const B = process.env.PP_B || 'http://127.0.0.1:28802';
const C = process.env.PP_C || 'http://127.0.0.1:28803';
const VAULT = process.env.VAULT || 'http://127.0.0.1:27778';
const CLIENT_ID = process.env.CLIENT_ID || '';

let failures = 0;
const results = [];
function record(name, ok, detail = '') {
  results.push({ name, ok, detail });
  if (!ok) failures++;
  console.log(`${ok ? '  ✓' : '  ✗ FAIL'} ${name}${detail ? ` — ${String(detail).slice(0, 300)}` : ''}`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, secs, every = 3000) {
  const deadline = Date.now() + secs * 1000;
  while (Date.now() < deadline) {
    try { if (await fn()) return true; } catch { /* not yet */ }
    await sleep(every);
  }
  return false;
}

async function status(base) {
  const r = await fetch(`${base}/dev/status`);
  return r.json();
}
/** One /dev/op call: `{ ok, data }` with the error text when refused. */
async function op(base, body, secs = 180) {
  const r = await fetch(`${base}/dev/op`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(secs * 1000),
  });
  const data = await r.json().catch(() => null);
  return { ok: r.ok, data, error: r.ok ? null : data?.error || `HTTP ${r.status}` };
}
async function waitReady(base, secs = 240) {
  return until(async () => (await status(base)).app?.ready === true, secs);
}
const nowS = () => Math.floor(Date.now() / 1000);

// ───────────────────────── legs ─────────────────────────

/** A fresh profile's first start. */
async function fresh() {
  console.log('\n── Fresh install');
  const t0 = Date.now();
  record('A: ready', await waitReady(A), `${Math.round((Date.now() - t0) / 1000)} s`);
  const st = await status(A);
  record('A: both configs carry the network auth', st.configs_with_auth === 2, JSON.stringify(st.configs_with_auth));
  record('A: not started offline', st.started_offline === false);
  const rss = st.rss_mb || {};
  // After the one restart, the 0.7 conductor holds what it compiled since,
  // not the ~2 GB an install kept (holochain#6064).
  record('A: both conductors under 1 GB after their restart', (rss['holochain-0.7'] ?? 9999) < 1024 && (rss['holochain-0.6'] ?? 9999) < 1024, JSON.stringify(rss));
  record('A: no identity link yet', st.identity_link == null, JSON.stringify(st.identity_link));
  // Each line's kit (its conductor, key store and hApp) set aside once a run.
  const kitsCached = await until(async () => { const k = (await status(A)).kits || {}; return !!k['holochain-0.6'] && !!k['holochain-0.7']; }, 120, 5000);
  const kits = (await status(A)).kits || {};
  record("A: both lines' kits cached", kitsCached, JSON.stringify(Object.fromEntries(Object.entries(kits).map(([l, m]) => [l, m?.entries]))));
  const refused = await op(A, { op: 'create_poll', title: 'before sign-in', description: '', options: ['a', 'b'] });
  record('A: a write before sign-in is refused', !refused.ok && /Sign in with Flowsta/.test(refused.error || ''), refused.error);
  // A fresh conductor's first network reads can take minutes (the cold
  // DHT); the UI retries, so the check does, and says how long it took.
  const tRead = Date.now();
  let polls = { ok: false };
  const read = await until(async () => (polls = await op(A, { op: 'polls' })).ok && Array.isArray(polls.data), 300, 5000);
  record('A: reads work before sign-in', read, polls.error || `${polls.data?.length} polls after ${Math.round((Date.now() - tRead) / 1000)} s`);
}

/** Sign in through the Vault test instance. */
async function signin(base = A, who = 'A') {
  console.log(`\n── Sign-in (${who})`);
  const vs = await fetch(`${VAULT}/status`).then((r) => r.json()).catch(() => null);
  record(`${who}: the Vault test instance is unlocked`, vs?.unlocked === true, JSON.stringify(vs && { unlocked: vs.unlocked, initialized: vs.initialized }));
  const linked = await op(base, { op: 'link', client_id: CLIENT_ID });
  record(`${who}: links to the Vault identity`, linked.ok && !!linked.data?.vault_agent_pub_key, linked.error || linked.data?.vault_agent_pub_key);
  const st = await status(base);
  record(`${who}: the link is kept`, !!st.identity_link?.vault_agent_pub_key && st.identity_link.vault_agent_pub_key === linked.data?.vault_agent_pub_key, JSON.stringify(st.identity_link));
  return linked.data?.vault_agent_pub_key;
}

/** Polls, votes, drafts and reasons on v1.4. */
async function polls() {
  console.log('\n── Polls');
  const title = `matrix poll ${nowS()}`;
  const created = await op(A, { op: 'create_poll', title, description: 'from the matrix', options: ['yes', 'no', 'maybe'], closes_at: nowS() + 7 * 86400 });
  record('A: creates a poll', created.ok && typeof created.data === 'string', created.error || created.data);
  const hash = created.data;
  record('A: lists it', await until(async () => ((await op(A, { op: 'polls' })).data || []).some((p) => p.hash === hash || p.poll?.title === title), 60));
  const voted = await op(A, { op: 'vote', poll: hash, option: 1, dna_version: '1.4' });
  record('A: votes', voted.ok, voted.error || voted.data);
  record('A: reads its vote', await until(async () => ((await op(A, { op: 'votes', poll: hash, dna_version: '1.4' })).data || []).length >= 1, 60));
  const reason = await op(A, { op: 'save_rationale', vote: voted.data, text: 'because the matrix said so' });
  record('A: saves a reason for the vote', reason.ok, reason.error);
  const readReason = await op(A, { op: 'rationale', vote: voted.data });
  record('A: reads the reason back', readReason.ok && readReason.data === 'because the matrix said so', readReason.error || JSON.stringify(readReason.data));
  const draft = await op(A, { op: 'save_draft', title: `draft ${nowS()}`, description: 'd', options: ['one', 'two'], closes_at: nowS() + 86400 });
  record('A: saves a draft', draft.ok, draft.error);
  record('A: lists the draft', await until(async () => ((await op(A, { op: 'drafts' })).data || []).some((d) => d.action_hash === draft.data || d.draft_action_hash === draft.data || d.hash === draft.data), 30), JSON.stringify((await op(A, { op: 'drafts' })).data)?.slice(0, 200));
  const pub = await op(A, { op: 'publish_draft', draft: draft.data });
  record('A: publishes the draft as a poll', pub.ok, pub.error || pub.data);
  return { hash, vote: voted.data, title, agent: (await status(A)).app?.agent_pub_key };
}

/** A second device of the same identity sees A's poll and vote as the same person. */
async function devices(state) {
  console.log('\n── Two devices (B joins the same identity)');
  record('B: ready', await waitReady(B));
  await signin(B, 'B');
  const set = await op(B, { op: 'agent_set' });
  const aKey = (await status(A)).app?.agent_pub_key;
  record("B: its agent set holds A's key", await until(async () => ((await op(B, { op: 'agent_set' })).data || []).includes(aKey), 300, 10000), JSON.stringify(set.data));
  if (state?.hash) {
    record("B: sees A's poll", await until(async () => ((await op(B, { op: 'polls' })).data || []).some((p) => p.hash === state.hash || p.poll?.title === state.title), 300, 10000));
    record("B: sees A's vote", await until(async () => ((await op(B, { op: 'votes', poll: state.hash, dna_version: '1.4' })).data || []).length >= 1, 300, 10000));
  }
}

/** ProofPoll 0.4.x's data on v1.3, then the migration as a start runs it. */
async function migration() {
  console.log('\n── Migration with 0.4.x data on v1.3');
  const open = await op(A, { op: 'v13_poll', title: `v13 open ${nowS()}`, description: 'from 0.4.x', options: ['red', 'blue'], closes_at: nowS() + 7 * 86400 });
  record('A: a v1.3 poll (open)', open.ok, open.error || open.data);
  const closing = await op(A, { op: 'v13_poll', title: `v13 closing ${nowS()}`, description: 'closes soon', options: ['x', 'y'], closes_at: nowS() + 45 });
  record('A: a v1.3 poll that closes in 45 s', closing.ok, closing.error);
  const vote13 = await op(A, { op: 'vote', poll: open.data, option: 0, dna_version: '1.3' });
  record('A: a vote on the v1.3 copy', vote13.ok, vote13.error);
  const voteClosing = await op(A, { op: 'vote', poll: closing.data, option: 1, dna_version: '1.3' });
  record('A: a vote on the closing poll', voteClosing.ok, voteClosing.error);
  const draft13 = await op(A, { op: 'v13_draft', title: `v13 draft ${nowS()}`, description: 'an old draft', options: ['p', 'q'], closes_at: nowS() + 86400 });
  record('A: a 0.4.x draft on v1.3', draft13.ok, draft13.error);
  const reason13 = await op(A, { op: 'v13_rationale', vote: vote13.data, text: 'an old reason' });
  record('A: a 0.4.x vote reason on v1.3', reason13.ok, reason13.error);
  console.log('  ... waiting for the closing poll to close');
  await sleep(60_000);
  const t0 = Date.now();
  const mig = await op(A, { op: 'migrate' }, 600);
  record('A: the migration runs to the end', mig.ok && mig.data?.state?.status === 'Complete', mig.error || JSON.stringify(mig.data?.state?.status));
  console.log(`  (took ${Math.round((Date.now() - t0) / 1000)} s)`);
  const st = mig.data?.state || {};
  record('A: the open poll moved to v1.4', (st.polls_migrated || []).length >= 1, JSON.stringify(st.polls_migrated)?.slice(0, 200));
  record('A: no vote parked for an author', (st.votes_pending || []).length === 0, JSON.stringify(st.votes_pending)?.slice(0, 300));
  record('A: the 0.4.x draft and reason carried', (mig.data?.drafts ?? 0) >= 1 && (mig.data?.reasons ?? 0) >= 1, JSON.stringify({ drafts: mig.data?.drafts, reasons: mig.data?.reasons }));
  const listed = (await op(A, { op: 'polls' })).data || [];
  record('A: both polls still list (the closed one in place)', listed.some((p) => p.poll?.title?.startsWith('v13 open')) && listed.some((p) => p.poll?.title?.startsWith('v13 closing')), JSON.stringify(listed.map((p) => [p.poll?.title, p.dna_version])).slice(0, 300));
  const drafts = (await op(A, { op: 'drafts' })).data || [];
  record('A: the old draft lists', drafts.some((d) => JSON.stringify(d).includes('v13 draft')), JSON.stringify(drafts).slice(0, 200));
  const reasonNow = await op(A, { op: 'rationale', vote: vote13.data });
  record('A: the old reason reads', reasonNow.ok && reasonNow.data === 'an old reason', reasonNow.error || JSON.stringify(reasonNow.data));
}

/** A restarted with every rendezvous check failing for ~90 s (debug switch):
 * it starts without the auth, works, and reconnects with it by itself. */
async function offline(state) {
  console.log('\n── Offline start, then back');
  record('A: ready while the rendezvous does not answer', await waitReady(A, 240));
  const st = await status(A);
  record('A: started without the network auth', st.started_offline === true && st.configs_with_auth === 0, JSON.stringify({ started_offline: st.started_offline, configs_with_auth: st.configs_with_auth }));
  let polls = { ok: false };
  const read = await until(async () => (polls = await op(A, { op: 'polls' })).ok && Array.isArray(polls.data), 120, 5000);
  record('A: its polls read while offline', read, polls.error || `${polls.data?.length} polls`);
  const t0 = Date.now();
  const back = await until(async () => { const s = await status(A); return s.started_offline === false && s.configs_with_auth === 2 && s.app?.ready === true; }, 300, 5000);
  record('A: reconnects with the auth by itself', back, `${Math.round((Date.now() - t0) / 1000)} s after the check began`);
  if (state?.hash) {
    record("A: its poll still lists after the reconnect", await until(async () => ((await op(A, { op: 'polls' })).data || []).some((p) => p.hash === state.hash), 180, 5000));
  }
  const rss = (await status(A)).rss_mb || {};
  // Watch item (11-03): the 0.6 conductor at ~800 MB after a fresh start.
  record('A: both conductors under 1.5 GB after the run', (rss['holochain-0.6'] ?? 9999) < 1536 && (rss['holochain-0.7'] ?? 9999) < 1536, JSON.stringify(rss));
}

/** The machine-death restore, part 1: A backs up (its seed escrowed in the
 * Vault backup), then B adopts that seed and exits (a debug build does). */
async function adopt(state) {
  console.log('\n── Restore onto another device (adopt the escrowed seed)');
  // C: a peer that stays up (staging has no ProofPoll node): once A is
  // gone, the network still holds A's public records for B to fetch.
  record('C: ready', await waitReady(C, 300));
  if (state?.hash) {
    record("C: holds A's poll before A goes", await until(async () => ((await op(C, { op: 'polls' })).data || []).some((p) => p.hash === state.hash), 300, 10000));
    record("C: holds A's vote", await until(async () => ((await op(C, { op: 'votes', poll: state.hash, dna_version: '1.4' })).data || []).length >= 1, 300, 10000));
  }
  const b = await op(A, { op: 'backup', client_id: CLIENT_ID }, 120);
  record('A: backs up to the Vault', b.ok, b.error || JSON.stringify(b.data)?.slice(0, 120));
  const esc = await op(B, { op: 'escrow', client_id: CLIENT_ID });
  record('B: the Vault backup holds an escrowed seed', esc.ok && esc.data?.seed === true, esc.error || JSON.stringify(esc.data));
  const ad = await op(B, { op: 'adopt', client_id: CLIENT_ID }, 180).catch((e) => ({ ok: true, data: { exited: String(e) } }));
  record('B: adopts it', ad.ok, ad.error || JSON.stringify(ad.data));
}

/** Part 2, B relaunched: it is A's agent now, both lines start under the
 * new key, and A's encrypted reason opens. */
async function adopted(state) {
  console.log('\n── After the restore (B relaunched)');
  record('B: ready on the adopted seed', await waitReady(B, 300));
  const st = await status(B);
  record("B: runs as A's agent", !!state.agent && st.app?.agent_pub_key === state.agent, JSON.stringify({ b: st.app?.agent_pub_key, a: state.agent }));
  record('B: both conductors up with the network auth', st.configs_with_auth === 2 && st.app?.conductor_status?.status === 'ready', JSON.stringify(st.app?.conductor_status));
  if (state.vote) {
    let r = { ok: false };
    const opened = await until(async () => (r = await op(B, { op: 'rationale', vote: state.vote })).ok && r.data === 'because the matrix said so', 300, 10000);
    record("B: opens A's encrypted reason (A's key is B's now)", opened, r.error || JSON.stringify(r.data));
  }
  if (state.hash) {
    record("B: lists A's poll", await until(async () => ((await op(B, { op: 'polls' })).data || []).some((p) => p.hash === state.hash), 300, 10000));
  }
}

// ───────────────────────── main ─────────────────────────

const legs = { fresh, signin: () => signin(), polls, migration };
try {
  if (['devices', 'offline', 'adopt', 'adopted'].includes(LEG)) {
    const s = JSON.parse(process.env.PP_STATE || '{}');
    await ({ devices, offline, adopt, adopted })[LEG](s);
  } else if (LEG === 'polls') {
    const s = await polls();
    if (process.env.PP_STATE_OUT) (await import('node:fs')).writeFileSync(process.env.PP_STATE_OUT, JSON.stringify(s));
  } else if (legs[LEG]) {
    await legs[LEG]();
  } else {
    console.error(`unknown leg ${LEG}`);
    process.exit(2);
  }
} catch (e) {
  record(`${LEG}: ran to the end`, false, e.stack || String(e));
}
console.log(`\nRESULT: ${results.length - failures}/${results.length} checks passed — ${failures ? `${failures} FAILED` : 'ALL GREEN'}`);
process.exit(failures ? 1 : 0);
