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

import { LEG, A, B, C, D, VAULT, CLIENT_ID, record, until, status, op, waitReady, nowS, sleep, signin, summary } from './matrix-lib.mjs';
import { polls, migration, carrier } from './matrix-polls.mjs';

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


/** A restore that comes back shorter than the backup. The device that holds
 * the key (B, since the adopt leg) writes with no network and backs up, so the
 * Vault backup holds records no peer has; a new device (A's folder, wiped)
 * adopts the seed. Its restore must not open on the shorter chain. */
async function shortWrite(state) {
  console.log('\n── A short restore: the old device writes with no network, then backs up');
  record('B: ready with no network', await waitReady(B, 300));
  const st = await status(B);
  record('B: started without the network auth', st.started_offline === true, JSON.stringify({ started_offline: st.started_offline }));
  const before = (await op(B, { op: 'chain07', dna: '1.5' })).data || {};
  const made = await op(B, { op: 'create_poll', title: `never left the old device ${nowS()}`, description: 'written with no network', options: ['yes', 'no'], closes_at: nowS() + 86400 }, 300);
  record('B: creates a poll with no network', made.ok && typeof made.data === 'string', made.error || made.data);
  const after = (await op(B, { op: 'chain07', dna: '1.5' })).data || {};
  record('B: its v1.5 chain grew', after.head_seq > before.head_seq, JSON.stringify({ before: before.head_seq, after: after.head_seq }));
  const b = await op(B, { op: 'backup', client_id: CLIENT_ID }, 120);
  record('B: backs up to the Vault', b.ok, b.error || JSON.stringify(b.data)?.slice(0, 120));
  await sleep(5000);
  for (const [name, base] of [['C', C], ['D', D]]) {
    const has = ((await op(base, { op: 'polls' })).data || []).some((p) => p.hash === made.data);
    record(`${name}: does not hold that poll`, !has);
  }
  if (process.env.PP_STATE_OUT) (await import('node:fs')).writeFileSync(process.env.PP_STATE_OUT, JSON.stringify({ ...state, short_poll: made.data, short_head_before: before.head_seq, short_head: after.head_seq }));
}

async function shortAdopt() {
  console.log('\n── A short restore: a new device adopts the seed');
  record('A: ready as a new install', await waitReady(A, 300));
  await signin(A, 'A');
  const esc = await op(A, { op: 'escrow', client_id: CLIENT_ID });
  record('A: the Vault backup holds an escrowed seed', esc.ok && esc.data?.seed === true, esc.error || JSON.stringify(esc.data));
  const ad = await op(A, { op: 'adopt', client_id: CLIENT_ID }, 180).catch((e) => ({ ok: true, data: { exited: String(e) } }));
  record('A: adopts it', ad.ok, ad.error || JSON.stringify(ad.data));
}

/** The new device waits: it names what is missing, stays closed, and steps
 * off the network under the key (the short restore is set aside). */
async function shortWaits(state) {
  console.log('\n── A short restore: the new device waits, off the network');
  let cs = null;
  const named = await until(async () => { cs = (await status(A)).app?.conductor_status; return cs?.status === 'restoring' && cs.missing > 0; }, 600, 2000);
  record('A: says how many records the network does not hold yet', named, JSON.stringify(cs));
  // What the old device wrote with no network: the poll's records, and at
  // most one action its own start wrote on each of the two chains.
  const written = state.short_head - state.short_head_before;
  record('A: that many are what the old device wrote with no network', cs?.missing >= 1 && cs?.missing <= written + 2, JSON.stringify({ missing: cs?.missing, poll_records: written }));
  record('A: sets the short restore aside', await until(async () => /the short restore is set aside and this device is off the network/.test((await logsSinceStart('pp-a')).app), 60, 3000));
  const gone = await op(A, { op: 'chain07', dna: '1.5' });
  record('A: holds no v1.5 chain meanwhile', !gone.ok && /not installed/.test(gone.error || ''), gone.error || JSON.stringify(gone.data));
  await sleep(45000);
  record('A: does not ask again by itself', !/asking the network again/.test((await logsSinceStart('pp-a')).app));
  const st = await status(A);
  record('A: still not open', st.app?.ready !== true && st.app?.conductor_status?.status === 'restoring', JSON.stringify(st.app?.conductor_status));
  record('A: the restore marker is kept', !!st.restore_marker && st.restore_marker.heads?.['1.5'] >= 1, JSON.stringify(st.restore_marker));
}

/** The old device is back online: its last records reach the peers, and the
 * new device's next ask comes back whole. */
