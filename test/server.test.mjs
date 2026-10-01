/**
 * Tests für den Datenserver und das Home-Assistant-Add-on.
 *
 *   npm test
 *
 * Kein Framework, nur node:test — der Server hat keine Abhängigkeiten, die
 * Tests auch nicht. openssl wird für die https-Fälle gebraucht.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { mkdtemp, readFile, readdir, writeFile, mkdir } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { request as httpsRequest } from 'node:https';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { X509Certificate } from 'node:crypto';
import { createStore } from '../chordwright-data/server/store.mjs';
import { createBackups } from '../chordwright-data/server/backups.mjs';
import { createJournal, cleanName } from '../chordwright-data/server/changes.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const ADDON = join(here, '..', 'chordwright-data');
const SERVER = join(ADDON, 'server');

const tempDir = () => mkdtemp(join(tmpdir(), 'chordwright-test-'));
const doc = (id, text = `{title: ${id}}\n[C]la`, origin = 'seed') =>
  JSON.stringify({ v: 1, data: { id, text, origin, updatedAt: new Date().toISOString() } });

let nextPort = 20000 + Math.floor(Math.random() * 20000);

/**
 * Which program runs serve.mjs. Normally this Node; with
 * CHORDWRIGHT_SERVER_BIN set, the compiled binary instead (npm run build:bin),
 * so the same tests check what the desktop app ships. start.mjs is the add-on's
 * and always runs under Node.
 */
const BIN = process.env.CHORDWRIGHT_SERVER_BIN;
function command(args) {
  return BIN && args[0] === join(SERVER, 'serve.mjs') ? [BIN, args.slice(1)] : [process.execPath, args];
}

/** Start a process that runs the server, wait for /api/health, hand back a stopper. */
async function launch(args, { env = {}, scheme = 'http', port } = {}) {
  const child = spawn(...command(args), {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '';
  child.stdout.on('data', (d) => (output += d));
  child.stderr.on('data', (d) => (output += d));
  const exited = new Promise((r) => child.on('exit', (code) => r(code)));

  const deadline = Date.now() + 10_000;
  for (;;) {
    if (child.exitCode !== null) throw new Error(`server exited early:\n${output}`);
    try {
      const res = await get(`${scheme}://127.0.0.1:${port}/api/health`);
      if (res.status === 200) break;
    } catch {
      /* not up yet */
    }
    if (Date.now() > deadline) {
      child.kill();
      throw new Error(`server did not come up:\n${output}`);
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  return {
    output: () => output,
    stop: async () => {
      // Windows has no SIGTERM: kill() ends the process outright, and the
      // add-on's start.mjs never gets to stop the server it started — which
      // lives on, holding our pipes open, and the test run never ends. So the
      // whole tree there.
      if (process.platform === 'win32') spawnSync('taskkill', ['/pid', String(child.pid), '/t', '/f']);
      else child.kill('SIGTERM');
      await exited;
    },
  };
}

/** GET that also works against a self-signed certificate. */
function get(url, headers = {}, method = 'GET') {
  if (url.startsWith('https:')) {
    return new Promise((resolve, reject) => {
      const req = httpsRequest(url, { method, headers, rejectUnauthorized: false }, (res) => {
        let body = '';
        res.on('data', (d) => (body += d));
        res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body }));
      });
      req.on('error', reject);
      req.end();
    });
  }
  return fetch(url, { method, headers }).then(async (res) => ({
    status: res.status,
    headers: Object.fromEntries(res.headers),
    body: await res.text(),
  }));
}

function selfSignedCert(dir) {
  const cert = join(dir, 'cert.pem');
  const key = join(dir, 'key.pem');
  execFileSync('openssl', [
    'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
    '-subj', '/CN=localhost', '-keyout', key, '-out', cert,
  ], { stdio: 'ignore' });
  return { cert, key };
}

// ---------------------------------------------------------------------------
// store.mjs
// ---------------------------------------------------------------------------

test('store: parallel song writes keep every sidecar entry', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();

  await Promise.all(Array.from({ length: 100 }, (_, i) => store.write('library', `doc.song-${i}`, doc(`song-${i}`))));

  const meta = JSON.parse(await readFile(join(root, 'library/songs/_documents.json'), 'utf8'));
  assert.equal(Object.keys(meta).length, 100);
  for (let i = 0; i < 100; i++) assert.deepEqual(meta[`song-${i}`], { origin: 'seed', v: 1 });

  const all = await store.readAll('library');
  for (let i = 0; i < 100; i++) assert.equal(JSON.parse(all[`doc.song-${i}`]).data.origin, 'seed');
});

test('store: parallel writes and removes leave exactly the survivors', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  for (let i = 0; i < 40; i++) await store.write('library', `doc.s${i}`, doc(`s${i}`));

  // Remove the even ones while writing twenty new ones, all at once.
  await Promise.all([
    ...Array.from({ length: 20 }, (_, i) => store.remove('library', `doc.s${i * 2}`)),
    ...Array.from({ length: 20 }, (_, i) => store.write('library', `doc.n${i}`, doc(`n${i}`, 'x', 'user'))),
  ]);

  const meta = JSON.parse(await readFile(join(root, 'library/songs/_documents.json'), 'utf8'));
  const expected = [
    ...Array.from({ length: 20 }, (_, i) => `s${i * 2 + 1}`),
    ...Array.from({ length: 20 }, (_, i) => `n${i}`),
  ].sort();
  assert.deepEqual(Object.keys(meta).sort(), expected);
  const files = (await readdir(join(root, 'library/songs'))).filter((f) => f.endsWith('.chordpro'));
  assert.deepEqual(files.map((f) => f.slice(0, -'.chordpro'.length)).sort(), expected);
});

