/**
 * Shared stage — one device leads, the others follow.
 *
 * A room is a band on stage: one leader, any number of followers. The leader
 * says where the stage is (set, song, whether and since when the clock runs);
 * the followers read it and go along. Several rooms can be open on one server
 * at the same time — two bands rehearsing in two rooms of the same house share
 * a library, not a stage.
 *
 * The server does not understand the state. It is an object the app writes
 * and reads, bounded in size, stamped with a revision and the server's clock.
 * The music lives in the app; putting it here twice would mean two places
 * that have to agree on what bar 17 is.
 *
 * Kept in memory only. A stage is something happening right now: after a
 * restart, the leader's next heartbeat puts it back, and a stage from last
 * night has no business coming back at all.
 *
 * Leading is a lease. The leader renews it with every write (and the app
 * writes every few seconds while it leads, so followers can correct drift);
 * a leader that went quiet — battery, wifi, the phone in a pocket — loses the
 * room after `leaseMs` and anyone may take it. Taking a room someone still
 * holds needs `force`: the app asks first.
 */

export const LEASE_MS = 15_000;
/** A room nobody leads and nobody wrote to for this long is forgotten. */
export const IDLE_MS = 12 * 60 * 60 * 1000;
export const MAX_STATE_BYTES = 8 * 1024;
export const MAX_ROOM = 40;
const MAX_NAME = 60;

/** A room name as typed: trimmed, bounded, no control characters. Null if unusable. */
export function cleanRoom(raw) {
  if (typeof raw !== 'string') return null;
  const room = raw.trim();
  // eslint-disable-next-line no-control-regex
  if (!room || room.length > MAX_ROOM || /[\u0000-\u001f\u007f/]/.test(room)) return null;
  return room;
}

function cleanName(raw) {
  if (typeof raw !== 'string') return null;
  const name = raw.trim().slice(0, MAX_NAME);
  return name || null;
}

/**
 * @param {{ now?: () => number, leaseMs?: number, onChange?: (snapshot) => void }} [options]
 */
export function createStage({ now = Date.now, leaseMs = LEASE_MS, onChange = () => {} } = {}) {
  /** room → { leader: { client, name, until } | null, state, rev, touched } */
  const rooms = new Map();

  const live = (leader) => leader && leader.until > now();

  function snapshot(room) {
    const entry = rooms.get(room);
    const leader = entry && live(entry.leader) ? entry.leader : null;
    return {
      room,
      leader: leader ? { client: leader.client, name: leader.name, until: leader.until } : null,
      state: entry?.state ?? null,
      rev: entry?.rev ?? 0,
      now: now(),
    };
  }

  function entryOf(room) {
    let entry = rooms.get(room);
    if (!entry) {
      entry = { leader: null, state: null, rev: 0, touched: now() };
      rooms.set(room, entry);
    }
    return entry;
  }

  /**
   * Take or renew the lead. `client` is the app's id (X-Chordwright-Client);
   * without one nobody could tell later who leads.
   */
  function lead(room, { client, name, force = false }) {
    if (!client) return { ok: false, status: 400, error: 'client id required' };
    const entry = entryOf(room);
    if (live(entry.leader) && entry.leader.client !== client && !force) {
      return { ok: false, status: 409, error: 'room is led', ...snapshot(room) };
    }
    const changed = !live(entry.leader) || entry.leader.client !== client;
    entry.leader = { client, name: cleanName(name) ?? entry.leader?.name ?? null, until: now() + leaseMs };
    entry.touched = now();
    if (changed) {
      entry.rev += 1;
      onChange(snapshot(room));
    }
    return { ok: true, ...snapshot(room) };
  }

  /** Let go of the lead. Only the leader can; anyone else gets a no-op. */
  function release(room, { client }) {
    const entry = rooms.get(room);
    if (!entry || !live(entry.leader) || entry.leader.client !== client) return { ok: true, ...snapshot(room) };
    entry.leader = null;
    entry.rev += 1;
    entry.touched = now();
    onChange(snapshot(room));
    return { ok: true, ...snapshot(room) };
  }

  /** The leader says where the stage is. Renews the lease. */
  function publish(room, { client, state }) {
    const entry = rooms.get(room);
    if (!entry || !live(entry.leader)) return { ok: false, status: 409, error: 'nobody leads this room', ...snapshot(room) };
    if (entry.leader.client !== client) return { ok: false, status: 403, error: 'not the leader', ...snapshot(room) };
    if (state === null || typeof state !== 'object' || Array.isArray(state)) {
      return { ok: false, status: 400, error: 'state must be an object' };
    }
    if (Buffer.byteLength(JSON.stringify(state)) > MAX_STATE_BYTES) {
      return { ok: false, status: 413, error: 'state too large' };
    }
    entry.state = state;
    entry.rev += 1;
    entry.leader.until = now() + leaseMs;
    entry.touched = now();
    onChange(snapshot(room));
    return { ok: true, ...snapshot(room) };
  }

  function read(room) {
    return snapshot(room);
  }

  /** Every room that is worth listing: led, or written to recently. */
  function list() {
    sweep();
    return [...rooms.keys()].sort().map((room) => {
      const { leader, rev } = snapshot(room);
      return { room, leader: leader ? { name: leader.name } : null, rev, touched: rooms.get(room).touched };
    });
  }

  /**
   * Leases that ran out tell the followers (they would otherwise keep waiting
   * on a leader that is gone), and old rooms are dropped. Called on a timer.
   */
  function sweep() {
    const t = now();
    for (const [room, entry] of rooms) {
      if (entry.leader && entry.leader.until <= t) {
        entry.leader = null;
        entry.rev += 1;
        onChange(snapshot(room));
      }
      if (!entry.leader && t - entry.touched > IDLE_MS) rooms.delete(room);
    }
  }

  return { lead, release, publish, read, list, sweep };
}
