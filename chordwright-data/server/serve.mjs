#!/usr/bin/env node
/**
 * chordwright serve — a `DocumentStore` over a folder of real files.
 *
 * Why a server at all, when the app already persists fine: a browser tab cannot
 * listen on a port, and localStorage is not a place you can open in an editor.
 * Turning it around — the machine serves, the app connects — gives you songs as
 * `.chordpro` files you can edit, grep, diff and version, while the app keeps
 * working exactly as it did. Nothing above `adapters/` knows the difference.
 *
 *   node server/serve.mjs --dir ./data --port 4174
 *
 * Deliberately dependency-free: node:http, node:fs, nothing else. A spike that
 * needs an install to try is a spike nobody tries.
 *
 * Routes (all under /api):
 *   GET    /api/:db                → { key: value } for the whole database
 *   GET    /api/:db?revs=1         → { records: { key: value }, revs: { key: rev } }
 *   GET    /api/:db/record/:key    → { value, rev } | 404
 *   GET    /api/:db/record/:key?quiet=1 → a miss as 200 { value: null, rev: null, missing: true }
 *   PUT    /api/:db/record/:key    → body is the raw value string
 *   DELETE /api/:db/record/:key
 *
 * PUT and DELETE take `If-Match: "<rev>"` (only if the record is still that
 * revision) or `If-None-Match: *` (only if it does not exist yet). If not,
 * nothing changes and the answer is 412 with `{ value, rev }` — what is there
 * now, so the client can put both versions in front of the user. Without
 * either header a write is unconditional, as it always was.
 *   GET    /api/events             → SSE: {"db","key","client"} on every change
 *   GET    /api/health             → { ok, databases, dir, revisions, backups, changes, stage }
 *   GET    /api/changes?limit=50   → { changes: [{ db, key, at, by, action }] }, newest first
 *   GET    /api/changes?db=&key=   → { change: { at, by, action } | null }
 *   GET    /api/time               → { now }  — the server's clock, for the stage
 *   GET    /api/stage              → { now, rooms: [{ room, leader, rev, touched }] }
 *   GET    /api/stage/:room        → { room, leader, state, rev, now }
 *   POST   /api/stage/:room/lead   → take or renew the lead; body { name, force }; 409 if led
 *   DELETE /api/stage/:room/lead   → let go of it
 *   PUT    /api/stage/:room        → the leader's state; body { state }; 403/409 if not leader
 *   POST   /api/stage/:room/here   → a follower is there; body { name, attached }
 *   DELETE /api/stage/:room/here   → and leaves
 * Stage changes also go out on /api/events as `event: stage` (stage.mjs).
 *   GET    /api/backups            → { backups: [...], everyHours, keep }
 *   POST   /api/backups            → a snapshot now → 201 { id, createdAt, … }
 *   POST   /api/backups/:id/restore → { restored, safety }
 *   DELETE /api/backups/:id
 *
 * `--cert <pem> --key <pem>` serves https instead of http; `--ca-file <pem>`
 * hands out the authority that signed it at GET /ca.crt.
 *
 * Writes may carry `X-Chordwright-User: <name, URI-encoded>`; reads of a
 * record and `?revs=1` then say who changed it last and when (changes.mjs).
 *
 * `--backup-every <hours>` (default 24, 0 = never) takes a snapshot on that
 * schedule when something changed; `--backup-keep <n>` (default 14) is how many
 * of those stay. See backups.mjs.
 */

import { createServer } from 'node:http';
import { createServer as createSecureServer } from 'node:https';
import { readFileSync, watch } from 'node:fs';
import { join, resolve } from 'node:path';
import { createStore, DATABASES, isValidKey } from './store.mjs';
import { createBackups } from './backups.mjs';
import { createJournal } from './changes.mjs';
import { createStage, cleanRoom } from './stage.mjs';

