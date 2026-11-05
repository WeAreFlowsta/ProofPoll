#!/usr/bin/env node
/**
 * ProofPoll's headless matrix: drives test instances (scripts/build-test-binary.sh,
 * the `harness` feature) through their /dev routes, beside a Flowsta Vault test
 * instance. scripts/matrix.sh starts everything and runs the legs in order.
 *
 * Usage: node scripts/pp-matrix.mjs --leg=<fresh|signin|polls|devices|migration|carrier>
 * Env:
 *   PP_A, PP_B     harness URLs of instances A and B (B only for `devices`)
 *   VAULT          the Vault test instance's URL (http://127.0.0.1:27778)
 *   CLIENT_ID      a client id the staging API knows (the staging fixture app)
 */

import { execFile } from 'node:child_process';

const LEG = (process.argv.find((a) => a.startsWith('--leg=')) || '--leg=').split('=')[1];
const A = process.env.PP_A || 'http://127.0.0.1:28801';
const B = process.env.PP_B || 'http://127.0.0.1:28802';
const C = process.env.PP_C || 'http://127.0.0.1:28803';
const D = process.env.PP_D || 'http://127.0.0.1:28804';
const VAULT = process.env.VAULT || 'http://127.0.0.1:27778';
const CLIENT_ID = process.env.CLIENT_ID || '';
const CARRIER = process.env.PP_CARRIER || new URL('../carrier/target/debug/proofpoll-carrier', import.meta.url).pathname;

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