test('store: the sidecar is valid JSON at every moment of a write storm', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  await store.write('library', 'doc.first', doc('first'));

  const sidecar = join(root, 'library/songs/_documents.json');
  let reads = 0;
  let broken = 0;
  let done = false;
  const reader = (async () => {
    while (!done) {
      try {
        JSON.parse(await readFile(sidecar, 'utf8'));
      } catch {
        broken++;
      }
      reads++;
      await new Promise((r) => setImmediate(r));
    }
  })();
  await Promise.all(Array.from({ length: 200 }, (_, i) => store.write('library', `doc.w${i}`, doc(`w${i}`))));
  done = true;
  await reader;

  assert.ok(reads > 10, `reader ran (${reads} reads)`);
  assert.equal(broken, 0, 'a reader saw a half-written sidecar');
});

test('store: a failed sidecar update does not jam later ones', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  await assert.rejects(store.write('library', 'doc.bad', 'not json'));
  await store.write('library', 'doc.good', doc('good'));
  const meta = JSON.parse(await readFile(join(root, 'library/songs/_documents.json'), 'utf8'));
  assert.deepEqual(Object.keys(meta), ['good']);
});

test('store: no temp files are left behind or read as records', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  await Promise.all([
    ...Array.from({ length: 30 }, (_, i) => store.write('library', `doc.t${i}`, doc(`t${i}`))),
    ...Array.from({ length: 10 }, (_, i) => store.write('user', `k${i}`, JSON.stringify({ i }))),
  ]);
  const leftovers = [
    ...(await readdir(join(root, 'library/songs'))),
    ...(await readdir(join(root, 'user'))),
  ].filter((f) => f.endsWith('.tmp'));
  assert.deepEqual(leftovers, []);

  assert.equal(store.keyForPath('library/songs/.t1.chordpro.12.3.tmp'), null);
  assert.equal(store.keyForPath('user/.k1.json.12.3.tmp'), null);

  // A temp file that did survive a crash is not mistaken for a record either.
  await writeFile(join(root, 'user', '.k1.json.99.1.tmp'), '{}');
  await writeFile(join(root, 'library/songs', '.t1.chordpro.99.1.tmp'), 'x');
  const user = await store.readAll('user');
  assert.deepEqual(Object.keys(user).sort(), Array.from({ length: 10 }, (_, i) => `k${i}`).sort());
  const library = await store.readAll('library');
  assert.equal(Object.keys(library).filter((k) => k.startsWith('doc.')).length, 30);
});

test('store: a user record is still pretty-printed and round-trips', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  await store.write('user', 'tags', '{"a":[1,2]}');
  assert.equal(await readFile(join(root, 'user/tags.json'), 'utf8'), '{\n  "a": [\n    1,\n    2\n  ]\n}\n');
  assert.deepEqual(JSON.parse(await store.read('user', 'tags')), { a: [1, 2] });
});

test('store: conditional writes — revision match, mismatch, create-only, same bytes', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();

  const first = await store.write('library', 'doc.a', doc('a', 'eins'), { expect: null });
  assert.equal(first.ok, true);
  assert.equal((await store.readVersioned('library', 'doc.a')).rev, first.rev);

  // Create-only on something that exists: refused, current state handed back.
  const again = await store.write('library', 'doc.a', doc('a', 'anders'), { expect: null });
  assert.deepEqual([again.ok, again.rev], [false, first.rev]);
  assert.equal(JSON.parse(again.value).data.text, 'eins');

  const second = await store.write('library', 'doc.a', doc('a', 'zwei'), { expect: first.rev });
  assert.equal(second.ok, true);
  assert.notEqual(second.rev, first.rev);

  // Based on the old revision: refused, nothing written.
  const stale = await store.write('library', 'doc.a', doc('a', 'veraltet'), { expect: first.rev });
  assert.equal(stale.ok, false);
  assert.equal(stale.rev, second.rev);
  assert.equal(await readFile(join(root, 'library/songs/a.chordpro'), 'utf8'), 'zwei');

  // ... unless it would write exactly what is there anyway.
  const same = await store.write('library', 'doc.a', doc('a', 'zwei'), { expect: first.rev });
  assert.deepEqual(same, { ok: true, rev: second.rev });

  // The revision is over the text: a different origin or timestamp is the same song.
  assert.equal((await store.write('library', 'doc.a', doc('a', 'zwei', 'user'), { expect: second.rev })).rev, second.rev);

  // Without expect: unconditional, as before.
  assert.equal((await store.write('library', 'doc.a', doc('a', 'drei'))).ok, true);
});

test('store: an edit by hand changes the revision', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  const { rev } = await store.write('library', 'doc.h', doc('h', 'vorher'));
  await writeFile(join(root, 'library/songs/h.chordpro'), 'in vim geändert');
  const now = await store.readVersioned('library', 'doc.h');
  assert.notEqual(now.rev, rev);
  const refused = await store.write('library', 'doc.h', doc('h', 'App'), { expect: rev });
  assert.equal(refused.ok, false);
  assert.equal(JSON.parse(refused.value).data.text, 'in vim geändert');
});

test('store: conditional removes', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  const { rev } = await store.write('library', 'doc.r', doc('r', 'eins'));
  const edited = await store.write('library', 'doc.r', doc('r', 'zwei'));
  const refused = await store.remove('library', 'doc.r', { expect: rev });
  assert.deepEqual([refused.ok, refused.rev], [false, edited.rev]);
  assert.ok(await readFile(join(root, 'library/songs/r.chordpro'), 'utf8'));
  assert.equal((await store.remove('library', 'doc.r', { expect: edited.rev })).ok, true);
  // Already gone: removing it again is not a conflict.
  assert.equal((await store.remove('library', 'doc.r', { expect: edited.rev })).ok, true);
});

