/**
 * The folder layout, and the mapping between a storage key and a file on disk.
 *
 * This is the whole point of the server: `DocumentStore` speaks in keys and
 * opaque strings, and a plain key-value dump would have written the ChordPro
 * text JSON-escaped inside a `.json` file — technically a file, useless to a
 * human. So one key shape is special-cased:
 *
 *   library / index          → library/index.json      (the record, pretty-printed)
 *   library / doc.<id>       → library/songs/<id>.chordpro   (the text itself)
 *   user    / <key>          → user/<key>.json
 *
 * A `.chordpro` file is the real thing: openable in any editor, greppable,
 * diffable in git. The two fields that are not in the file — where the document
 * came from, and which envelope version wrote it — live in a small sidecar so
 * that a round trip through the server changes nothing. A file someone drops
 * into `songs/` by hand has no sidecar entry and is reported as an import,
 * which is exactly what it is.
 */

import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';

/** Keys are file names. Anything that could climb out of the folder is refused. */
const SAFE_KEY = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export const DATABASES = ['library', 'user'];

const DOCUMENT_PREFIX = 'doc.';
const SONGS_DIR = 'songs';
const SIDECAR = '_documents.json';
/** Matches the client's ENVELOPE_VERSION; only used for files that lack a sidecar. */
const DEFAULT_ENVELOPE_VERSION = 1;

/**
 * Write through a temporary file and rename it into place. A reader — the app,
 * the watcher, the next write's read-modify-write — sees the old file or the
 * new one, never half of one. The temp name starts with a dot and ends in
 * `.tmp`, so neither `readAll` nor `keyForPath` mistakes it for a record.
 */
let tmpCounter = 0;
async function writeAtomic(path, body) {
  const tmp = join(dirname(path), `.${basename(path)}.${process.pid}.${++tmpCounter}.tmp`);
  try {
    await writeFile(tmp, body);
    await rename(tmp, path);
  } catch (err) {
    await rm(tmp, { force: true });
    throw err;
  }
}

/**
 * A record's revision: a hash of the bytes on disk. Not a timestamp — clocks
 * differ between devices, and a hash changes by itself when someone edits the
 * file over Samba or in vim, with nobody having to remember to bump it. For a
 * song it is taken over the ChordPro text alone, so it names exactly the thing
 * two people can overwrite each other in.
 */
export function revisionOf(bytes) {
  return createHash('sha256').update(bytes).digest('hex').slice(0, 16);
}

export function isValidKey(key) {
  return typeof key === 'string' && SAFE_KEY.test(key) && !key.includes('..');
}

