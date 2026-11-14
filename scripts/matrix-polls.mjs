/**
 * The legs that are about POLLS: creating, voting, drafts and vote reasons;
 * the move of ProofPoll's own data between its DNA versions; the node's
 * carrier. A fork replaces this file with legs for its own app. The other
 * legs (pp-matrix.mjs: a fresh start, sign-in, two devices, offline and
 * back, a restore on a new device) run for any app; they call a few of the
 * operations below (`create_poll`, `polls`, `vote`, `votes`) as "write
 * something" and "read it back", which is where a fork puts its own.
 */
import { A, B, C, D, CLIENT_ID, CARRIER, execFile, record, until, status, op, waitReady, nowS, sleep, signin } from './matrix-lib.mjs';

/** Polls, votes, drafts and reasons on the active network (v1.5). */
export async function polls() {
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

/** ProofPoll 0.4.x's data on v1.3, then the migration as a start runs it. */
export async function migration() {
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
  const aLinked = await op(A, { op: 'linked_active' });
  record('A: v1.5 links its key to the identity (its own link from the sign-in)', aLinked.ok && aLinked.data === true, aLinked.error || JSON.stringify(aLinked.data));
}

/** The node's carrier: ProofPoll 0.4.x data on v1.3 by installs that never
 * update (each instance holds its own move: PROOFPOLL_TEST_HOLD_MOVE), carried
 * to v1.5 by the carrier binary against D's conductors, D standing for the
 * node. Nobody whose records move acts. The staging network holds other runs'
 * polls too, so the counts are at least, not exactly. */
export async function carrier() {
  console.log('\n── The node carries what nobody else does');
  for (const [b, w] of [[A, 'A'], [B, 'B'], [C, 'C'], [D, 'D']]) record(`${w}: ready`, await waitReady(b));
  const keyOf = async (b) => (await status(b)).app?.agent_pub_key;
  const [aKey, bKey, cKey] = [await keyOf(A), await keyOf(B), await keyOf(C)];
  // B signs in on v1.3 as ProofPoll 0.4.x did: its only identity link is
  // there. Once its move carries that link, v1.5 links its key to the
  // identity, and the app has nothing to ask the Vault for.
  const b13 = await op(B, { op: 'v13_link', client_id: CLIENT_ID });
  record('B: signed in on v1.3 as 0.4.x did (its only link there)', b13.ok, b13.error);
  const bBefore = await op(B, { op: 'linked_active' });
  const bCarriedBefore = (await status(B)).app?.links_carried;
  record('B: before its move, v1.5 links its key to no identity', bBefore.ok && bBefore.data === false && bCarriedBefore === false,
    bBefore.error || JSON.stringify({ linked: bBefore.data, links_carried: bCarriedBefore }));
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
  record('B: its move carried its links', (await status(B)).app?.links_carried === true);
  let bAfter = { ok: false };
  record('B: its carried v1.3 link links its key to the identity on v1.5 (no new link to ask the Vault for)',
    await until(async () => (bAfter = await op(B, { op: 'linked_active' })).ok && bAfter.data === true, 120, 5000), bAfter.error || JSON.stringify(bAfter.data));
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