test('store: racing conditional writes on one record — exactly one wins', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  const { rev } = await store.write('library', 'doc.race', doc('race', 'basis'));
  const results = await Promise.all(
    Array.from({ length: 20 }, (_, i) => store.write('library', 'doc.race', doc('race', `Gerät ${i}`), { expect: rev })),
  );
  assert.equal(results.filter((r) => r.ok).length, 1);
  const winner = results.findIndex((r) => r.ok);
  assert.equal(await readFile(join(root, 'library/songs/race.chordpro'), 'utf8'), `Gerät ${winner}`);
});

test('store: readAllVersioned gives a revision for every record', async () => {
  const root = await tempDir();
  const store = createStore(root);
  await store.ensureLayout();
  const a = await store.write('library', 'doc.a', doc('a'));
  const i = await store.write('library', 'index', '[]');
  const { records, revs } = await store.readAllVersioned('library');
  assert.deepEqual(Object.keys(records).sort(), ['doc.a', 'index']);
  assert.deepEqual(revs, { 'doc.a': a.rev, index: i.rev });
});

// ---------------------------------------------------------------------------
// serve.mjs
// ---------------------------------------------------------------------------

test('backups: a snapshot copies both trees and counts songs and sets', async () => {
  const dir = await tempDir();
  const store = createStore(dir);
  await store.ensureLayout();
  await store.write('library', 'doc.a', doc('a'));
  await store.write('library', 'doc.b', doc('b'));
  await store.write('user', 'setlists', JSON.stringify({ v: 1, data: [{ id: 's1' }, { id: 's2' }, { id: 's3' }] }));
  const backups = createBackups(dir);

  const entry = await backups.create();
  assert.equal(entry.reason, 'manual');
  assert.equal(entry.songs, 2);
  assert.equal(entry.sets, 3);
  assert.match(entry.id, /^\d{8}-\d{6}$/);
  const copy = await readFile(join(dir, 'backups', entry.id, 'library', 'songs', 'a.chordpro'), 'utf8');
  assert.equal(copy, '{title: a}\n[C]la');
  assert.deepEqual((await backups.list()).map((b) => b.id), [entry.id]);
  // The snapshot folder is not part of the library.
  assert.deepEqual(Object.keys(await store.readAll('library')).sort(), ['doc.a', 'doc.b']);
});

test('backups: the schedule skips an unchanged or empty library and keeps the newest', async () => {
  const dir = await tempDir();
  const store = createStore(dir);
  await store.ensureLayout();
  let clock = Date.parse('2026-01-01T00:00:00Z');
  const hour = 60 * 60 * 1000;
  const backups = createBackups(dir, { keep: 2, now: () => new Date(clock) });

  assert.equal(await backups.createIfDue(24 * hour), null, 'empty library');
  await store.write('library', 'doc.a', doc('a', 'eins'));
  const first = await backups.createIfDue(24 * hour);
  assert.equal(first.reason, 'auto');
  clock += hour;
  await store.write('library', 'doc.a', doc('a', 'zwei'));
  assert.equal(await backups.createIfDue(24 * hour), null, 'not due yet');
  clock += 24 * hour;
  assert.ok(await backups.createIfDue(24 * hour));
  clock += 24 * hour;
  assert.equal(await backups.createIfDue(24 * hour), null, 'nothing changed');
  const manual = await backups.create('manual');
  for (const text of ['drei', 'vier']) {
    clock += 24 * hour;
    await store.write('library', 'doc.a', doc('a', text));
    assert.ok(await backups.createIfDue(24 * hour));
  }
  const left = await backups.list();
  assert.equal(left.filter((b) => b.reason === 'auto').length, 2);
  assert.ok(left.some((b) => b.id === manual.id), 'a manual snapshot does not age out');
  assert.ok(!left.some((b) => b.id === first.id));
  assert.deepEqual(left.map((b) => b.createdAt), [...left.map((b) => b.createdAt)].sort().reverse());
});

test('backups: restore puts the snapshot back and keeps what was there as its own', async () => {
  const dir = await tempDir();
  const store = createStore(dir);
  await store.ensureLayout();
  await store.write('library', 'doc.a', doc('a', 'alt'));
  await store.write('user', 'tags', JSON.stringify({ v: 1, data: ['x'] }));
  const backups = createBackups(dir);
  const snap = await backups.create();

  await store.write('library', 'doc.a', doc('a', 'neu'));
  await store.write('library', 'doc.b', doc('b'));
  await store.remove('user', 'tags');

  const result = await backups.restore(snap.id);
  assert.equal(result.restored.id, snap.id);
  assert.equal(result.safety.reason, 'restore');
  const lib = await store.readAll('library');
  assert.deepEqual(Object.keys(lib).sort(), ['doc.a']);
  assert.equal(JSON.parse(lib['doc.a']).data.text, 'alt');
  assert.ok(await store.read('user', 'tags'));
  // …and the safety snapshot undoes the restore.
  await backups.restore(result.safety.id);
  assert.deepEqual(Object.keys(await store.readAll('library')).sort(), ['doc.a', 'doc.b']);

  assert.equal(await backups.restore('../library'), null);
  assert.equal(await backups.restore('20990101-000000'), null);
  assert.equal(await backups.remove(snap.id), true);
  assert.equal(await backups.remove(snap.id), false);
});

