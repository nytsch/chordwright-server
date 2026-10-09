//! Shared stage — one device leads, the others follow.
//!
//! A room is a band on stage: one leader, any number of followers. The leader
//! says where the stage is; the followers read it and go along. The server
//! does not understand the state: it is an object the app writes and reads,
//! bounded in size, stamped with a revision and the server's clock.
//!
//! Kept in memory only. A stage is something happening right now: after a
//! restart, the leader's next heartbeat puts it back.
//!
//! Leading is a lease, renewed with every write; a leader that went quiet
//! loses the room after `LEASE_MS`. Taking a room someone still holds needs
//! `force`. Followers say they are there the same way (`here`), and drop out
//! after `PRESENCE_MS`. Protocol: docs/GEMEINSAME-BUEHNE.md.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::util::{js_trim, now_ms};

pub const LEASE_MS: i64 = 15_000;
pub const PRESENCE_MS: i64 = 12_000;
/// A room nobody leads and nobody wrote to for this long is forgotten.
pub const IDLE_MS: i64 = 12 * 60 * 60 * 1000;
pub const MAX_STATE_BYTES: usize = 8 * 1024;
pub const MAX_ROOM: usize = 40;
const MAX_NAME: usize = 60;

/// A room name as typed: trimmed, bounded, no control characters or slashes.
pub fn clean_room(raw: &str) -> Option<String> {
    let room = js_trim(raw);
    let bad = |c: char| matches!(c as u32, 0..=0x1f | 0x7f) || c == '/';
    (!room.is_empty() && room.encode_utf16().count() <= MAX_ROOM && !room.chars().any(bad))
        .then(|| room.to_string())
}

fn clean_name(raw: Option<&str>) -> Option<String> {
    let name: String = js_trim(raw?).chars().take(MAX_NAME).collect();
    (!name.is_empty()).then_some(name)
}

struct Leader {
    client: String,
    name: Option<String>,
    until: i64,
}

struct Follower {
    name: Option<String>,
    attached: bool,
    until: i64,
}

struct Room {
    leader: Option<Leader>,
    state: Value,
    rev: u64,
    touched: i64,
    followers: BTreeMap<String, Follower>,
}

/// An answer for the HTTP layer: the status and the JSON body.
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

pub struct Stage {
    rooms: Mutex<BTreeMap<String, Room>>,
    on_change: Box<dyn Fn(Value) + Send + Sync>,
}

fn live(leader: &Option<Leader>, now: i64) -> Option<&Leader> {
    leader.as_ref().filter(|l| l.until > now)
}