async function shortWhole(state) {
  console.log('\n── A short restore: the old device comes back, the new one opens whole');
  record('B: ready again, with the network', await until(async () => { const s = await status(B); return s.app?.ready === true && s.started_offline === false; }, 300, 5000));
  for (const [name, base] of [['C', C], ['D', D]]) {
    record(`${name}: now holds the poll the old device wrote with no network`, await until(async () => ((await op(base, { op: 'polls' })).data || []).some((p) => p.hash === state.short_poll), 900, 10000));
  }
  // As a person does it: "Try again" once the old device has been open a
  // few minutes, and again later while the screen still says records are
  // missing. Not at once: each try puts this device on the network under
  // the key for a moment, and the peers then follow it instead of the old
  // device until the old device signs a new note (up to about 15 minutes;
  // tried each minute on 11-13, the old device's last action on one chain
  // never arrived in 20).
  let presses = 0;
  let pressedAt = 0;
  const opened = await until(async () => {
    const st = await status(A);
    if (st.app?.ready === true) return true;
    const cs = st.app?.conductor_status;
    const due = presses === 0 || Date.now() - pressedAt > 20 * 60 * 1000;
    if (cs?.status === 'restoring' && (presses === 0 || cs.missing > 0) && due) {
      const again = await op(A, { op: 'restore_try_again' });
      // Not taken while the app still asks for the wait (its screen says how long).
      if (again.ok && again.data?.asked) { presses += 1; pressedAt = Date.now(); console.log(`    (Try again, press ${presses}, ${new Date().toISOString().slice(11, 19)})`); }
    }
    return false;
  }, 65 * 60, 10000);
  record('A: "Try again" (as often as the screen still said records were missing)', presses >= 1, JSON.stringify({ presses }));
  record('A: opens', opened);
  const st = await status(A);
  record('A: the restore marker is gone', st.restore_marker === null, JSON.stringify(st.restore_marker));
  const c15 = (await op(A, { op: 'chain07', dna: '1.5' })).data || {};
  record("A: its v1.5 chain reaches the backup's head, one action per seq", c15.forked_seqs?.length === 0 && c15.actions === c15.head_seq + 1 && c15.head_seq >= state.short_head, JSON.stringify({ ...c15, backup_head: state.short_head }));
  record('A: lists the poll written with no network', await until(async () => ((await op(A, { op: 'polls' })).data || []).some((p) => p.hash === state.short_poll), 300, 10000));
  const logs = await logsSinceStart('pp-a');
  record('A: nobody chose to go on without them', !/chose to go on/.test(logs.app));
}

/** The old device is gone: the person says to go on, and the new device
 * opens on what the network holds. */
async function shortGoOn(state) {
  console.log('\n── A short restore: the old device is gone, the person goes on without them');
  const went = await op(A, { op: 'restore_go_on' });
  record('A: "Continue without them"', went.ok, went.error || JSON.stringify(went.data));
  record('A: opens', await until(async () => (await status(A)).app?.ready === true, 900, 3000));
  const c15 = (await op(A, { op: 'chain07', dna: '1.5' })).data || {};
  record('A: its v1.5 chain is what the network held, one action per seq', c15.forked_seqs?.length === 0 && c15.actions === c15.head_seq + 1, JSON.stringify({ ...c15, backup_head: state.short_head }));
  const logs = await logsSinceStart('pp-a');
  record('A: its log says the person chose, and how many records', /is enabled at seq \d+ though the backup holds to seq \d+: the person chose to go on without the last \d+ record/.test(logs.app));
  record('A: the restore marker is gone', (await status(A)).restore_marker === null);
  const has = ((await op(A, { op: 'polls' })).data || []).some((p) => p.hash === state.short_poll);
  record('A: does not list the poll that never left the old device', !has);
  await signin(A, 'A');
  const made = await op(A, { op: 'create_poll', title: `after going on ${nowS()}`, description: 'written by the new device', options: ['yes', 'no'], closes_at: nowS() + 86400 });
  record('A: creates a poll', made.ok && typeof made.data === 'string', made.error || made.data);
  record("C: sees A's new poll", await until(async () => ((await op(C, { op: 'polls' })).data || []).some((p) => p.hash === made.data), 300, 10000));
}

// ───────────────────────── main ─────────────────────────

const legs = { fresh, signin: () => signin(), polls, migration, carrier };
try {
  const withState = { devices, offline, adopt, adopted, 'short-write': shortWrite, 'short-adopt': shortAdopt, 'short-waits': shortWaits, 'short-whole': shortWhole, 'short-go-on': shortGoOn };
  if (withState[LEG]) {
    const s = JSON.parse(process.env.PP_STATE || '{}');
    await withState[LEG](s);
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
const { checks, failures } = summary();
console.log(`\nRESULT: ${checks - failures}/${checks} checks passed — ${failures ? `${failures} FAILED` : 'ALL GREEN'}`);
process.exit(failures ? 1 : 0);