test('serve: backups over HTTP — list, create, restore, delete, token enforced', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch(
    [join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port), '--token', 't', '--backup-every', '0'],
    { port },
  );
  const base = `http://127.0.0.1:${port}/api`;
  const auth = { authorization: 'Bearer t' };
  try {
    assert.equal((await (await fetch(`${base}/health`)).json()).backups, true);
    assert.equal((await fetch(`${base}/backups`)).status, 401);
    const empty = await (await fetch(`${base}/backups`, { headers: auth })).json();
    assert.deepEqual(empty, { backups: [], everyHours: 0, keep: 14 });

    await fetch(`${base}/library/record/doc.a`, { method: 'PUT', headers: auth, body: doc('a', 'alt') });
    const created = await fetch(`${base}/backups`, { method: 'POST', headers: auth });
    assert.equal(created.status, 201);
    const entry = await created.json();
    assert.equal(entry.songs, 1);

    await fetch(`${base}/library/record/doc.a`, { method: 'PUT', headers: auth, body: doc('a', 'neu') });
    const restored = await fetch(`${base}/backups/${entry.id}/restore`, { method: 'POST', headers: auth });
    assert.equal(restored.status, 200);
    const read = await (await fetch(`${base}/library/record/doc.a`, { headers: auth })).json();
    assert.equal(JSON.parse(read.value).data.text, 'alt');
    assert.equal((await (await fetch(`${base}/backups`, { headers: auth })).json()).backups.length, 2);

    assert.equal((await fetch(`${base}/backups/${entry.id}`, { method: 'DELETE', headers: auth })).status, 204);
    assert.equal((await fetch(`${base}/backups/${entry.id}`, { method: 'DELETE', headers: auth })).status, 404);
    assert.equal((await fetch(`${base}/backups/nope/restore`, { method: 'POST', headers: auth })).status, 404);
    const preflight = await get(`${base}/backups`, {}, 'OPTIONS');
    assert.match(preflight.headers['access-control-allow-methods'], /POST/);
  } finally {
    await server.stop();
  }
});

test('changes: last change per record, a merged recent list, written to disk', async () => {
  const dir = await tempDir();
  let clock = Date.parse('2026-09-30T10:00:00Z');
  const journal = createJournal(dir, { keep: 3, now: () => new Date(clock) });
  await journal.record({ db: 'library', key: 'doc.a', by: 'Anna' });
  clock += 60_000;
  await journal.record({ db: 'library', key: 'doc.a', by: 'Anna' }); // typing on: merged
  clock += 60_000;
  await journal.record({ db: 'user', key: 'setlists', by: ' Jürgen\n', action: 'write' });
  await journal.record({ db: 'library', key: 'doc.b', action: 'file' });
  await journal.record({ db: 'library', key: 'doc.a', by: 'Anna', action: 'remove' });

  assert.deepEqual(await journal.of('library', 'doc.a'), { at: '2026-09-30T10:02:00.000Z', by: 'Anna', action: 'remove' });
  assert.equal((await journal.of('user', 'setlists')).by, 'Jürgen');
  assert.equal(await journal.of('library', 'nope'), null);
  const recent = await journal.recent();
  assert.deepEqual(recent.map((c) => `${c.key}:${c.action}`), ['doc.a:remove', 'doc.b:file', 'setlists:write']);
  assert.deepEqual(Object.keys(await journal.forDb('library')).sort(), ['doc.a', 'doc.b']);

  await journal.flush();
  const again = createJournal(dir);
  assert.equal((await again.of('library', 'doc.b')).action, 'file');
  assert.equal(cleanName('   '), null);
  assert.equal(cleanName('x'.repeat(100)).length, 60);
});

test('serve: who changed what — header, reads, /api/changes, edits by hand, restore', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port), '--backup-every', '0'], { port });
  const base = `http://127.0.0.1:${port}/api`;
  const as = (name) => ({ 'x-chordwright-user': encodeURIComponent(name) });
  try {
    assert.equal((await (await fetch(`${base}/health`)).json()).changes, true);
    await fetch(`${base}/library/record/doc.a`, { method: 'PUT', headers: as('Jürgen'), body: doc('a', 'eins') });
    const read = await (await fetch(`${base}/library/record/doc.a`)).json();
    assert.equal(read.change.by, 'Jürgen');
    assert.equal(read.change.action, 'write');
    const all = await (await fetch(`${base}/library?revs=1`)).json();
    assert.equal(all.changes['doc.a'].by, 'Jürgen');
    assert.ok(typeof (await (await fetch(`${base}/library`)).json())['doc.a'] === 'string', 'plain form unchanged');

    const snap = await (await fetch(`${base}/backups`, { method: 'POST' })).json();
    await fetch(`${base}/library/record/doc.a`, { method: 'DELETE', headers: as('Anna') });
    const gone = await (await fetch(`${base}/library/record/doc.a`)).json();
    assert.equal(gone.change.by, 'Anna');
    assert.equal(gone.change.action, 'remove');

    // Saved by hand: the watcher books it as a file edit, nobody named.
    await new Promise((r) => setTimeout(r, 1_200)); // past the self-write window
    await writeFile(join(dir, 'library', 'songs', 'b.chordpro'), '{title: b}');
    let byHand = null;
    for (let i = 0; i < 40 && !byHand; i++) {
      await new Promise((r) => setTimeout(r, 50));
      byHand = (await (await fetch(`${base}/changes?db=library&key=doc.b`)).json()).change;
    }
    assert.equal(byHand?.action, 'file');
    assert.equal(byHand.by, null);

    await fetch(`${base}/backups/${snap.id}/restore`, { method: 'POST', headers: as('Anna') });
    await new Promise((r) => setTimeout(r, 300));
    const restored = (await (await fetch(`${base}/changes?db=library&key=doc.a`)).json()).change;
    assert.equal(restored.action, 'restore');
    assert.equal(restored.by, 'Anna');
    const recent = (await (await fetch(`${base}/changes?limit=10`)).json()).changes;
    assert.equal(recent[0].action, 'restore');
    assert.equal(recent[0].key, snap.id);
    assert.ok(!recent.some((c) => c.key === 'doc.a' && c.action === 'file'), 'the restore is not booked as edits by hand');
    assert.equal((await fetch(`${base}/changes?db=nope&key=x`)).status, 400);
    const preflight = await get(`${base}/library`, {}, 'OPTIONS');
    assert.match(preflight.headers['access-control-allow-headers'], /x-chordwright-user/);
  } finally {
    await server.stop();
  }
});