/** Polls, votes, drafts and reasons on the active network (v1.5). */
async function polls() {
  console.log('\n── Polls');
  const title = `matrix poll ${nowS()}`;
  const created = await op(A, { op: 'create_poll', title, description: 'from the matrix', options: ['yes', 'no', 'maybe'], closes_at: nowS() + 7 * 86400 });
  record('A: creates a poll', created.ok && typeof created.data === 'string', created.error || created.data);
  const hash = created.data;
  record('A: lists it', await until(async () => ((await op(A, { op: 'polls' })).data || []).some((p) => p.hash === hash || p.poll?.title === title), 60));
  const voted = await op(A, { op: 'vote', poll: hash, option: 1, dna_version: '1.5' });
  record('A: votes', voted.ok, voted.error || voted.data);
  record('A: reads its vote', await until(async () => ((await op(A, { op: 'votes', poll: hash, dna_version: '1.5' })).data || []).length >= 1, 60));
  const reason = await op(A, { op: 'save_rationale', vote: voted.data, text: 'because the matrix said so' });
  record('A: saves a reason for the vote', reason.ok, reason.error);
  const readReason = await op(A, { op: 'rationale', vote: voted.data });
  record('A: reads the reason back', readReason.ok && readReason.data === 'because the matrix said so', readReason.error || JSON.stringify(readReason.data));
  const draft = await op(A, { op: 'save_draft', title: `draft ${nowS()}`, description: 'd', options: ['one', 'two'], closes_at: nowS() + 86400 });
  record('A: saves a draft', draft.ok, draft.error);
  record('A: lists the draft', await until(async () => ((await op(A, { op: 'drafts' })).data || []).some((d) => d.action_hash === draft.data || d.draft_action_hash === draft.data || d.hash === draft.data), 30), JSON.stringify((await op(A, { op: 'drafts' })).data)?.slice(0, 200));
  const pub = await op(A, { op: 'publish_draft', draft: draft.data });
  record('A: publishes the draft as a poll', pub.ok, pub.error || pub.data);
  return { hash, vote: voted.data, title, published: pub.data, agent: (await status(A)).app?.agent_pub_key };
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
    record("B: sees A's vote", await until(async () => ((await op(B, { op: 'votes', poll: state.hash, dna_version: '1.5' })).data || []).length >= 1, 300, 10000));
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
  const reasonNow = await op(A, { op: 'rationale', vote: vote13.data });
  record('A: the old reason reads', reasonNow.ok && reasonNow.data === 'an old reason', reasonNow.error || JSON.stringify(reasonNow.data));

  // The move to v1.5: the v1.3 records and the 0.5 move's v1.4 copies
  // carried with their proofs, each poll once.
  console.log('\n── The move to v1.5 (carried with their proofs)');
  const t1 = Date.now();
  const mv = await op(A, { op: 'move15' }, 600);
  const rep = mv.data?.report || {};
  record('A: the move to v1.5 runs to the end', mv.ok && mv.data?.complete === true && rep.failed === 0 && rep.waiting === 0, mv.error || JSON.stringify(rep));
  console.log(`  (took ${Math.round((Date.now() - t1) / 1000)} s)`);
  record('A: both v1.3 polls carried, the open one\'s v1.4 copy mapped to its copy', rep.polls >= 2 && rep.later >= 1, JSON.stringify(rep));
  const after = (await op(A, { op: 'polls' })).data || [];
  const titled = (t) => after.filter((p) => p.poll?.title?.startsWith(t));
  record('A: each poll lists once, from v1.5', titled('v13 open').length === 1 && titled('v13 closing').length === 1 && [...titled('v13 open'), ...titled('v13 closing')].every((p) => p.dna_version === '1.5'),
    JSON.stringify(after.map((p) => [p.poll?.title, p.dna_version])).slice(0, 300));
  const copy = titled('v13 open')[0]?.hash;
  const later = st.polls_migrated?.[0]?.new_hash;
  const viaOld = await op(A, { op: 'poll', poll: open.data });
  const viaLater = later ? await op(A, { op: 'poll', poll: later }) : { ok: false };
  record('A: the v1.3 hash and its v1.4 copy\'s both open the v1.5 copy', viaOld.data?.hash === copy && viaLater.data?.hash === copy && viaOld.data?.dna_version === '1.5',
    JSON.stringify({ copy, old: viaOld.data?.hash, later: viaLater.data?.hash }));
  const votes15 = (await op(A, { op: 'votes', poll: copy, dna_version: '1.5' })).data || [];
  const aKey = (await status(A)).app?.agent_pub_key;
  record('A: its vote counts once there, as its own', votes15.length === 1 && votes15[0].author === aKey, JSON.stringify(votes15.map((v) => [v.author?.slice(0, 12), v.vote?.hash?.slice(0, 12)])));
  const reason15 = votes15[0] ? await op(A, { op: 'rationale', vote: votes15[0].vote.hash }) : { ok: false };
  record('A: its old reason reads on the carried vote', reason15.ok && reason15.data === 'an old reason', reason15.error || JSON.stringify(reason15.data));
  const drafts15 = (await op(A, { op: 'drafts' })).data || [];
  record('A: the old draft lists once', drafts15.filter((d) => JSON.stringify(d).includes('v13 draft')).length === 1, JSON.stringify(drafts15).slice(0, 200));
  const again = await op(A, { op: 'move15' }, 600);
  const rep2 = again.data?.report || {};
  record('A: a second run writes nothing new', again.ok && rep2.failed === 0 && (await op(A, { op: 'polls' })).data?.length === after.length && ((await op(A, { op: 'drafts' })).data || []).length === drafts15.length,
    again.error || JSON.stringify(rep2));
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
  // C and D: peers that stay up (staging has no ProofPoll node). Once A is
  // gone they hold A's public records, and a restore needs two peers that
  // agree on A's chain head (restore_chain_quorum, 2 by default).
  for (const [name, base] of [['C', C], ['D', D]]) {
    record(`${name}: ready`, await waitReady(base, 300));
    if (state?.hash) {
      record(`${name}: holds A's poll before A goes`, await until(async () => ((await op(base, { op: 'polls' })).data || []).some((p) => p.hash === state.hash), 300, 10000));
      record(`${name}: holds A's vote`, await until(async () => ((await op(base, { op: 'votes', poll: state.hash, dna_version: '1.5' })).data || []).length >= 1, 300, 10000));
    }
    // A's last public write: a restore soon after authoring elsewhere can pin
    // a stale head (upstream's caution), so the peers hold it first.
    if (state?.published) {
      record(`${name}: holds A's latest poll`, await until(async () => ((await op(base, { op: 'polls' })).data || []).some((p) => p.hash === state.published), 300, 10000));
    }
  }
  const oneEach = (d) => d?.forked_seqs?.length === 0 && d?.actions === d?.head_seq + 1;
  const chainA = await op(A, { op: 'chain07' });
  record("A: its v1.4 chain, one action per seq", chainA.ok && oneEach(chainA.data), chainA.error || JSON.stringify(chainA.data));
  const chainA15 = await op(A, { op: 'chain07', dna: '1.5' });
  record("A: its v1.5 chain, one action per seq", chainA15.ok && oneEach(chainA15.data), chainA15.error || JSON.stringify(chainA15.data));
  if (process.env.PP_STATE_OUT) (await import('node:fs')).writeFileSync(process.env.PP_STATE_OUT, JSON.stringify({ ...state, a_head: chainA.data?.head_seq, a_head15: chainA15.data?.head_seq }));
  const b = await op(A, { op: 'backup', client_id: CLIENT_ID }, 120);
  record('A: backs up to the Vault', b.ok, b.error || JSON.stringify(b.data)?.slice(0, 120));
  const esc = await op(B, { op: 'escrow', client_id: CLIENT_ID });
  record('B: the Vault backup holds an escrowed seed', esc.ok && esc.data?.seed === true, esc.error || JSON.stringify(esc.data));
  const ad = await op(B, { op: 'adopt', client_id: CLIENT_ID }, 180).catch((e) => ({ ok: true, data: { exited: String(e) } }));
  record('B: adopts it', ad.ok, ad.error || JSON.stringify(ad.data));
}