function parseArgs(argv) {
  const args = {
    dir: './data',
    port: 4174,
    host: '127.0.0.1',
    token: '',
    insecure: false,
    cert: '',
    key: '',
    'ca-file': '',
    'backup-every': 24,
    'backup-keep': 14,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--insecure') args.insecure = true;
    else if (arg.startsWith('--')) args[arg.slice(2)] = argv[++i];
  }
  args.port = Number(args.port) || 4174;
  const every = Number(args['backup-every']);
  args['backup-every'] = Number.isFinite(every) && every >= 0 ? every : 24;
  args['backup-keep'] = Math.max(1, Math.floor(Number(args['backup-keep'])) || 14);
  return args;
}

const args = parseArgs(process.argv.slice(2));
const root = resolve(args.dir);
const store = createStore(root);
const backups = createBackups(root, { keep: args['backup-keep'] });
const journal = createJournal(root);

/**
 * The name the app sends with its writes. Encoded by the client, because a
 * header only carries Latin-1 and people are called Jürgen.
 */
function userOf(req) {
  const raw = req.headers['x-chordwright-user'];
  if (typeof raw !== 'string' || !raw) return null;
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

// A restore writes the files itself; the watcher would book each one as an
// edit by hand. Until then, it is the restore's.
let restoringUntil = 0;

// Binding to anything but loopback puts every song in the venue's wifi within
// reach of anyone on it. Refuse rather than warn: a warning scrolls past.
const exposed = args.host !== '127.0.0.1' && args.host !== 'localhost';
if (exposed && !args.token && !args.insecure) {
  console.error(
    `Refusing to listen on ${args.host} without --token.\n` +
      'Anyone on the network could read and rewrite the library.\n' +
      'Pass --token <secret>, or --insecure if you really mean it.',
  );
  process.exit(1);
}

// An app served over https — GitHub Pages, say — may not talk to an http://
// server anywhere but loopback; the browser blocks it as mixed content. So the
// server can speak https itself. Both files or neither.
if (Boolean(args.cert) !== Boolean(args.key)) {
  console.error('--cert and --key go together: pass both, or neither.');
  process.exit(1);
}
const tls = args.cert ? { cert: readFileSync(args.cert), key: readFileSync(args.key) } : null;

// The certificate authority that signed `--cert`, handed out at /ca.crt so a
// device can install it once and trust this server from then on — the only
// way an app on an iPhone home screen ever will. DER, with the content type
// iOS answers with "install profile". Public, like /api/health: a CA
// certificate is not a secret, and a device that does not trust the server
// yet has no way to send a token over it anyway.
const caDer = args['ca-file']
  ? Buffer.from(readFileSync(args['ca-file'], 'utf8').replace(/-----[^-]+-----|\s+/g, ''), 'base64')
  : null;

// ---------------------------------------------------------------------------
// Change notification. Two sources: writes through this server, and edits made
// to the files directly. The second one is the interesting half — it is what
// makes "save in vim, watch the app update" work.
// ---------------------------------------------------------------------------

const listeners = new Set();
const recentSelfWrites = new Map();

function broadcast(db, key, client = null) {
  const line = `data: ${JSON.stringify({ db, key, client })}\n\n`;
  for (const res of listeners) res.write(line);
}

// A named event: an app that only listens with `onmessage` never sees it, so
// older apps on the same server are not confused by a line they cannot read.
function broadcastStage(snapshot) {
  const line = `event: stage\ndata: ${JSON.stringify(snapshot)}\n\n`;
  for (const res of listeners) res.write(line);
}

const stage = createStage({ onChange: broadcastStage });

function watchFiles() {
  // Not `{ recursive: true }`: on Linux Node implements that by watching each
  // file, and a file replaced by a rename — an atomic save, which this server
  // does and so do most editors — silently drops out of it; the next edit to
  // that song never arrives. A directory watch sees every entry in it however
  // it was written, and the layout is fixed, so three directories are the
  // whole tree.
  for (const sub of ['library', join('library', 'songs'), 'user']) {
    try {
      watch(join(root, sub), (_event, filename) => {
        if (!filename) return;
        const found = store.keyForPath(join(sub, filename.toString()));
        if (!found) return;
        // A write we just made comes back through the watcher too. The client id
        // rides along so the tab that wrote is not told about its own change.
        const stamp = `${found.db}/${found.key}`;
        const client = recentSelfWrites.get(stamp) ?? null;
        // Not ours and not a restore: someone saved the file by hand.
        if (!recentSelfWrites.has(stamp) && Date.now() > restoringUntil) {
          void journal.record({ db: found.db, key: found.key, action: 'file' });
        }
        broadcast(found.db, found.key, client);
      });
    } catch (err) {
      console.warn(`File watching unavailable for ${sub} (${err.code ?? err.message}); live reload is off there.`);
    }
  }
}

// ---------------------------------------------------------------------------

function send(res, status, body, headers = {}) {
  const payload = body === undefined ? '' : JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'access-control-allow-origin': '*',
    'access-control-allow-headers': 'authorization, content-type, x-chordwright-client, x-chordwright-user, if-match, if-none-match',
    'access-control-expose-headers': 'etag',
    'access-control-allow-methods': 'GET, PUT, POST, DELETE, OPTIONS',
    'cache-control': 'no-store',
    ...headers,
  });
  res.end(payload);
}