test('serve: If-Match / If-None-Match over HTTP, 412 with the current state', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port)], { port });
  const url = `http://127.0.0.1:${port}/api/library/record/doc.x`;
  try {
    const created = await fetch(url, { method: 'PUT', body: doc('x', 'eins'), headers: { 'if-none-match': '*' } });
    assert.equal(created.status, 204);
    const rev = created.headers.get('etag');
    assert.match(rev, /^"[0-9a-f]{16}"$/);

    // A miss says the server keeps revisions too, before anything exists.
    const miss = await fetch(`http://127.0.0.1:${port}/api/user/record/nothing`);
    assert.equal(miss.status, 404);
    assert.ok('rev' in (await miss.json()));
    // …and asked quietly, the same answer without a 404 in the browser's console.
    const quiet = await fetch(`http://127.0.0.1:${port}/api/user/record/nothing?quiet=1`);
    assert.equal(quiet.status, 200);
    assert.deepEqual(await quiet.json(), { value: null, rev: null, change: null, missing: true });
    const found = await fetch(`http://127.0.0.1:${port}/api/library/record/doc.x?quiet=1`);
    assert.equal(found.status, 200);
    assert.equal(JSON.parse((await found.json()).value).data.text, 'eins');

    const read = await (await fetch(url)).json();
    assert.equal(`"${read.rev}"`, rev);
    const all = await (await fetch(`http://127.0.0.1:${port}/api/library?revs=1`)).json();
    assert.equal(`"${all.revs['doc.x']}"`, rev);
    // The plain form is unchanged for older apps.
    assert.ok(typeof (await (await fetch(`http://127.0.0.1:${port}/api/library`)).json())['doc.x'] === 'string');

    const updated = await fetch(url, { method: 'PUT', body: doc('x', 'zwei'), headers: { 'if-match': rev } });
    assert.equal(updated.status, 204);

    const stale = await fetch(url, { method: 'PUT', body: doc('x', 'alt'), headers: { 'if-match': rev } });
    assert.equal(stale.status, 412);
    const body = await stale.json();
    assert.equal(JSON.parse(body.value).data.text, 'zwei');
    assert.equal(`"${body.rev}"`, updated.headers.get('etag'));

    assert.equal((await fetch(url, { method: 'DELETE', headers: { 'if-match': rev } })).status, 412);
    assert.equal((await fetch(url, { method: 'DELETE', headers: { 'if-match': updated.headers.get('etag') } })).status, 204);

    const preflight = await get(url, {}, 'OPTIONS');
    assert.match(preflight.headers['access-control-allow-headers'], /if-match/);
    assert.match(preflight.headers['access-control-allow-headers'], /if-none-match/);
  } finally {
    await server.stop();
  }
});

test('serve: parallel PUTs over HTTP keep every song and sidecar entry', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port)], { port });
  try {
    const results = await Promise.all(
      Array.from({ length: 60 }, (_, i) =>
        fetch(`http://127.0.0.1:${port}/api/library/record/doc.h${i}`, { method: 'PUT', body: doc(`h${i}`) }),
      ),
    );
    assert.ok(results.every((r) => r.status === 204));
    const all = await (await fetch(`http://127.0.0.1:${port}/api/library`)).json();
    for (let i = 0; i < 60; i++) assert.equal(JSON.parse(all[`doc.h${i}`]).data.origin, 'seed');
  } finally {
    await server.stop();
  }
});

test('serve: file edits reach SSE listeners, also after an atomic replace', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port)], { port });
  const controller = new AbortController();
  try {
    const events = [];
    const res = await fetch(`http://127.0.0.1:${port}/api/events`, { signal: controller.signal });
    (async () => {
      const decoder = new TextDecoder();
      try {
        for await (const chunk of res.body) {
          for (const line of decoder.decode(chunk).split('\n')) {
            if (line.startsWith('data: ')) events.push(JSON.parse(line.slice(6)));
          }
        }
      } catch {
        /* aborted */
      }
    })();
    const waitFor = async (what, predicate) => {
      // Generous: file events can lag when the machine is busy; a pass returns at once.
      for (let i = 0; i < 200 && !events.some(predicate); i++) await new Promise((r) => setTimeout(r, 50));
      assert.ok(events.some(predicate), `${what} — events: ${JSON.stringify(events)}`);
      events.length = 0;
    };
    const songs = join(dir, 'library', 'songs');
    const isSong = (id, client = null) => (e) => e.db === 'library' && e.key === `doc.${id}` && e.client === client;

    // The app writes (atomically, by rename) ...
    const put = await fetch(`http://127.0.0.1:${port}/api/library/record/doc.lied`, {
      method: 'PUT', body: doc('lied'), headers: { 'x-chordwright-client': 'app' },
    });
    assert.equal(put.status, 204);
    await waitFor('the app write', isSong('lied', 'app'));
    await new Promise((r) => setTimeout(r, 1100)); // past the self-write window

    // ... and an edit in place afterwards still arrives. With a recursive
    // watch on Linux it did not: the renamed file had dropped out.
    await writeFile(join(songs, 'lied.chordpro'), '{title: lied}\nvon Hand');
    await waitFor('an in-place edit after an atomic write', isSong('lied'));

    // An editor that saves by rename (TextEdit, vim, most IDEs), twice.
    for (const text of ['erste Sicherung', 'zweite Sicherung']) {
      await writeFile(join(songs, '.lied.swp'), `{title: lied}\n${text}`);
      await (await import('node:fs/promises')).rename(join(songs, '.lied.swp'), join(songs, 'lied.chordpro'));
      await waitFor(`an atomic save (${text})`, isSong('lied'));
    }

    await writeFile(join(songs, 'neu.chordpro'), '{title: neu}');
    await waitFor('a dropped-in file', isSong('neu'));

    await writeFile(join(dir, 'user', 'tags.json'), '{}');
    await waitFor('a user record', (e) => e.db === 'user' && e.key === 'tags');
  } finally {
    controller.abort();
    await server.stop();
  }
});