fn snapshot(rooms: &BTreeMap<String, Room>, room: &str) -> Value {
    let now = now_ms();
    let entry = rooms.get(room);
    let leader = entry.and_then(|e| live(&e.leader, now));
    let mut followers: Vec<(&String, &Follower)> = entry
        .map(|e| {
            e.followers
                .iter()
                .filter(|(client, f)| {
                    f.until > now && Some(client.as_str()) != leader.map(|l| l.client.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    followers.sort_by(|(ca, a), (cb, b)| {
        let name = |f: &Follower| f.name.clone().unwrap_or_default().to_lowercase();
        name(a).cmp(&name(b)).then_with(|| ca.cmp(cb))
    });
    json!({
        "room": room,
        "leader": leader.map(|l| json!({ "client": l.client, "name": l.name, "until": l.until })),
        "followers": followers
            .into_iter()
            .map(|(client, f)| json!({ "client": client, "name": f.name, "attached": f.attached }))
            .collect::<Vec<_>>(),
        "state": entry.map(|e| e.state.clone()).unwrap_or(Value::Null),
        "rev": entry.map(|e| e.rev).unwrap_or(0),
        "now": now,
    })
}

fn ok(body: Value) -> Answer {
    Answer { status: 200, body }
}

fn fail(status: u16, error: &str, snapshot: Option<Value>) -> Answer {
    let mut body = Map::new();
    body.insert("error".into(), json!(error));
    if let Some(Value::Object(fields)) = snapshot {
        body.extend(fields);
    }
    Answer {
        status,
        body: Value::Object(body),
    }
}

impl Stage {
    pub fn new(on_change: impl Fn(Value) + Send + Sync + 'static) -> Self {
        Stage {
            rooms: Mutex::new(BTreeMap::new()),
            on_change: Box::new(on_change),
        }
    }

    fn rooms(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Room>> {
        self.rooms.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self, rooms: &BTreeMap<String, Room>, room: &str) {
        (self.on_change)(snapshot(rooms, room));
    }

    /// Take or renew the lead. `client` is the app's id (X-Chordwright-Client);
    /// without one nobody could tell later who leads.
    pub fn lead(
        &self,
        room: &str,
        client: Option<&str>,
        name: Option<&str>,
        force: bool,
    ) -> Answer {
        let Some(client) = client.filter(|c| !c.is_empty()) else {
            return fail(400, "client id required", None);
        };
        let now = now_ms();
        let mut rooms = self.rooms();
        let entry = rooms.entry(room.to_string()).or_insert_with(|| Room {
            leader: None,
            state: Value::Null,
            rev: 0,
            touched: now,
            followers: BTreeMap::new(),
        });
        let current = live(&entry.leader, now).map(|l| l.client.clone());
        if current.as_deref().is_some_and(|c| c != client) && !force {
            return fail(409, "room is led", Some(snapshot(&rooms, room)));
        }
        let changed = current.as_deref() != Some(client);
        let name = clean_name(name).or_else(|| entry.leader.as_ref().and_then(|l| l.name.clone()));
        entry.leader = Some(Leader {
            client: client.to_string(),
            name,
            until: now + LEASE_MS,
        });
        entry.touched = now;
        if changed {
            entry.rev += 1;
            self.changed(&rooms, room);
        }
        ok(snapshot(&rooms, room))
    }

    /// Let go of the lead. Only the leader can; anyone else gets a no-op.
    pub fn release(&self, room: &str, client: Option<&str>) -> Answer {
        let now = now_ms();
        let mut rooms = self.rooms();
        if let Some(entry) = rooms.get_mut(room) {
            if live(&entry.leader, now).is_some_and(|l| Some(l.client.as_str()) == client) {
                entry.leader = None;
                entry.rev += 1;
                entry.touched = now;
                self.changed(&rooms, room);
            }
        }
        ok(snapshot(&rooms, room))
    }

    /// The leader says where the stage is. Renews the lease.
    pub fn publish(&self, room: &str, client: Option<&str>, state: Option<&Value>) -> Answer {
        let now = now_ms();
        let mut rooms = self.rooms();
        let Some(leader) = rooms.get(room).and_then(|e| live(&e.leader, now)) else {
            return fail(409, "nobody leads this room", Some(snapshot(&rooms, room)));
        };
        if Some(leader.client.as_str()) != client {
            return fail(403, "not the leader", Some(snapshot(&rooms, room)));
        }
        let Some(state) = state.filter(|s| s.is_object()) else {
            return fail(400, "state must be an object", None);
        };
        if state.to_string().len() > MAX_STATE_BYTES {
            return fail(413, "state too large", None);
        }
        let entry = rooms.get_mut(room).expect("checked above");
        entry.state = state.clone();
        entry.rev += 1;
        if let Some(leader) = entry.leader.as_mut() {
            leader.until = now + LEASE_MS;
        }
        entry.touched = now;
        self.changed(&rooms, room);
        ok(snapshot(&rooms, room))
    }

    /// A follower says it is there — and whether it is going along right now.
    pub fn here(
        &self,
        room: &str,
        client: Option<&str>,
        name: Option<&str>,
        attached: bool,
    ) -> Answer {
        let Some(client) = client.filter(|c| !c.is_empty()) else {
            return fail(400, "client id required", None);
        };
        let now = now_ms();
        let mut rooms = self.rooms();
        let entry = rooms.entry(room.to_string()).or_insert_with(|| Room {
            leader: None,
            state: Value::Null,
            rev: 0,
            touched: now,
            followers: BTreeMap::new(),
        });
        let was = entry.followers.get(client);
        let name = clean_name(name).or_else(|| was.and_then(|f| f.name.clone()));
        let news = match was {
            None => true,
            Some(f) => f.until <= now || f.name != name || f.attached != attached,
        };
        entry.followers.insert(
            client.to_string(),
            Follower {
                name,
                attached,
                until: now + PRESENCE_MS,
            },
        );
        entry.touched = now;
        if news {
            entry.rev += 1;
            self.changed(&rooms, room);
        }
        ok(snapshot(&rooms, room))
    }

    /// A follower that leaves the stage says so, rather than fading out.
    pub fn leave(&self, room: &str, client: Option<&str>) -> Answer {
        let mut rooms = self.rooms();
        if let (Some(entry), Some(client)) = (rooms.get_mut(room), client) {
            if entry.followers.remove(client).is_some() {
                entry.rev += 1;
                self.changed(&rooms, room);
            }
        }
        ok(snapshot(&rooms, room))
    }

    pub fn read(&self, room: &str) -> Value {
        snapshot(&self.rooms(), room)
    }

    /// Every room that is worth listing: led, or written to recently.
    pub fn list(&self) -> Vec<Value> {
        self.sweep();
        let rooms = self.rooms();
        rooms
            .iter()
            .map(|(room, entry)| {
                let snap = snapshot(&rooms, room);
                json!({
                    "room": room,
                    "leader": snap["leader"].get("name").map(|name| json!({ "name": name })),
                    "followers": snap["followers"].as_array().map_or(0, Vec::len),
                    "rev": entry.rev,
                    "touched": entry.touched,
                })
            })
            .collect()
    }

    /// Leases that ran out tell the followers (they would otherwise keep
    /// waiting on a leader that is gone), and old rooms are dropped. Called on
    /// a timer.
    pub fn sweep(&self) {
        let now = now_ms();
        let mut rooms = self.rooms();
        let names: Vec<String> = rooms.keys().cloned().collect();
        for room in names {
            let entry = rooms.get_mut(&room).expect("listed above");
            let mut changed = false;
            if entry.leader.as_ref().is_some_and(|l| l.until <= now) {
                entry.leader = None;
                changed = true;
            }
            let before = entry.followers.len();
            entry.followers.retain(|_, f| f.until > now);
            changed |= entry.followers.len() != before;
            if changed {
                entry.rev += 1;
                self.changed(&rooms, &room);
            }
            let entry = &rooms[&room];
            if entry.leader.is_none() && entry.followers.is_empty() && now - entry.touched > IDLE_MS
            {
                rooms.remove(&room);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn lead_publish_takeover_follow() {
        let events = Arc::new(Mutex::new(0));
        let counter = events.clone();
        let stage = Stage::new(move |_| *counter.lock().unwrap() += 1);
        assert_eq!(stage.lead("Probe", None, None, false).status, 400);
        let a = stage.lead("Probe", Some("a"), Some(" Anna "), false);
        assert_eq!(a.status, 200);
        assert_eq!(a.body["leader"]["name"], "Anna");
        assert_eq!(stage.lead("Probe", Some("b"), None, false).status, 409);
        assert_eq!(
            stage.publish("Probe", Some("b"), Some(&json!({}))).status,
            403
        );
        assert_eq!(
            stage.publish("Probe", Some("a"), Some(&json!([]))).status,
            400
        );
        let big = json!({ "x": "y".repeat(MAX_STATE_BYTES) });
        assert_eq!(stage.publish("Probe", Some("a"), Some(&big)).status, 413);
        let published = stage.publish("Probe", Some("a"), Some(&json!({ "song": 3 })));
        assert_eq!(published.body["state"]["song"], 3);
        let here = stage.here("Probe", Some("b"), Some("Ben"), false);
        assert_eq!(here.body["followers"][0]["attached"], false);
        let taken = stage.lead("Probe", Some("b"), None, true);
        assert_eq!(taken.body["leader"]["client"], "b");
        assert_eq!(
            taken.body["followers"].as_array().unwrap().len(),
            0,
            "the leader is no follower"
        );
        assert_eq!(stage.list()[0]["room"], "Probe");
        assert!(*events.lock().unwrap() >= 4);
        assert_eq!(clean_room(" Probe "), Some("Probe".into()));
        assert_eq!(clean_room("a/b"), None);
        assert_eq!(clean_room(&"x".repeat(41)), None);
    }
}