function authorised(req) {
  if (!args.token) return true;
  const url = new URL(req.url, 'http://localhost');
  const header = req.headers.authorization ?? '';
  return header === `Bearer ${args.token}` || url.searchParams.get('token') === args.token;
}

/** `If-Match: "abc"` → 'abc', `If-None-Match: *` → null, neither → undefined. */
function expectedRevision(req) {
  const match = req.headers['if-match'];
  if (typeof match === 'string' && match.trim()) return match.trim().replace(/^W\//, '').replace(/^"|"$/g, '');
  if (req.headers['if-none-match']?.trim() === '*') return null;
  return undefined;
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}

async function handle(req, res) {
  // Chrome asks before a public page may reach a private address (Private
  // Network Access); this header is the server's yes. Harmless everywhere else.
  if (req.method === 'OPTIONS') return send(res, 204, undefined, { 'access-control-allow-private-network': 'true' });

  const url = new URL(req.url, 'http://localhost');
  const parts = url.pathname.split('/').filter(Boolean);
  if (caDer && req.method === 'GET' && url.pathname === '/ca.crt') {
    res.writeHead(200, {
      'content-type': 'application/x-x509-ca-cert',
      'content-disposition': 'attachment; filename="chordwright-ca.crt"',
      'cache-control': 'no-store',
    });
    return res.end(caDer);
  }
  if (parts[0] !== 'api') return send(res, 404, { error: 'not found' });

  if (parts[1] === 'health') {
    return send(res, 200, {
      ok: true,
      databases: DATABASES,
      dir: root,
      watching: listeners.size,
      revisions: true,
      backups: true,
      changes: true,
      stage: true,
    });
  }
  if (!authorised(req)) return send(res, 401, { error: 'unauthorised' });

  if (parts[1] === 'events') {
    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-store',
      connection: 'keep-alive',
      'access-control-allow-origin': '*',
    });
    res.write(': connected\n\n');
    listeners.add(res);
    // Proxies and phones drop an idle stream; a comment every 25s keeps it up.
    const beat = setInterval(() => res.write(': ping\n\n'), 25_000);
    req.on('close', () => {
      clearInterval(beat);
      listeners.delete(res);
    });
    return undefined;
  }

  if (parts[1] === 'time' && parts.length === 2 && req.method === 'GET') return send(res, 200, { now: Date.now() });
  if (parts[1] === 'stage') return handleStage(req, res, parts.slice(2));
  if (parts[1] === 'backups') return handleBackups(req, res, parts.slice(2));
  if (parts[1] === 'changes' && parts.length === 2 && req.method === 'GET') {
    const db = url.searchParams.get('db');
    const key = url.searchParams.get('key');
    if (db !== null || key !== null) {
      if (!DATABASES.includes(db) || !isValidKey(key)) return send(res, 400, { error: 'bad db or key' });
      return send(res, 200, { change: await journal.of(db, key) });
    }
    const limit = Math.min(500, Math.max(1, Number(url.searchParams.get('limit')) || 50));
    return send(res, 200, { changes: await journal.recent(limit) });
  }

  const db = parts[1];
  if (!DATABASES.includes(db)) return send(res, 404, { error: 'unknown database' });

  try {
    if (parts.length === 2 && req.method === 'GET') {
      if (url.searchParams.get('revs') === '1') {
        const [versioned, changes] = await Promise.all([store.readAllVersioned(db), journal.forDb(db)]);
        return send(res, 200, { ...versioned, changes });
      }
      return send(res, 200, await store.readAll(db));
    }

    if (parts[2] !== 'record' || parts.length !== 4) return send(res, 404, { error: 'not found' });
    const key = decodeURIComponent(parts[3]);
    if (!isValidKey(key)) return send(res, 400, { error: 'bad key' });

    const client = req.headers['x-chordwright-client'] ?? null;

    if (req.method === 'GET') {
      const [{ value, rev }, change] = await Promise.all([store.readVersioned(db, key), journal.of(db, key)]);
      // `rev: null` on a miss too: it tells a client this server keeps
      // revisions even before it has read a single record that exists. The
      // last change goes along either way: a removed record was removed by
      // someone.
      if (value !== null) return send(res, 200, { value, rev, change }, { etag: `"${rev}"` });
      // A miss is an answer, not an error — but a browser logs every 404 in
      // red, and an app asks for a dozen records nobody has set yet on each
      // start. With `?quiet=1` the miss comes back as 200 and `missing: true`;
      // without it, as the 404 an older app expects.
      if (url.searchParams.has('quiet')) return send(res, 200, { value: null, rev: null, change, missing: true });
      return send(res, 404, { error: 'no such record', rev: null, change });
    }

    if (req.method === 'PUT' || req.method === 'DELETE') {
      const stamp = `${db}/${key}`;
      recentSelfWrites.set(stamp, client);
      // Long enough for the watcher to fire, short enough that a later edit by
      // hand is not mistaken for this client's own write.
      setTimeout(() => recentSelfWrites.delete(stamp), 1_000).unref?.();

      const expect = expectedRevision(req);
      const result =
        req.method === 'PUT'
          ? await store.write(db, key, await readBody(req), { expect })
          : await store.remove(db, key, { expect });
      if (!result.ok) {
        recentSelfWrites.delete(stamp);
        return send(res, 412, { value: result.value, rev: result.rev });
      }
      void journal.record({ db, key, by: userOf(req), action: req.method === 'PUT' ? 'write' : 'remove' });
      broadcast(db, key, client);
      return send(res, 204, undefined, result.rev ? { etag: `"${result.rev}"` } : {});
    }

    return send(res, 405, { error: 'method not allowed' });
  } catch (err) {
    console.error(err);
    return send(res, 500, { error: String(err.message ?? err) });
  }
}