test('serve: as the desktop app starts it — free port, one JSON line, gone with its parent', async () => {
  const dir = await tempDir();
  const child = spawn(
    ...command([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', '0', '--token', 't', '--ready-json', '--exit-with-stdin']),
    { stdio: ['pipe', 'pipe', 'pipe'] },
  );
  const exited = new Promise((r) => child.on('exit', (code) => r(code)));
  try {
    let out = '';
    const line = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`no ready line:\n${out}`)), 10_000);
      child.stdout.on('data', (d) => {
        out += d;
        if (out.includes('\n')) {
          clearTimeout(timer);
          resolve(out.split('\n')[0]);
        }
      });
    });
    const ready = JSON.parse(line);
    assert.equal(ready.ready, true);
    assert.ok(ready.port > 0 && ready.port !== 4174, `a port of the system's choosing: ${ready.port}`);
    assert.equal(ready.url, `http://127.0.0.1:${ready.port}`);
    const res = await get(`${ready.url}/api/library`, { authorization: 'Bearer t' });
    assert.equal(res.status, 200);
  } finally {
    // The app closes its end — or crashes, which closes it just the same.
    child.stdin.end();
  }
  const code = await Promise.race([exited, new Promise((r) => setTimeout(() => r('still running'), 5_000))]);
  if (code === 'still running') child.kill();
  assert.equal(code, 0);
});

test('serve: preflight answers Private Network Access', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(port)], { port });
  try {
    const res = await get(`http://127.0.0.1:${port}/api/library`, {}, 'OPTIONS');
    assert.equal(res.status, 204);
    assert.equal(res.headers['access-control-allow-private-network'], 'true');
  } finally {
    await server.stop();
  }
});

test('serve: https with --cert/--key, token enforced', async () => {
  const dir = await tempDir();
  const { cert, key } = selfSignedCert(dir);
  const port = nextPort++;
  const server = await launch(
    [join(SERVER, 'serve.mjs'), '--dir', join(dir, 'data'), '--port', String(port), '--token', 'geheim', '--cert', cert, '--key', key],
    { port, scheme: 'https' },
  );
  try {
    assert.match(server.output(), /url {4}https:\/\/[^\s]+:\d+\n/);
    assert.equal((await get(`https://127.0.0.1:${port}/api/user`)).status, 401);
    const ok = await get(`https://127.0.0.1:${port}/api/user`, { authorization: 'Bearer geheim' });
    assert.equal(ok.status, 200);
    // Plain http on the same port does not answer as the API.
    const plain = await fetch(`http://127.0.0.1:${port}/api/health`).catch(() => null);
    assert.ok(plain === null || plain.status !== 200, 'plain http must not answer');
  } finally {
    await server.stop();
  }
});

