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
 *   PUT    /api/:db/record/:key    → body is the raw value string
 *   DELETE /api/:db/record/:key
 *
 * PUT and DELETE take `If-Match: "<rev>"` (only if the record is still that
 * revision) or `If-None-Match: *` (only if it does not exist yet). If not,
 * nothing changes and the answer is 412 with `{ value, rev }` — what is there
 * now, so the client can put both versions in front of the user. Without
 * either header a write is unconditional, as it always was.
 *   GET    /api/events             → SSE: {"db","key","client"} on every change
 *   GET    /api/health             → { ok, databases, dir }
 *
 * `--cert <pem> --key <pem>` serves https instead of http; `--ca-file <pem>`
 * hands out the authority that signed it at GET /ca.crt.
 */

import { createServer } from 'node:http';
import { createServer as createSecureServer } from 'node:https';
import { readFileSync, watch } from 'node:fs';
import { join, resolve } from 'node:path';
import { createStore, DATABASES, isValidKey } from './store.mjs';

function parseArgs(argv) {
  const args = { dir: './data', port: 4174, host: '127.0.0.1', token: '', insecure: false, cert: '', key: '', 'ca-file': '' };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--insecure') args.insecure = true;
    else if (arg.startsWith('--')) args[arg.slice(2)] = argv[++i];
  }
  args.port = Number(args.port) || 4174;
  return args;
}

const args = parseArgs(process.argv.slice(2));
const root = resolve(args.dir);
const store = createStore(root);

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
        const client = recentSelfWrites.get(`${found.db}/${found.key}`) ?? null;
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
    'access-control-allow-headers': 'authorization, content-type, x-chordwright-client, if-match, if-none-match',
    'access-control-expose-headers': 'etag',
    'access-control-allow-methods': 'GET, PUT, DELETE, OPTIONS',
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
    return send(res, 200, { ok: true, databases: DATABASES, dir: root, watching: listeners.size, revisions: true });
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

  const db = parts[1];
  if (!DATABASES.includes(db)) return send(res, 404, { error: 'unknown database' });

  try {
    if (parts.length === 2 && req.method === 'GET') {
      if (url.searchParams.get('revs') === '1') return send(res, 200, await store.readAllVersioned(db));
      return send(res, 200, await store.readAll(db));
    }

    if (parts[2] !== 'record' || parts.length !== 4) return send(res, 404, { error: 'not found' });
    const key = decodeURIComponent(parts[3]);
    if (!isValidKey(key)) return send(res, 400, { error: 'bad key' });

    const client = req.headers['x-chordwright-client'] ?? null;

    if (req.method === 'GET') {
      const { value, rev } = await store.readVersioned(db, key);
      // `rev: null` on a miss too: it tells a client this server keeps
      // revisions even before it has read a single record that exists.
      return value === null
        ? send(res, 404, { error: 'no such record', rev: null })
        : send(res, 200, { value, rev }, { etag: `"${rev}"` });
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
      broadcast(db, key, client);
      return send(res, 204, undefined, result.rev ? { etag: `"${result.rev}"` } : {});
    }

    return send(res, 405, { error: 'method not allowed' });
  } catch (err) {
    console.error(err);
    return send(res, 500, { error: String(err.message ?? err) });
  }
}

const server = tls ? createSecureServer(tls, handle) : createServer(handle);

await store.ensureLayout();
watchFiles();

server.listen(args.port, args.host, () => {
  console.log(`chordwright serve`);
  console.log(`  data   ${root}`);
  // Without /api: this is what goes into the app, which adds the paths itself.
  console.log(`  url    ${tls ? 'https' : 'http'}://${args.host}:${args.port}`);
  console.log(`  auth   ${args.token ? 'token required' : 'none (loopback only)'}`);
});