/** B's logs since its latest start: the app log is kept across starts; the
 * 0.7 conductor's folder was wiped by the adopt, so its log is new. */
async function logsSinceStart(base) {
  const fs = await import('node:fs');
  const path = await import('node:path');
  const root = path.join(process.env.HOME, '.local/share', `proofpoll-test-${base}`);
  const files = fs.readdirSync(root, { recursive: true }).map(String);
  const read = (re) => files.filter((f) => re.test(f)).map((f) => fs.readFileSync(path.join(root, f), 'utf8')).join('\n');
  const app = read(/com\.proofpoll\.app\/logs\/proofpoll\.log$/);
  return { app: app.slice(app.lastIndexOf('ProofPoll starting up')), conductor07: read(/conductor-07\/holochain-stderr\.log$/) };
}

/** Part 2, B relaunched: it is A's agent now. Its v1.4 chain is A's,
 * rebuilt from the network (no second chain under the key), both lines
 * start under the new key, A's encrypted reason opens, and B's own writes
 * after the restore publish. */
async function adopted(state) {
  console.log('\n── After the restore (B relaunched)');
  let sawRestore = false;
  const ready = await until(async () => {
    const s = await status(B);
    if (s.app?.conductor_status?.status === 'restoring' || s.restore_marker) sawRestore = true;
    return s.app?.ready === true;
  }, 900, 2000);
  record('B: ready on the adopted seed', ready);
  record("B: restored A's chain from the network at its start", sawRestore || /rebuilding this key's chain/.test((await logsSinceStart('pp-b')).app));
  const st = await status(B);
  record('B: the restore marker is gone', st.restore_marker === null, JSON.stringify(st.restore_marker));
  const chainB = await op(B, { op: 'chain07' });
  const c = chainB.data || {};
  record("B: its v1.4 chain is A's, one action per seq", chainB.ok && c.forked_seqs?.length === 0 && c.actions === c.head_seq + 1 && (state.a_head == null || c.head_seq >= state.a_head), chainB.error || JSON.stringify({ ...c, a_head: state.a_head }));
  const chainB15 = await op(B, { op: 'chain07', dna: '1.5' });
  const c15 = chainB15.data || {};
  record("B: its v1.5 chain is A's, one action per seq", chainB15.ok && c15.forked_seqs?.length === 0 && c15.actions === c15.head_seq + 1 && (state.a_head15 == null || c15.head_seq >= state.a_head15), chainB15.error || JSON.stringify({ ...c15, a_head15: state.a_head15 }));
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
  // B's own writes after the restore: signed back in (the adopt dropped the
  // old binding), a new poll lands on top of A's restored chain and reaches C.
  await signin(B, 'B');
  const title = `after the restore ${nowS()}`;
  const made = await op(B, { op: 'create_poll', title, description: 'written by the restored device', options: ['yes', 'no'], closes_at: nowS() + 86400 });
  record('B: creates a poll after the restore', made.ok && typeof made.data === 'string', made.error || made.data);
  record("C: sees B's new poll (B's writes publish)", await until(async () => ((await op(C, { op: 'polls' })).data || []).some((p) => p.hash === made.data), 300, 10000));
  // Writes go to v1.5: that chain grows on A's; v1.4's stays as restored.
  const after = (await op(B, { op: 'chain07', dna: '1.5' })).data || {};
  record("B: its v1.5 chain grew on A's, still one action per seq", after.forked_seqs?.length === 0 && after.actions === after.head_seq + 1 && after.head_seq > (c15.head_seq ?? -1), JSON.stringify(after));
  const after14 = (await op(B, { op: 'chain07' })).data || {};
  record("B: its v1.4 chain as restored, one action per seq", after14.forked_seqs?.length === 0 && after14.actions === after14.head_seq + 1 && after14.head_seq === c.head_seq, JSON.stringify(after14));
  const logs = await logsSinceStart('pp-b');
  record('B: no "head has moved" and no failed publishes', !/head has moved/.test(logs.app) && !/FOREIGN KEY constraint failed/.test(logs.conductor07),
    JSON.stringify({ headMoved: (logs.app.match(/head has moved/g) || []).length, foreignKey: (logs.conductor07.match(/FOREIGN KEY constraint failed/g) || []).length }));
}

/** The node's carrier: ProofPoll 0.4.x data on v1.3 by installs that never
 * update (each instance holds its own move: PROOFPOLL_TEST_HOLD_MOVE), carried
 * to v1.5 by the carrier binary against D's conductors, D standing for the
 * node. Nobody whose records move acts. The staging network holds other runs'
 * polls too, so the counts are at least, not exactly. */
async function carrier() {
  console.log('\n── The node carries what nobody else does');
  for (const [b, w] of [[A, 'A'], [B, 'B'], [C, 'C'], [D, 'D']]) record(`${w}: ready`, await waitReady(b));
  const keyOf = async (b) => (await status(b)).app?.agent_pub_key;
  const [aKey, bKey, cKey] = [await keyOf(A), await keyOf(B), await keyOf(C)];
  const anon = await op(A, { op: 'v13_poll', title: `carrier anonymous ${nowS()}`, description: 'by an install that never updates', options: ['red', 'blue'], closes_at: nowS() + 7 * 86400 });
  const pub = await op(A, { op: 'v13_poll', title: `carrier public ${nowS()}`, description: 'names shown', options: ['yes', 'no'], closes_at: nowS() + 7 * 86400, poll_type: 'Public' });
  record('A: two v1.3 polls, one anonymous and one public', anon.ok && pub.ok, anon.error || pub.error);
  for (const [b, w] of [[B, 'B'], [C, 'C']]) {
    record(`${w}: sees A's v1.3 polls`, await until(async () => {
      const listed = (await op(b, { op: 'polls' })).data || [];
      return listed.some((p) => p.hash === anon.data) && listed.some((p) => p.hash === pub.data);
    }, 300, 10000));
  }
  const votes = [
    await op(A, { op: 'v13_vote', poll: anon.data, option: 0 }),
    await op(B, { op: 'v13_vote', poll: anon.data, option: 1 }),
    await op(C, { op: 'v13_vote', poll: anon.data, option: 0 }),
    await op(B, { op: 'v13_vote', poll: pub.data, option: 0, display_name: 'Voter B' }),
  ];
  record('three votes on the anonymous poll (A, B, C), B named on the public one', votes.every((v) => v.ok), JSON.stringify(votes.map((v) => v.error)));
  const flag = await op(C, { op: 'v13_flag', poll: anon.data, reason: 'Spam' });
  record('C: a flag on the anonymous poll', flag.ok, flag.error);

  const node = (await op(D, { op: 'node_admin' })).data;
  record("D: its conductors' admin ports and origin", !!node?.origin, JSON.stringify(node && { admin_06: node.admin_06, admin_07: node.admin_07 }));
  // The carrier keeps its signing credentials between runs in this file, as
  // the node's job does; each run's new grants are noted.
  const fs = await import('node:fs');
  const path = await import('node:path');
  const dRoot = path.join(process.env.HOME, '.local/share', 'proofpoll-test-pp-d');
  const stateFile = path.join(dRoot, 'carrier-credentials.json');
  fs.rmSync(stateFile, { force: true });
  const grants = [];
  // A connection the carrier drops without a close frame is a warning in
  // D's conductor logs (A, B and C's apps leave none).
  const resets = () => fs.readdirSync(dRoot, { recursive: true }).map(String).filter((f) => /holochain-stderr\.log$/.test(f))
    .reduce((n, f) => n + (fs.readFileSync(path.join(dRoot, f), 'utf8').match(/ResetWithoutClosingHandshake/g) || []).length, 0);
  const resets0 = resets();
  const run = (extra = []) => new Promise((resolve) => execFile(CARRIER,
    ['--admin-06', `localhost:${node.admin_06}`, '--admin-07', `localhost:${node.admin_07}`, '--origin', node.origin, '--pause-ms', '50', '--max-writes', '2000', '--verbose', '--state', stateFile, ...extra],
    { timeout: 900_000, maxBuffer: 1 << 24 },
    (err, stdout, stderr) => {
      let report = null;
      try { report = JSON.parse(String(stdout).trim().split('\n').pop()); } catch { /* none */ }
      grants.push(report?.grants ?? null);
      for (const line of String(stderr).split('\n').filter((l) => /grant|credentials/.test(l))) console.log(`    ${line}`);
      resolve({ code: err ? (err.code ?? 1) : 0, report, stderr: String(stderr) });
    }));
  const show = (r) => JSON.stringify(r.report || { code: r.code, stderr: r.stderr.slice(-400) });

  // D holds what the network gossips to it: the dry run counts until it has
  // every record, then the real run carries them.
  let dry = { report: null };
  const seen = await until(async () => {
    dry = await run(['--dry-run']);
    const s = dry.report?.seen || {};
    return s.polls_v13 >= 2 && s.votes >= 4 && s.flags >= 1;
  }, 600, 15000);
  record('the carrier (dry run) reads both polls, four votes and the flag through D', seen, show(dry));
  record('the dry run writes nothing', dry.report?.wrote === false && dry.report?.carried?.polls >= 2, show(dry));
  const t0 = Date.now();
  const first = await run();
  record('the carrier runs to the end, nothing refused', first.code === 0 && first.report?.failed === 0 && !first.report?.stopped, show(first));
  console.log(`  (took ${Math.round((Date.now() - t0) / 1000)} s)`);
  record('it carried both polls and the four votes and the flag', first.report?.carried?.polls >= 2 && first.report?.carried?.votes >= 4 && first.report?.carried?.flags >= 1, show(first));

  const copyOf = async (old) => (await op(C, { op: 'poll', poll: old })).data?.hash;
  let anonCopy, pubCopy;
  record('C: each v1.3 hash opens its v1.5 copy', await until(async () => {
    [anonCopy, pubCopy] = [await copyOf(anon.data), await copyOf(pub.data)];
    return !!anonCopy && anonCopy !== anon.data && !!pubCopy && pubCopy !== pub.data;
  }, 300, 10000), JSON.stringify({ anonCopy, pubCopy }));
  // A failed read is null, never an empty list: no check passes on it.
  const v15 = async (b, fn, hash) => {
    const r = await op(b, { op: 'v15_records', fn, hash });
    return r.ok && Array.isArray(r.data) ? r.data : null;
  };
  let anonVotes = [];
  await until(async () => ((anonVotes = (await v15(C, 'get_poll_votes', anonCopy)) || []).length >= 3), 300, 10000);
  const by = (rs) => rs.map((v) => [v.carried_author?.slice(0, 12), v.writer?.slice(0, 12), v.fields?.display_name ?? null]);
  record('C, on v1.5 alone: the anonymous poll has three votes, one each by A, B and C', anonVotes.length === 3 && [aKey, bKey, cKey].every((k) => anonVotes.filter((v) => v.carried_author === k).length === 1), JSON.stringify(by(anonVotes)));
  record('no name on an anonymous vote; each carried by another key than its voter\'s', anonVotes.every((v) => !v.fields?.display_name && v.writer !== v.carried_author), JSON.stringify(by(anonVotes)));
  const pubVotes = (await v15(C, 'get_poll_votes', pubCopy)) || [];
  record("the public poll's vote keeps its name, by B", pubVotes.length === 1 && pubVotes[0].fields?.display_name === 'Voter B' && pubVotes[0].carried_author === bKey, JSON.stringify(by(pubVotes)));
  const flags = (await v15(C, 'get_poll_flags', anonCopy)) || [];
  record('the flag is on the copy, by C', flags.length === 1 && flags[0].carried_author === cKey, JSON.stringify(by(flags)));
  const second = await run();
  record('a second run writes nothing', second.code === 0 && second.report?.wrote === false && second.report?.failed === 0, show(second));
  const cells = 2 + (first.report?.v14_read ? 1 : 0);
  record('the credentials are kept: a grant per cell on the first run only', grants[0] === cells && grants.slice(1).every((g) => g === 0), JSON.stringify(grants));
  const mode = fs.statSync(stateFile).mode & 0o777;
  record('the credentials file is readable by its owner only', mode === 0o600, mode.toString(8));

  // B updates: its own move finds its votes carried; a second vote is refused.
  // The carrier's records reach B by gossip first, as they would a voter
  // who updates later (before that, its move retries on its next run).
  record('B: sees the carried votes on both copies', await until(async () => ((await v15(B, 'get_poll_votes', anonCopy)) || []).length >= 3 && ((await v15(B, 'get_poll_votes', pubCopy)) || []).length >= 1, 300, 10000));
  const mv = await op(B, { op: 'move15' }, 600);
  record("B: its own move runs, nothing refused", mv.ok && mv.data?.report?.failed === 0, mv.error || JSON.stringify(mv.data?.report));
  const afterMove = (await v15(C, 'get_poll_votes', anonCopy)) || [];
  record('the anonymous poll still has three votes', afterMove.length === 3, JSON.stringify(by(afterMove)));
  await signin(B, 'B');
  const recast = await op(B, { op: 'vote', poll: anonCopy, option: 0, dna_version: '1.5' });
  record('B: a second vote on the copy is refused', !recast.ok, recast.error || JSON.stringify(recast.data));

  // A deletes its public poll and C removes its flag, on v1.3: the carrier
  // removes the copies it made.
  const del = await op(A, { op: 'v13_delete', poll: pub.data });
  const unflag = await op(C, { op: 'v13_unflag', flag: flag.data });
  record('A deletes its public poll on v1.3; C removes its flag', del.ok && unflag.ok, del.error || unflag.error);
  let removed = 0;
  let last = { report: null };
  const mirrored = await until(async () => {
    last = await run();
    removed += last.report?.carried?.deleted ?? 0;
    console.log(`  (a run: ${show(last)})`);
    for (const line of last.stderr.split('\n').filter((l) => l.includes('deleted') || l.includes('removed flag') || l.includes('on copy'))) console.log(`    ${line}`);
    return removed >= 2;
  }, 600, 20000);
  record('the carrier removes both copies', mirrored, show(last));
  let listed = null;
  record("the deleted poll's copy no longer lists on v1.5", await until(async () => (listed = await v15(C, 'get_all_polls')) !== null && !listed.some((p) => p.hash === pubCopy), 300, 10000), listed === null ? 'the list did not read' : `${listed.length} listed`);
  let left = null;
  record('the removed flag is gone from v1.5', await until(async () => (left = await v15(C, 'get_poll_flags', anonCopy)) !== null && left.length === 0, 300, 10000), JSON.stringify(left && by(left)));
  record('no run added a grant after the first', grants.slice(1).every((g) => g === 0), JSON.stringify(grants));
  record("every run closed its connections (no reset in D's conductor logs)", resets() === resets0, `${resets() - resets0} new over ${grants.length} runs`);
}

// ───────────────────────── main ─────────────────────────

const legs = { fresh, signin: () => signin(), polls, migration, carrier };
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
