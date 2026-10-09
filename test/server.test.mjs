/**
 * Tests für den Datenserver und das Home-Assistant-Add-on — von außen, über
 * HTTP, gegen das Programm (`CHORDWRIGHT_SERVER_BIN`):
 *
 *   npm test          baut es mit cargo und lässt diese Tests dagegen laufen
 *
 * Was sich von innen besser prüfen lässt (Speicher, Sicherungen, Protokoll,
 * Bühne, Zertifikate), prüfen die Rust-Tests (`cargo test`). Bis 1.7.0 war der
 * Server in Node geschrieben; diese Tests sind dieselben geblieben und halten
 * fest, dass sich für die App nichts geändert hat. openssl wird für die
 * https-Fälle gebraucht.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile, mkdir } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { request as httpsRequest } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { X509Certificate } from 'node:crypto';


const tempDir = () => mkdtemp(join(tmpdir(), 'chordwright-test-'));
const doc = (id, text = `{title: ${id}}\n[C]la`, origin = 'seed') =>
  JSON.stringify({ v: 1, data: { id, text, origin, updatedAt: new Date().toISOString() } });

let nextPort = 20000 + Math.floor(Math.random() * 20000);

/** The program under test: `npm test` builds it and passes it in. */
const BIN = process.env.CHORDWRIGHT_SERVER_BIN;
if (!BIN) throw new Error('CHORDWRIGHT_SERVER_BIN is not set — run the tests with `npm test`.');