export function createStore(root) {
  const sidecarPath = join(root, 'library', SONGS_DIR, SIDECAR);

  async function readSidecar() {
    try {
      return JSON.parse(await readFile(sidecarPath, 'utf8'));
    } catch {
      return {};
    }
  }

  async function writeSidecar(next) {
    await mkdir(dirname(sidecarPath), { recursive: true });
    await writeAtomic(sidecarPath, JSON.stringify(next, null, 2) + '\n');
  }

  /**
   * The sidecar is one file shared by every song, so changing it is a
   * read-modify-write — and two of those interleaved lose one of the changes.
   * The first app to connect seeds the whole library at once, which is exactly
   * that: without this, fifty songs arrive and one entry survives. Every
   * change goes through this chain, one after the other.
   */
  let sidecarQueue = Promise.resolve();
  function updateSidecar(change) {
    const run = sidecarQueue.then(async () => {
      const meta = await readSidecar();
      if (change(meta) === false) return;
      await writeSidecar(meta);
    });
    // A failed update must not jam every later one.
    sidecarQueue = run.catch(() => {});
    return run;
  }

  /** Where a key lives, and whether it is a ChordPro file rather than a record. */
  function locate(db, key) {
    if (db === 'library' && key.startsWith(DOCUMENT_PREFIX)) {
      const id = key.slice(DOCUMENT_PREFIX.length);
      return { text: true, id, path: join(root, 'library', SONGS_DIR, `${id}.chordpro`) };
    }
    return { text: false, path: join(root, db, `${key}.json`) };
  }

  /** Rebuild the envelope the client wrote, from the file plus its sidecar entry. */
  async function readDocument(id, path) {
    const [text, info, meta] = await Promise.all([
      readFile(path, 'utf8'),
      stat(path),
      readSidecar(),
    ]);
    const side = meta[id] ?? {};
    const value = JSON.stringify({
      v: side.v ?? DEFAULT_ENVELOPE_VERSION,
      data: {
        id,
        text,
        origin: side.origin ?? 'imported',
        // The file's own timestamp, so editing it in vim really does count as
        // a newer version than what the app last wrote.
        updatedAt: info.mtime.toISOString(),
      },
    });
    return { value, rev: revisionOf(text) };
  }

  /** `{ value, rev }`, or both null when the record is not there. */
  async function readVersioned(db, key) {
    const at = locate(db, key);
    try {
      if (at.text) return await readDocument(at.id, at.path);
      const value = await readFile(at.path, 'utf8');
      return { value, rev: revisionOf(value) };
    } catch {
      return { value: null, rev: null };
    }
  }

  /** What a write of `value` would put on disk — the bytes its revision is taken over. */
  function bodyFor(at, value) {
    if (at.text) {
      const envelope = JSON.parse(value);
      const doc = envelope.data ?? envelope;
      return { body: doc.text ?? '', doc, envelope };
    }
    // Pretty-print anything that parses, so the file is readable. The client
    // parses it back, so the whitespace costs nothing.
    try {
      return { body: JSON.stringify(JSON.parse(value), null, 2) + '\n' };
    } catch {
      return { body: value }; // not JSON — store it as it came
    }
  }

  /**
   * One change per record at a time. A conditional write is a compare and a
   * write, and two of those interleaved would both see the old revision and
   * both succeed — exactly the lost edit the revision is there to catch.
   */
  const recordLocks = new Map();
  function withRecordLock(db, key, task) {
    const lockKey = `${db}/${key}`;
    const run = (recordLocks.get(lockKey) ?? Promise.resolve()).then(task);
    const settled = run.catch(() => {});
    recordLocks.set(lockKey, settled);
    settled.then(() => {
      if (recordLocks.get(lockKey) === settled) recordLocks.delete(lockKey);
    });
    return run;
  }

  async function readAllVersioned(db) {
    const records = {};
    const revs = {};
    const base = join(root, db);
    let entries = [];
    try {
      entries = await readdir(base, { withFileTypes: true });
    } catch {
      return { records, revs }; // the folder does not exist yet — an empty database
    }
    for (const entry of entries) {
      if (entry.isFile() && entry.name.endsWith('.json')) {
        const key = entry.name.slice(0, -'.json'.length);
        records[key] = await readFile(join(base, entry.name), 'utf8');
        revs[key] = revisionOf(records[key]);
      }
    }
    if (db !== 'library') return { records, revs };

    let songs = [];
    try {
      songs = await readdir(join(base, SONGS_DIR), { withFileTypes: true });
    } catch {
      return { records, revs };
    }
    for (const entry of songs) {
      if (!entry.isFile() || !entry.name.endsWith('.chordpro')) continue;
      const key = DOCUMENT_PREFIX + entry.name.slice(0, -'.chordpro'.length);
      const read = await readDocument(key.slice(DOCUMENT_PREFIX.length), join(base, SONGS_DIR, entry.name));
      records[key] = read.value;
      revs[key] = read.rev;
    }
    return { records, revs };
  }

  return {
    async read(db, key) {
      return (await readVersioned(db, key)).value;
    },

    readVersioned,

    async readAll(db) {
      return (await readAllVersioned(db)).records;
    },

    readAllVersioned,

    /**
     * Write a record. With `expect` (a revision, or null for "must not exist
     * yet") the write only happens if the record is still what the writer
     * last saw; otherwise nothing is written and the current state comes back
     * as `{ ok: false, value, rev }`. A write that would put exactly the bytes
     * already there is no conflict, whatever the writer last saw.
     */
    async write(db, key, value, { expect } = {}) {
      const at = locate(db, key);
      const { body, doc, envelope } = bodyFor(at, value);
      return withRecordLock(db, key, async () => {
        if (expect !== undefined) {
          const current = await readVersioned(db, key);
          if (current.rev !== expect && current.rev !== revisionOf(body)) {
            return { ok: false, value: current.value, rev: current.rev };
          }
        }
        await mkdir(dirname(at.path), { recursive: true });
        await writeAtomic(at.path, body);
        if (at.text) {
          await updateSidecar((meta) => {
            meta[at.id] = { origin: doc.origin ?? 'imported', v: envelope.v ?? DEFAULT_ENVELOPE_VERSION };
          });
        }
        return { ok: true, rev: revisionOf(body) };
      });
    },

    /** Remove a record; `expect` as for `write`. Removing what is already gone succeeds. */
    async remove(db, key, { expect } = {}) {
      const at = locate(db, key);
      return withRecordLock(db, key, async () => {
        if (expect !== undefined) {
          const current = await readVersioned(db, key);
          if (current.rev !== null && current.rev !== expect) {
            return { ok: false, value: current.value, rev: current.rev };
          }
        }
        await rm(at.path, { force: true });
        if (at.text) {
          await updateSidecar((meta) => {
            if (!(at.id in meta)) return false;
            delete meta[at.id];
          });
        }
        return { ok: true, rev: null };
      });
    },

    /** Map a changed file back to the key it represents. Null for anything else. */
    keyForPath(relativePath) {
      const parts = relativePath.split(/[/\\]/).filter(Boolean);
      const [db, ...rest] = parts;
      if (!DATABASES.includes(db) || rest.length === 0) return null;
      if (db === 'library' && rest[0] === SONGS_DIR) {
        const name = rest[1];
        if (!name || name === SIDECAR || !name.endsWith('.chordpro')) return null;
        return { db, key: DOCUMENT_PREFIX + name.slice(0, -'.chordpro'.length) };
      }
      if (rest.length !== 1 || !rest[0].endsWith('.json')) return null;
      return { db, key: rest[0].slice(0, -'.json'.length) };
    },

    async ensureLayout() {
      await mkdir(join(root, 'library', SONGS_DIR), { recursive: true });
      await mkdir(join(root, 'user'), { recursive: true });
    },
  };
}