async function handleStage(req, res, rest) {
  if (rest.length === 0) {
    return req.method === 'GET'
      ? send(res, 200, { now: Date.now(), rooms: stage.list() })
      : send(res, 405, { error: 'method not allowed' });
  }
  let room;
  try {
    room = cleanRoom(decodeURIComponent(rest[0]));
  } catch {
    room = null;
  }
  if (!room) return send(res, 400, { error: 'bad room' });
  const client = req.headers['x-chordwright-client'] ?? null;
  const answer = ({ ok, status, ...body }) => send(res, ok ? 200 : status, body);

  let body = {};
  if (req.method === 'PUT' || req.method === 'POST') {
    const raw = await readBody(req);
    try {
      body = raw ? JSON.parse(raw) : {};
    } catch {
      return send(res, 400, { error: 'body is not JSON' });
    }
    if (body === null || typeof body !== 'object') return send(res, 400, { error: 'body must be an object' });
  }

  if (rest.length === 1) {
    if (req.method === 'GET') return send(res, 200, stage.read(room));
    if (req.method === 'PUT') return answer(stage.publish(room, { client, state: body.state }));
    return send(res, 405, { error: 'method not allowed' });
  }
  if (rest.length === 2 && rest[1] === 'lead') {
    if (req.method === 'POST') {
      return answer(stage.lead(room, { client, name: body.name ?? userOf(req), force: body.force === true }));
    }
    if (req.method === 'DELETE') return answer(stage.release(room, { client }));
    return send(res, 405, { error: 'method not allowed' });
  }
  if (rest.length === 2 && rest[1] === 'here') {
    if (req.method === 'POST') {
      return answer(stage.here(room, { client, name: body.name ?? userOf(req), attached: body.attached !== false }));
    }
    if (req.method === 'DELETE') return answer(stage.leave(room, { client }));
    return send(res, 405, { error: 'method not allowed' });
  }
  return send(res, 404, { error: 'not found' });
}