/** The server's command line; `addon` for the Home Assistant add-on's start. */
const SERVE = 'serve';
const ADDON_START = 'addon';
function command(args) {
  return args[0] === ADDON_START ? [BIN, ['--addon', ...args.slice(1)]] : [BIN, args.slice(1)];
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
// Der Server
// ---------------------------------------------------------------------------

test('serve: backups over HTTP — list, create, restore, delete, token enforced', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch(
    [SERVE, '--dir', dir, '--port', String(port), '--token', 't', '--backup-every', '0'],
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

test('serve: who changed what — header, reads, /api/changes, edits by hand, restore', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([SERVE, '--dir', dir, '--port', String(port), '--backup-every', '0'], { port });
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
  const server = await launch([SERVE, '--dir', dir, '--port', String(port)], { port });
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
  const server = await launch([SERVE, '--dir', dir, '--port', String(port)], { port });
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
  const server = await launch([SERVE, '--dir', dir, '--port', String(port)], { port });
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
    ...command([SERVE, '--dir', dir, '--port', '0', '--token', 't', '--ready-json', '--exit-with-stdin']),
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

test('serve: shared stage over HTTP — token, lead, publish, SSE as a named event', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([SERVE, '--dir', dir, '--port', String(port), '--token', 't'], { port });
  const base = `http://127.0.0.1:${port}/api`;
  const as = (client) => ({ authorization: 'Bearer t', 'content-type': 'application/json', 'x-chordwright-client': client });
  const controller = new AbortController();
  try {
    assert.equal((await (await fetch(`${base}/health`)).json()).stage, true);
    assert.equal((await fetch(`${base}/stage/band`)).status, 401);
    const time = await (await fetch(`${base}/time`, { headers: as('a') })).json();
    assert.ok(Math.abs(time.now - Date.now()) < 5_000);

    const named = [];
    const plain = [];
    const res = await fetch(`${base}/events?token=t`, { signal: controller.signal });
    (async () => {
      const decoder = new TextDecoder();
      let buffer = '';
      try {
        for await (const chunk of res.body) {
          buffer += decoder.decode(chunk);
          let end;
          while ((end = buffer.indexOf('\n\n')) >= 0) {
            const block = buffer.slice(0, end);
            buffer = buffer.slice(end + 2);
            const event = /^event: (.*)$/m.exec(block)?.[1];
            const data = /^data: (.*)$/m.exec(block)?.[1];
            if (data) (event === 'stage' ? named : plain).push(JSON.parse(data));
          }
        }
      } catch {
        /* aborted */
      }
    })();

    const lead = await fetch(`${base}/stage/Die%20Band/lead`, { method: 'POST', headers: as('a'), body: JSON.stringify({ name: 'Niko' }) });
    assert.equal(lead.status, 200);
    assert.equal((await lead.json()).leader.name, 'Niko');
    const other = await fetch(`${base}/stage/Die%20Band/lead`, { method: 'POST', headers: as('b'), body: '{}' });
    assert.equal(other.status, 409);

    const state = { setId: 's1', index: 2, songId: 'lied', playing: true, startAt: Date.now() };
    const put = await fetch(`${base}/stage/Die%20Band`, { method: 'PUT', headers: as('a'), body: JSON.stringify({ state }) });
    assert.equal(put.status, 200);
    assert.equal((await fetch(`${base}/stage/Die%20Band`, { method: 'PUT', headers: as('b'), body: JSON.stringify({ state }) })).status, 403);
    assert.equal((await fetch(`${base}/stage/Die%20Band`, { method: 'PUT', headers: as('a'), body: 'nope' })).status, 400);

    const read = await (await fetch(`${base}/stage/Die%20Band`, { headers: as('b') })).json();
    assert.deepEqual(read.state, state);
    assert.equal(read.room, 'Die Band');
    const rooms = await (await fetch(`${base}/stage`, { headers: as('b') })).json();
    assert.deepEqual(rooms.rooms.map((r) => r.room), ['Die Band']);

    for (let i = 0; i < 100 && !named.some((e) => e.state?.songId === 'lied'); i++) await new Promise((r) => setTimeout(r, 20));
    assert.ok(named.some((e) => e.room === 'Die Band' && e.state?.songId === 'lied'), JSON.stringify(named));
    // Nothing of it on the unnamed channel an older app reads.
    assert.equal(plain.length, 0);

    const here = await fetch(`${base}/stage/Die%20Band/here`, { method: 'POST', headers: as('b'), body: JSON.stringify({ name: 'Bass' }) });
    assert.equal(here.status, 200);
    assert.deepEqual((await here.json()).followers, [{ client: 'b', name: 'Bass', attached: true }]);
    for (let i = 0; i < 100 && !named.some((e) => e.followers?.length === 1); i++) await new Promise((r) => setTimeout(r, 20));
    assert.ok(named.some((e) => e.followers?.[0]?.name === 'Bass'), 'the leader hears who follows');
    assert.equal((await fetch(`${base}/stage/Die%20Band/here`, { method: 'DELETE', headers: as('b') })).status, 200);
    assert.deepEqual((await (await fetch(`${base}/stage/Die%20Band`, { headers: as('a') })).json()).followers, []);

    assert.equal((await fetch(`${base}/stage/Die%20Band/lead`, { method: 'DELETE', headers: as('a') })).status, 200);
    assert.equal((await (await fetch(`${base}/stage/Die%20Band`, { headers: as('b') })).json()).leader, null);
    assert.equal((await fetch(`${base}/stage/a%2Fb`, { headers: as('a') })).status, 400);
  } finally {
    controller.abort();
    await server.stop();
  }
});

test('serve: shared in the network — own CA, https outside, http on loopback for the starter', async () => {
  const dir = await tempDir();
  const caDir = join(dir, 'ca');
  const child = spawn(
    ...command([
      SERVE, '--dir', join(dir, 'data'), '--host', '0.0.0.0', '--port', '0', '--token', 't',
      '--own-ca', caDir, '--hostnames', 'buehne.fritz.box', '--loopback-port', '0', '--ready-json', '--exit-with-stdin',
    ]),
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

    // The starter's way in: plain http on loopback, token still required.
    assert.match(ready.url, /^http:\/\/127\.0\.0\.1:\d+$/);
    assert.equal((await get(`${ready.url}/api/library`)).status, 401);
    assert.equal((await get(`${ready.url}/api/library`, { authorization: 'Bearer t' })).status, 200);

    // Everyone else's: https on its own port, under this machine's names.
    assert.ok(ready.lan.port > 0 && ready.lan.port !== ready.port);
    assert.equal(ready.lan.ca, true);
    assert.ok(ready.lan.urls.every((u) => u.startsWith('https://') && u.endsWith(`:${ready.lan.port}`)), ready.lan.urls);
    assert.ok(ready.lan.urls.some((u) => u.includes('.local:')), ready.lan.urls);

    // A device that installed the CA trusts it — checked for real, not waved through.
    const ca = readFileSync(join(caDir, 'ca-cert.pem'));
    const trusted = await new Promise((resolve, reject) => {
      const req = httpsRequest(
        `https://127.0.0.1:${ready.lan.port}/api/health`,
        { ca, rejectUnauthorized: true },
        (res) => {
          res.resume();
          resolve(res.statusCode);
        },
      );
      req.on('error', reject);
      req.end();
    });
    assert.equal(trusted, 200);
    const san = new X509Certificate(readFileSync(join(caDir, 'server-cert.pem'))).subjectAltName;
    assert.match(san, /DNS:buehne\.fritz\.box/);
    assert.match(san, /\.local/);
    const crt = await get(`https://127.0.0.1:${ready.lan.port}/ca.crt`);
    assert.equal(crt.status, 200);
  } finally {
    child.stdin.end();
  }
  const code = await Promise.race([exited, new Promise((r) => setTimeout(() => r('still running'), 5_000))]);
  if (code === 'still running') child.kill();
  assert.equal(code, 0);
});

test('serve: the CA and the server certificate it issues are what browsers and openssl accept', async () => {
  const dir = await tempDir();
  const caDir = join(dir, 'ca');
  const child = spawn(
    ...command([SERVE, '--dir', join(dir, 'data'), '--host', '0.0.0.0', '--port', '0', '--token', 't', '--own-ca', caDir,
      '--hostnames', 'homeassistant.local,ha.fritz.box,192.168.68.123,fd00::5', '--ready-json', '--exit-with-stdin']),
    { stdio: ['pipe', 'pipe', 'pipe'] },
  );
  const exited = new Promise((r) => child.on('exit', r));
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('no ready line')), 10_000);
      child.stdout.on('data', () => (clearTimeout(timer), resolve()));
    });
  } finally {
    child.stdin.end();
  }
  await exited;
  const caPem = await readFile(join(caDir, 'ca-cert.pem'), 'utf8');
  const serverPem = await readFile(join(caDir, 'server-cert.pem'), 'utf8');
  const c = new X509Certificate(caPem);
  const x = new X509Certificate(serverPem);
  assert.equal(c.ca, true);
  assert.equal(x.ca, false);
  assert.ok(x.checkIssued(c) && x.verify(c.publicKey), 'issued and signed by the CA');
  assert.equal(x.checkHost('ha.fritz.box'), 'ha.fritz.box');
  assert.equal(x.checkIP('192.168.68.123'), '192.168.68.123');
  assert.equal(x.checkIP('fd00:0:0:0:0:0:0:5'), 'fd00:0:0:0:0:0:0:5');
  assert.deepEqual(x.keyUsage, ['1.3.6.1.5.5.7.3.1']);
  const days = (Date.parse(x.validTo) - Date.now()) / 86_400_000;
  assert.ok(days > 824 && days <= 825, `server validity ${days} days`);

  // openssl agrees: the chain verifies, the CA may sign and nothing else.
  execFileSync('openssl', ['verify', '-CAfile', join(caDir, 'ca-cert.pem'), join(caDir, 'server-cert.pem')]);
  const caText = execFileSync('openssl', ['x509', '-in', join(caDir, 'ca-cert.pem'), '-noout', '-text'], { encoding: 'utf8' });
  assert.match(caText, /CA:TRUE/);
  assert.match(caText, /Certificate Sign/);
  const text = execFileSync('openssl', ['x509', '-in', join(caDir, 'server-cert.pem'), '-noout', '-text'], { encoding: 'utf8' });
  assert.match(text, /CA:FALSE/);
  assert.match(text, /TLS Web Server Authentication/);
  assert.match(text, /IP Address:192\.168\.68\.123/);
  assert.match(text, /Authority Key Identifier/);
  const pubFromCert = execFileSync('openssl', ['x509', '-in', join(caDir, 'server-cert.pem'), '-noout', '-pubkey'], { encoding: 'utf8' });
  const pubFromKey = execFileSync('openssl', ['pkey', '-in', join(caDir, 'server-key.pem'), '-pubout'], { encoding: 'utf8' });
  assert.equal(pubFromCert, pubFromKey);
});

