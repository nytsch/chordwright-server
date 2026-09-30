/**
 * Snapshots of the whole library — songs, sets, settings — as plain folders.
 *
 *   backups/
 *     20260928-101530/
 *       backup.json        when, why, how many songs and sets
 *       library/…          a copy of library/ at that moment
 *       user/…             a copy of user/
 *
 * A snapshot is a folder, not an archive: over Samba it opens like the live
 * one, a single song comes back by dragging one file, and nothing here needs a
 * dependency to write it.
 *
 * Three reasons a snapshot is taken:
 *   auto      on a schedule, and only when something changed since the last
 *             one — a quiet week costs no disk. The newest `keep` are kept.
 *   manual    asked for from the app. Kept until someone deletes it.
 *   restore   what was there just before a restore, so that a restore can
 *             itself be undone. Kept like a manual one.
 *
 * A restore writes file by file into the live folders instead of swapping them:
 * the watcher watches those directories, and a directory renamed away takes the
 * watch with it.
 */

import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { writeAtomic } from './store.mjs';

export const BACKUP_DIR = 'backups';
const TREES = ['library', 'user'];
const META = 'backup.json';
const ID = /^\d{8}-\d{6}(-\d+)?$/;
const REASONS = ['auto', 'manual', 'restore'];

export function isValidBackupId(id) {
  return typeof id === 'string' && ID.test(id);
}

/** Every file under `dir`, as paths relative to it. Temp files are not files. */
async function walk(dir, prefix = '') {
  let entries;
  try {
    entries = await readdir(join(dir, prefix), { withFileTypes: true });
  } catch {
    return [];
  }
  const files = [];
  for (const entry of entries) {
    if (entry.name.startsWith('.')) continue;
    const rel = prefix ? join(prefix, entry.name) : entry.name;
    if (entry.isDirectory()) files.push(...(await walk(dir, rel)));
    else if (entry.isFile()) files.push(rel);
  }
  return files.sort();
}

function stampOf(date) {
  const iso = date.toISOString(); // 2026-09-28T10:15:30.123Z
  return `${iso.slice(0, 10).replace(/-/g, '')}-${iso.slice(11, 19).replace(/:/g, '')}`;
}