test('serve: --cert without --key is refused', async () => {
  const dir = await tempDir();
  const child = spawn(...command([join(SERVER, 'serve.mjs'), '--dir', dir, '--port', String(nextPort++), '--cert', 'x.pem']), {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let err = '';
  child.stderr.on('data', (d) => (err += d));
  const code = await new Promise((r) => child.on('exit', r));
  assert.equal(code, 1);
  assert.match(err, /--cert and --key go together/);
});

// ---------------------------------------------------------------------------
// Home-Assistant-Add-on (chordwright-data)
// ---------------------------------------------------------------------------

test('addon: every certificate parses — the random serial is always valid DER', async () => {
  // Eine zufällige Seriennummer mit führendem 0x00 war nicht minimal kodiert;
  // OpenSSL 3 lehnte etwa jedes zweihundertste Zertifikat ab. Tausend Stück
  // hätten das fast sicher getroffen.
  const { createCA, issueServerCert } = await import(pathToFileURL(join(ADDON, 'certs.mjs')).href);
  const ca = createCA();
  for (let i = 0; i < 1000; i++) {
    new X509Certificate(createCA().cert);
    new X509Certificate(issueServerCert(ca, { dnsNames: ['a.local'], ips: ['127.0.0.1'] }).cert);
  }
});

test('addon: the CA and the server certificate it issues are what browsers and openssl accept', async () => {
  const { createCA, issueServerCert, SERVER_VALIDITY_DAYS } = await import(pathToFileURL(join(ADDON, 'certs.mjs')).href);
  const dir = await tempDir();
  const ca = createCA();
  const server = issueServerCert(ca, { dnsNames: ['homeassistant.local', 'ha.fritz.box'], ips: ['127.0.0.1', '192.168.68.123', 'fd00::5'] });
  await writeFile(join(dir, 'ca.pem'), ca.cert);
  await writeFile(join(dir, 'server.pem'), server.cert);
  await writeFile(join(dir, 'server-key.pem'), server.key);

  const c = new X509Certificate(ca.cert);
  const x = new X509Certificate(server.cert);
  assert.equal(c.ca, true);
  assert.equal(x.ca, false);
  assert.ok(x.checkIssued(c) && x.verify(c.publicKey), 'issued and signed by the CA');
  assert.equal(x.checkHost('ha.fritz.box'), 'ha.fritz.box');
  assert.equal(x.checkIP('192.168.68.123'), '192.168.68.123');
  assert.equal(x.checkIP('fd00:0:0:0:0:0:0:5'), 'fd00:0:0:0:0:0:0:5');
  assert.deepEqual(x.keyUsage, ['1.3.6.1.5.5.7.3.1']);
  const days = (Date.parse(x.validTo) - Date.now()) / 86_400_000;
  assert.ok(days > SERVER_VALIDITY_DAYS - 1 && days <= 825, `server validity ${days} days`);

  // openssl agrees: the chain verifies, the CA may sign and nothing else.
  execFileSync('openssl', ['verify', '-CAfile', join(dir, 'ca.pem'), join(dir, 'server.pem')]);
  const caText = execFileSync('openssl', ['x509', '-in', join(dir, 'ca.pem'), '-noout', '-text'], { encoding: 'utf8' });
  assert.match(caText, /CA:TRUE/);
  assert.match(caText, /Certificate Sign/);
  const text = execFileSync('openssl', ['x509', '-in', join(dir, 'server.pem'), '-noout', '-text'], { encoding: 'utf8' });
  assert.match(text, /CA:FALSE/);
  assert.match(text, /TLS Web Server Authentication/);
  assert.match(text, /IP Address:192\.168\.68\.123/);
  assert.match(text, /Authority Key Identifier/);
  const pubFromCert = execFileSync('openssl', ['x509', '-in', join(dir, 'server.pem'), '-noout', '-pubkey'], { encoding: 'utf8' });
  const pubFromKey = execFileSync('openssl', ['pkey', '-in', join(dir, 'server-key.pem'), '-pubout'], { encoding: 'utf8' });
  assert.equal(pubFromCert, pubFromKey);
});

/** A stand-in for the Supervisor's /network/info. */
async function fakeSupervisor(addresses) {
  const { createServer } = await import('node:http');
  const state = { addresses };
  const server = createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({
      result: 'ok',
      data: { interfaces: [{ interface: 'eth0', ipv4: { address: state.addresses.v4 }, ipv6: { address: state.addresses.v6 } }] },
    }));
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { url: `http://127.0.0.1:${server.address().port}`, state, close: () => server.close() };
}

/** GET over https, trusting nothing but `ca` — what a device with the CA installed does. */
function getTrusting(ca, url, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = httpsRequest(url, { headers, ca }, (res) => {
      const chunks = [];
      res.on('data', (d) => chunks.push(d));
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) }));
    });
    req.on('error', reject);
    req.end();
  });
}

async function addonDirs() {
  const base = await tempDir();
  const dirs = { data: join(base, 'data'), share: join(base, 'share'), ssl: join(base, 'ssl') };
  for (const d of Object.values(dirs)) await mkdir(d, { recursive: true });
  return dirs;
}

function addonEnv(dirs, port, extra = {}) {
  return { ADDON_DATA_DIR: dirs.data, ADDON_SHARE_DIR: dirs.share, ADDON_SSL_DIR: dirs.ssl, ADDON_PORT: String(port), ...extra };
}

