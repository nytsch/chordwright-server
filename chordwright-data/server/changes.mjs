/**
 * Who changed what, and when.
 *
 *   .chordwright/changes.json
 *     records   "<db>/<key>" → { at, by, action }   the last change to each record
 *     recent    [{ db, key, at, by, action }]         newest first, at most `keep`
 *
 * `by` is the name the app sends in `X-Chordwright-User` — set per server in the
 * app, not an account: there are no accounts here, a token opens the whole
 * library. Null when nobody said, and for a file saved by hand (`action:
 * 'file'`), which the watcher sees but cannot put a name to.
 *
 * The journal sits in a dot folder next to the library, so it is neither a
 * record nor part of a snapshot: restoring an old state does not rewrite who
 * did what since.
 *
 * Kept in memory and written behind — one pending write at a time, always of
 * the newest state — so the first app to connect, sending fifty songs at once,
 * costs fifty map updates and a couple of file writes, not fifty rewrites.
 */

import { mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { writeAtomic } from './store.mjs';

const DIR = '.chordwright';
const FILE = 'changes.json';
/**
 * Saving a song while typing sends a write every few seconds. In the list of
 * recent changes those are one change: the same record, the same person, the
 * same kind of change within this window only moves the time forward.
 */
const MERGE_WINDOW_MS = 5 * 60 * 1000;
const MAX_NAME = 60;

export const ACTIONS = ['write', 'remove', 'file', 'restore'];

/** What goes into `by`: trimmed, bounded, or null. */
export function cleanName(raw) {
  if (typeof raw !== 'string') return null;
  const name = raw.replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, MAX_NAME);
  return name || null;
}

export function createJournal(root, { keep = 500, now = () => new Date() } = {}) {
  const path = join(root, DIR, FILE);
  let state = null;
  let loading = null;

  function load() {
    loading ??= (async () => {
      try {
        const parsed = JSON.parse(await readFile(path, 'utf8'));
        state = {
          records: parsed && typeof parsed.records === 'object' && parsed.records ? parsed.records : {},
          recent: Array.isArray(parsed?.recent) ? parsed.recent : [],
        };
      } catch {
        state = { records: {}, recent: [] };
      }
    })();
    return loading;
  }

  let saving = Promise.resolve();
  let dirty = false;
  function save() {
    if (dirty) return saving;
    dirty = true;
    saving = saving.then(async () => {
      dirty = false;
      await mkdir(join(root, DIR), { recursive: true });
      await writeAtomic(path, JSON.stringify(state, null, 2) + '\n');
    }).catch((err) => console.error('Could not write the change journal:', err));
    return saving;
  }

  return {
    /**
     * Note a change. `listed: false` updates the record's last change without
     * an entry of its own in `recent` — a restore lists itself once, not once
     * per file.
     */
    async record({ db, key, by = null, action = 'write', listed = true }) {
      await load();
      const at = now().toISOString();
      const change = { at, by: cleanName(by), action };
      state.records[`${db}/${key}`] = change;
      if (listed) {
        const last = state.recent[0];
        const same =
          last &&
          last.db === db &&
          last.key === key &&
          last.by === change.by &&
          last.action === action &&
          Date.parse(at) - Date.parse(last.at) < MERGE_WINDOW_MS;
        if (same) last.at = at;
        else state.recent.unshift({ db, key, ...change });
        if (state.recent.length > keep) state.recent.length = keep;
      }
      save();
      return change;
    },

    /** The last change to one record, or null when none is on file. */
    async of(db, key) {
      await load();
      return state.records[`${db}/${key}`] ?? null;
    },

    /** `{ key: { at, by, action } }` for every record of `db` with a change on file. */
    async forDb(db) {
      await load();
      const prefix = `${db}/`;
      const out = {};
      for (const [id, change] of Object.entries(state.records)) {
        if (id.startsWith(prefix)) out[id.slice(prefix.length)] = change;
      }
      return out;
    },

    /** Newest first. */
    async recent(limit = 50) {
      await load();
      return state.recent.slice(0, Math.max(0, limit));
    },

    /** Resolves once everything recorded so far is on disk. */
    flush() {
      return saving;
    },
  };
}