export function createBackups(root, { keep = 14, now = () => new Date() } = {}) {
  const base = join(root, BACKUP_DIR);

  /** The live library as it is now: its files, a fingerprint over them, the counts. */
  async function survey(from) {
    const hash = createHash('sha256');
    const files = [];
    let bytes = 0;
    let songs = 0;
    let sets = 0;
    for (const tree of TREES) {
      for (const rel of await walk(join(from, tree))) {
        const path = join(tree, rel);
        const body = await readFile(join(from, path));
        files.push({ path, body });
        bytes += body.length;
        hash.update(path).update('\0').update(body).update('\0');
        if (tree === 'library' && path.endsWith('.chordpro')) songs++;
        if (path === join('user', 'setlists.json')) {
          try {
            const data = JSON.parse(body.toString('utf8'));
            const list = Array.isArray(data) ? data : data?.data;
            if (Array.isArray(list)) sets = list.length;
          } catch {
            /* not JSON — no count, the file is still copied */
          }
        }
      }
    }
    return { files, bytes, songs, sets, fingerprint: hash.digest('hex').slice(0, 16) };
  }

  async function readMeta(id) {
    try {
      const meta = JSON.parse(await readFile(join(base, id, META), 'utf8'));
      return {
        id,
        createdAt: String(meta.createdAt ?? ''),
        reason: REASONS.includes(meta.reason) ? meta.reason : 'manual',
        songs: Number(meta.songs) || 0,
        sets: Number(meta.sets) || 0,
        bytes: Number(meta.bytes) || 0,
        fingerprint: String(meta.fingerprint ?? ''),
      };
    } catch {
      return null; // half written, or not ours
    }
  }

  /** Newest first. */
  async function list() {
    let entries = [];
    try {
      entries = await readdir(base, { withFileTypes: true });
    } catch {
      return [];
    }
    const ids = entries.filter((e) => e.isDirectory() && isValidBackupId(e.name)).map((e) => e.name);
    const metas = await Promise.all(ids.map(readMeta));
    return metas.filter(Boolean).sort((a, b) => (a.createdAt < b.createdAt ? 1 : a.createdAt > b.createdAt ? -1 : 0));
  }

  /** Only auto snapshots age out; the others stay until someone deletes them. */
  async function prune() {
    const autos = (await list()).filter((b) => b.reason === 'auto');
    for (const old of autos.slice(Math.max(1, keep))) await rm(join(base, old.id), { recursive: true, force: true });
  }

  /**
   * One at a time: two snapshots in the same second would want the same name,
   * and a restore must not run while one is being copied.
   */
  let queue = Promise.resolve();
  function serial(task) {
    const run = queue.then(task);
    queue = run.catch(() => {});
    return run;
  }

  async function take(reason, current) {
    const at = now();
    let id = stampOf(at);
    for (let n = 2; await stat(join(base, id)).then(() => true, () => false); n++) id = `${stampOf(at)}-${n}`;
    const dir = join(base, id);
    // Written under a dot name and renamed at the end would be tidier, but a
    // folder without backup.json is already invisible to `list` — the meta file
    // goes last and is what makes it a snapshot.
    for (const file of current.files) {
      await mkdir(dirname(join(dir, file.path)), { recursive: true });
      await writeFile(join(dir, file.path), file.body);
    }
    const meta = {
      createdAt: at.toISOString(),
      reason,
      songs: current.songs,
      sets: current.sets,
      bytes: current.bytes,
      fingerprint: current.fingerprint,
    };
    await mkdir(dir, { recursive: true });
    await writeFile(join(dir, META), JSON.stringify(meta, null, 2) + '\n');
    return { id, ...meta };
  }

  return {
    list,

    /** A snapshot now, whatever changed or did not. */
    create(reason = 'manual') {
      return serial(async () => {
        const entry = await take(reason, await survey(root));
        await prune();
        return entry;
      });
    },

    /**
     * The scheduled one: only if the newest auto snapshot is at least `everyMs`
     * old, and only if the library differs from the newest snapshot of any
     * kind. An empty library is not worth keeping. Null when nothing was taken.
     */
    createIfDue(everyMs) {
      return serial(async () => {
        const all = await list();
        const lastAuto = all.find((b) => b.reason === 'auto');
        if (lastAuto && now().getTime() - Date.parse(lastAuto.createdAt) < everyMs) return null;
        const current = await survey(root);
        if (current.files.length === 0) return null;
        if (all[0]?.fingerprint === current.fingerprint) return null;
        const entry = await take('auto', current);
        await prune();
        return entry;
      });
    },

    /**
     * Put a snapshot back. What is there now is kept first as a `restore`
     * snapshot. Resolves to both, and the paths that changed, or null when
     * there is no such snapshot.
     */
    restore(id) {
      return serial(async () => {
        if (!isValidBackupId(id)) return null;
        const target = await readMeta(id);
        if (!target) return null;
        const safety = await take('restore', await survey(root));
        const wanted = new Set();
        const changed = [];
        for (const tree of TREES) {
          for (const rel of await walk(join(base, id, tree))) {
            const path = join(tree, rel);
            wanted.add(path);
            const body = await readFile(join(base, id, path));
            const live = join(root, path);
            const same = await readFile(live).then((old) => old.equals(body), () => false);
            if (same) continue;
            await mkdir(dirname(live), { recursive: true });
            await writeAtomic(live, body);
            changed.push(path);
          }
        }
        for (const tree of TREES) {
          for (const rel of await walk(join(root, tree))) {
            const path = join(tree, rel);
            if (wanted.has(path)) continue;
            await rm(join(root, path), { force: true });
            changed.push(path);
          }
        }
        return { restored: target, safety, changed };
      });
    },

    /** False when there was nothing by that id. */
    remove(id) {
      return serial(async () => {
        if (!isValidBackupId(id) || !(await readMeta(id))) return false;
        await rm(join(base, id), { recursive: true, force: true });
        return true;
      });
    },
  };
}