async function handleBackups(req, res, rest) {
  try {
    if (rest.length === 0 && req.method === 'GET') {
      const all = await backups.list();
      return send(res, 200, { backups: all, everyHours: args['backup-every'], keep: args['backup-keep'] });
    }
    if (rest.length === 0 && req.method === 'POST') {
      const entry = await backups.create('manual');
      console.log(`Backup ${entry.id} (manual): ${entry.songs} songs, ${entry.sets} sets`);
      return send(res, 201, entry);
    }
    if (rest.length === 2 && rest[1] === 'restore' && req.method === 'POST') {
      restoringUntil = Infinity;
      let result;
      try {
        result = await backups.restore(decodeURIComponent(rest[0]));
      } finally {
        // The watcher reports a little after the write; give it that long.
        restoringUntil = Date.now() + 2_000;
      }
      if (!result) return send(res, 404, { error: 'no such backup' });
      const by = userOf(req);
      for (const path of result.changed) {
        const found = store.keyForPath(path);
        if (found) void journal.record({ ...found, by, action: 'restore', listed: false });
      }
      void journal.record({ db: '*', key: result.restored.id, by, action: 'restore' });
      console.log(`Restored backup ${result.restored.id}; what was there before is backup ${result.safety.id}`);
      // The watcher reports every file on its own; this tells each app that
      // the whole database moved, so it reads it again in one go.
      for (const db of DATABASES) broadcast(db, null, req.headers['x-chordwright-client'] ?? null);
      return send(res, 200, result);
    }
    if (rest.length === 1 && req.method === 'DELETE') {
      return (await backups.remove(decodeURIComponent(rest[0])))
        ? send(res, 204)
        : send(res, 404, { error: 'no such backup' });
    }
    return send(res, rest.length > 2 ? 404 : 405, { error: rest.length > 2 ? 'not found' : 'method not allowed' });
  } catch (err) {
    console.error(err);
    return send(res, 500, { error: String(err.message ?? err) });
  }
}

/**
 * The schedule. Checked every hour rather than slept for `every` hours: a
 * server that restarts daily would otherwise never reach the end of a day.
 */
function scheduleBackups() {
  const every = args['backup-every'];
  if (!every) return;
  const check = () =>
    backups
      .createIfDue(every * 60 * 60 * 1000)
      .then((entry) => entry && console.log(`Backup ${entry.id} (auto): ${entry.songs} songs, ${entry.sets} sets`))
      .catch((err) => console.error('Backup failed:', err));
  void check();
  setInterval(check, 60 * 60 * 1000).unref?.();
}

const server = tls ? createSecureServer(tls, handle) : createServer(handle);

await store.ensureLayout();
watchFiles();
// Runs out leases of leaders that went quiet, so their followers hear of it.
setInterval(() => stage.sweep(), 2_000).unref?.();
scheduleBackups();

server.listen(args.port, args.host, () => {
  console.log(`chordwright serve`);
  console.log(`  data   ${root}`);
  // Without /api: this is what goes into the app, which adds the paths itself.
  console.log(`  url    ${tls ? 'https' : 'http'}://${args.host}:${args.port}`);
  console.log(`  auth   ${args.token ? 'token required' : 'none (loopback only)'}`);
  console.log(
    `  backup ${args['backup-every'] ? `every ${args['backup-every']} h when changed, newest ${args['backup-keep']} kept` : 'on request only'}`,
  );
});