test('addon: no options → token, own CA, a certificate a device with the CA trusts, /ca.crt', async () => {
  const dirs = await addonDirs();
  const port = nextPort++;
  const supervisor = await fakeSupervisor({ v4: ['192.168.68.123/24'], v6: ['fe80::1/64', 'fd00::5/64'] });
  const env = addonEnv(dirs, port, { ADDON_SUPERVISOR_URL: supervisor.url });

  let server = await launch([join(ADDON, 'start.mjs')], { env, port, scheme: 'https' });
  let token;
  let caPem;
  let serverCert;
  try {
    token = (await readFile(join(dirs.data, 'token'), 'utf8')).trim();
    assert.match(token, /^[0-9a-f]{32}$/);
    assert.match(server.output(), new RegExp(`Token {4}${token}`));
    // The log names the real address and where the CA is.
    assert.match(server.output(), /Adresse {2}https:\/\/192\.168\.68\.123:\d+/);
    assert.match(server.output(), /\/ca\.crt/);

    caPem = await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8');
    serverCert = await readFile(join(dirs.data, 'server-cert.pem'), 'utf8');
    const x = new X509Certificate(serverCert);
    assert.equal(x.checkIP('192.168.68.123'), '192.168.68.123', 'the host address from the Supervisor');
    assert.equal(x.checkIP('fd00:0:0:0:0:0:0:5'), 'fd00:0:0:0:0:0:0:5');
    assert.doesNotMatch(x.subjectAltName, /FE80/i, 'no link-local addresses');
    assert.equal(x.checkHost('homeassistant.local'), 'homeassistant.local');

    // The point of it all: trusting only the CA, the handshake succeeds.
    const health = await getTrusting(caPem, `https://127.0.0.1:${port}/api/health`);
    assert.equal(health.status, 200);
    assert.equal(
      (await getTrusting(caPem, `https://127.0.0.1:${port}/api/user`, { authorization: `Bearer ${token}` })).status,
      200,
    );

    // /ca.crt: public, DER, the content type iOS installs as a profile.
    const ca = await getTrusting(caPem, `https://127.0.0.1:${port}/ca.crt`);
    assert.equal(ca.status, 200);
    assert.equal(ca.headers['content-type'], 'application/x-x509-ca-cert');
    assert.deepEqual(new X509Certificate(ca.body).raw, new X509Certificate(caPem).raw);
    // …and as a file in share, for the Mac over Samba.
    assert.equal(await readFile(join(dirs.share, 'chordwright', 'chordwright-ca.crt'), 'utf8'), caPem);

    const put = await new Promise((resolve, reject) => {
      const req = httpsRequest(`https://127.0.0.1:${port}/api/library/record/doc.x`, {
        method: 'PUT', ca: caPem, headers: { authorization: `Bearer ${token}` },
      }, (res) => { res.resume(); res.on('end', () => resolve(res.statusCode)); });
      req.on('error', reject);
      req.end(doc('x'));
    });
    assert.equal(put, 204);
    assert.match(await readFile(join(dirs.share, 'chordwright/library/songs/x.chordpro'), 'utf8'), /\{title: x\}/);
  } finally {
    await server.stop();
  }

  // Restart: same token, same CA, same certificate.
  server = await launch([join(ADDON, 'start.mjs')], { env, port, scheme: 'https' });
  try {
    assert.equal((await readFile(join(dirs.data, 'token'), 'utf8')).trim(), token);
    assert.equal(await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8'), caPem);
    assert.equal(await readFile(join(dirs.data, 'server-cert.pem'), 'utf8'), serverCert);
  } finally {
    await server.stop();
  }

  // The router hands out a new address: a new server certificate, the same CA.
  supervisor.state.addresses = { v4: ['192.168.1.50/24'], v6: [] };
  server = await launch([join(ADDON, 'start.mjs')], { env, port, scheme: 'https' });
  try {
    assert.equal(await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8'), caPem, 'the installed CA stays valid');
    const renewed = new X509Certificate(await readFile(join(dirs.data, 'server-cert.pem'), 'utf8'));
    assert.equal(renewed.checkIP('192.168.1.50'), '192.168.1.50');
    assert.equal((await getTrusting(caPem, `https://127.0.0.1:${port}/api/health`)).status, 200);
  } finally {
    await server.stop();
    supervisor.close();
  }
});

test('addon: extra hostnames go into the certificate; an expiring one is renewed', async () => {
  const dirs = await addonDirs();
  await writeFile(join(dirs.data, 'options.json'), JSON.stringify({ ssl: true, folder: 'chordwright', hostnames: ['ha.fritz.box', '10.0.0.9'] }));
  const port = nextPort++;
  let server = await launch([join(ADDON, 'start.mjs')], { env: addonEnv(dirs, port), port, scheme: 'https' });
  const caPem = await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8');
  try {
    const x = new X509Certificate(await readFile(join(dirs.data, 'server-cert.pem')));
    assert.equal(x.checkHost('ha.fritz.box'), 'ha.fritz.box');
    assert.equal(x.checkIP('10.0.0.9'), '10.0.0.9');
  } finally {
    await server.stop();
  }

  // A server certificate with 15 days left is replaced — by the same CA.
  const { issueServerCert } = await import(pathToFileURL(join(ADDON, 'certs.mjs')).href);
  const old = issueServerCert(
    { cert: caPem, key: await readFile(join(dirs.data, 'ca-key.pem'), 'utf8') },
    { dnsNames: ['homeassistant.local'], ips: ['127.0.0.1'], now: new Date(Date.now() - 810 * 86_400_000) },
  );
  await writeFile(join(dirs.data, 'server-cert.pem'), old.cert);
  await writeFile(join(dirs.data, 'server-key.pem'), old.key);
  server = await launch([join(ADDON, 'start.mjs')], { env: addonEnv(dirs, port), port, scheme: 'https' });
  try {
    const now = new X509Certificate(await readFile(join(dirs.data, 'server-cert.pem')));
    assert.ok(Date.parse(now.validTo) - Date.now() > 800 * 86_400_000, 'renewed');
    assert.equal(await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8'), caPem);
    assert.equal((await getTrusting(caPem, `https://127.0.0.1:${port}/api/health`)).status, 200);
  } finally {
    await server.stop();
  }
});

test('addon: certificates in /ssl are used, token and folder from the options', async () => {
  const dirs = await addonDirs();
  const { cert, key } = selfSignedCert(dirs.ssl);
  await writeFile(join(dirs.data, 'options.json'), JSON.stringify({
    token: 'aus-den-optionen', ssl: true, certfile: 'cert.pem', keyfile: 'key.pem', folder: 'songs-band',
  }));
  const port = nextPort++;
  const server = await launch([join(ADDON, 'start.mjs')], { env: addonEnv(dirs, port), port, scheme: 'https' });
  try {
    const res = await get(`https://127.0.0.1:${port}/api/health`);
    assert.equal(JSON.parse(res.body).dir, join(dirs.share, 'songs-band'));
    assert.equal((await get(`https://127.0.0.1:${port}/api/user`, { authorization: 'Bearer aus-den-optionen' })).status, 200);
    assert.doesNotMatch(server.output(), /Chordwright-CA/);
    await assert.rejects(readFile(join(dirs.data, 'token')));
    await assert.rejects(readFile(join(dirs.data, 'ca-cert.pem')));
    // No CA of ours, so nothing to hand out.
    assert.equal((await get(`https://127.0.0.1:${port}/ca.crt`)).status, 404);
    assert.ok(readFileSync(cert) && readFileSync(key));
  } finally {
    await server.stop();
  }
});

test('addon: ssl off → plain http, still with a token', async () => {
  const dirs = await addonDirs();
  await writeFile(join(dirs.data, 'options.json'), JSON.stringify({ token: '', ssl: false, folder: 'chordwright' }));
  const port = nextPort++;
  const server = await launch([join(ADDON, 'start.mjs')], { env: addonEnv(dirs, port), port });
  try {
    assert.equal((await get(`http://127.0.0.1:${port}/api/user`)).status, 401);
    const token = (await readFile(join(dirs.data, 'token'), 'utf8')).trim();
    assert.equal((await get(`http://127.0.0.1:${port}/api/user`, { authorization: `Bearer ${token}` })).status, 200);
  } finally {
    await server.stop();
  }
});