test('serve: preflight answers Private Network Access', async () => {
  const dir = await tempDir();
  const port = nextPort++;
  const server = await launch([SERVE, '--dir', dir, '--port', String(port)], { port });
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
    [SERVE, '--dir', join(dir, 'data'), '--port', String(port), '--token', 'geheim', '--cert', cert, '--key', key],
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
  const child = spawn(...command([SERVE, '--dir', dir, '--port', String(nextPort++), '--cert', 'x.pem']), {
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

  let server = await launch([ADDON_START], { env, port, scheme: 'https' });
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
  server = await launch([ADDON_START], { env, port, scheme: 'https' });
  try {
    assert.equal((await readFile(join(dirs.data, 'token'), 'utf8')).trim(), token);
    assert.equal(await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8'), caPem);
    assert.equal(await readFile(join(dirs.data, 'server-cert.pem'), 'utf8'), serverCert);
  } finally {
    await server.stop();
  }

  // The router hands out a new address: a new server certificate, the same CA.
  supervisor.state.addresses = { v4: ['192.168.1.50/24'], v6: [] };
  server = await launch([ADDON_START], { env, port, scheme: 'https' });
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
  let server = await launch([ADDON_START], { env: addonEnv(dirs, port), port, scheme: 'https' });
  const caPem = await readFile(join(dirs.data, 'ca-cert.pem'), 'utf8');
  try {
    const x = new X509Certificate(await readFile(join(dirs.data, 'server-cert.pem')));
    assert.equal(x.checkHost('ha.fritz.box'), 'ha.fritz.box');
    assert.equal(x.checkIP('10.0.0.9'), '10.0.0.9');
  } finally {
    await server.stop();
  }

  // A server certificate with 15 days left is replaced — by the same CA.
  const csr = execFileSync('openssl', ['req', '-new', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes',
    '-keyout', join(dirs.data, 'server-key.pem'), '-subj', '/CN=chordwright'], { encoding: 'utf8' });
  execFileSync('openssl', ['x509', '-req', '-days', '15', '-CA', join(dirs.data, 'ca-cert.pem'), '-CAkey', join(dirs.data, 'ca-key.pem'),
    '-out', join(dirs.data, 'server-cert.pem')], { input: csr });
  assert.ok(Date.parse(new X509Certificate(await readFile(join(dirs.data, 'server-cert.pem'))).validTo) - Date.now() < 16 * 86_400_000);
  server = await launch([ADDON_START], { env: addonEnv(dirs, port), port, scheme: 'https' });
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
  const server = await launch([ADDON_START], { env: addonEnv(dirs, port), port, scheme: 'https' });
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
  const server = await launch([ADDON_START], { env: addonEnv(dirs, port), port });
  try {
    assert.equal((await get(`http://127.0.0.1:${port}/api/user`)).status, 401);
    const token = (await readFile(join(dirs.data, 'token'), 'utf8')).trim();
    assert.equal((await get(`http://127.0.0.1:${port}/api/user`, { authorization: `Bearer ${token}` })).status, 200);
  } finally {
    await server.stop();
  }
});
