//! Changes proposed by someone who may not make them — a musician who keeps
//! a copy of the library (a mirror) and sends back what they changed there.
//!
//! ```text
//!   .chordwright/proposals/<id>.json
//!     { id, title, note, by: { id, name }, createdAt, status, decidedAt, decidedBy,
//!       changes: [{ db, key, base, value, state }] }
//! ```
//!
//! `base` is the revision the change was made on (null: the record did not
//! exist), `value` what it should become (null: removed). Nothing of it
//! touches `library/` or `user/` until the owner accepts it; then each change
//! is written as a conditional write on its `base`, exactly as the app writes
//! with `If-Match`. A record that changed since is a conflict: it stays open
//! in the proposal and comes back with what is there now, for the owner's app
//! to merge, to accept anyway (`force`), or to leave.
//!
//! Outside `library/` and `user/`, so the watcher does not see proposals and
//! no backup carries them.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::http::Shared;
use crate::store::{is_database, is_valid_key, song_text, song_title, Expect, Outcome};
use crate::util::{hex, iso, now_ms, parse_iso, pretty, write_atomic};

const DIR: &str = ".chordwright/proposals";
/// Records in one proposal: a whole library of a band is a few hundred.
pub const MAX_CHANGES: usize = 1000;
/// Open proposals per author; more is someone sending in a loop.
pub const MAX_OPEN: usize = 20;
/// Decided proposals that stay to be looked at; older ones go.
const KEEP_CLOSED: usize = 50;
const MAX_TITLE: usize = 120;
const MAX_NOTE: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    /// Every change written.
    Accepted,
    /// Closed by the owner; what was accepted of it before stays.
    Rejected,
    /// Taken back by its author.
    Withdrawn,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Open,
    Applied,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Author {
    /// A musician's id, or `owner`.
    pub id: String,
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Change {
    pub db: String,
    pub key: String,
    /// The revision it was made on; None: the record was not there.
    pub base: Option<String>,
    /// What it becomes, as a PUT would send it; None: removed.
    pub value: Option<String>,
    #[serde(default)]
    pub state: State,
}

impl Change {
    pub fn id(&self) -> String {
        format!("{}/{}", self.db, self.key)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    pub by: Author,
    pub created_at: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub decided_at: Option<String>,
    #[serde(default)]
    pub decided_by: Option<String>,
    pub changes: Vec<Change>,
}

impl Proposal {
    /// When it was sent, ms since the epoch.
    pub fn created_ms(&self) -> i64 {
        parse_iso(&self.created_at).unwrap_or(0)
    }

    /// When it was decided, ms since the epoch.
    pub fn decided_ms(&self) -> Option<i64> {
        self.decided_at.as_deref().and_then(parse_iso)
    }
}

/// `p<yyyymmdd>-<hhmmss>-<4 hex>`: sorts by time, one path segment.
pub fn is_valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn new_id() -> String {
    let stamp: String = iso(now_ms())
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let mut nonce = [0u8; 2];
    SystemRandom::new()
        .fill(&mut nonce)
        .expect("system randomness");
    format!("p{}-{}-{}", &stamp[..8], &stamp[8..], hex(&nonce))
}

fn clip(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    clean.trim().chars().take(max).collect()
}

pub struct Proposals {
    dir: PathBuf,
    /// One change at a time: two accepts of the same proposal must not both
    /// write it.
    lock: Mutex<()>,
}

impl Proposals {
    pub fn new(root: &Path) -> Self {
        Proposals {
            dir: root.join(DIR),
            lock: Mutex::new(()),
        }
    }

    fn locked(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn read(&self, id: &str) -> Option<Proposal> {
        if !is_valid_id(id) {
            return None;
        }
        let text = std::fs::read_to_string(self.path(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, p: &Proposal) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let value = serde_json::to_value(p).map_err(io::Error::other)?;
        write_atomic(&self.path(&p.id), pretty(&value).as_bytes())
    }

    /// Every proposal, newest first.
    pub fn list(&self) -> Vec<Proposal> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut all: Vec<Proposal> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                self.read(name.strip_suffix(".json")?)
            })
            .collect();
        all.sort_by(|a, b| b.id.cmp(&a.id));
        all
    }

    pub fn get(&self, id: &str) -> Option<Proposal> {
        self.read(id)
    }

    /// Only the newest `KEEP_CLOSED` decided ones stay.
    fn prune(&self) {
        let closed = self.list().into_iter().filter(|p| p.status != Status::Open);
        for old in closed.skip(KEEP_CLOSED) {
            let _ = std::fs::remove_file(self.path(&old.id));
        }
    }
}

/// Why a proposal was not taken or not changed.
#[derive(Debug)]
pub struct Refused {
    pub status: u16,
    pub message: String,
}

fn refused(status: u16, message: impl Into<String>) -> Refused {
    Refused {
        status,
        message: message.into(),
    }
}

/// A proposal as a list shows it: what it touches, without the values.
pub fn summary(p: &Proposal) -> Value {
    let changes: Vec<Value> = p
        .changes
        .iter()
        .map(|c| {
            json!({
                "db": c.db, "key": c.key, "base": c.base,
                "remove": c.value.is_none(), "state": c.state,
            })
        })
        .collect();
    let open = p.changes.iter().filter(|c| c.state == State::Open).count();
    json!({
        "id": p.id, "title": p.title, "note": p.note, "by": p.by,
        "createdAt": p.created_at, "status": p.status,
        "decidedAt": p.decided_at, "decidedBy": p.decided_by,
        "open": open, "changes": changes,
    })
}

/// What a change is, for a person: a song by the title it gets, else as
/// the store names the record.
pub(crate) fn label(s: &Shared, c: &Change) -> String {
    let proposed = (c.db == "library" && c.key.starts_with("doc."))
        .then(|| c.value.as_deref().and_then(song_text))
        .flatten()
        .and_then(|text| song_title(&text));
    proposed.unwrap_or_else(|| s.store.label(&c.db, &c.key))
}

/// A proposal with its values, and for each change what is there now:
/// `current` its revision, `stale` whether it moved since `base`.
pub fn detail(s: &Shared, p: &Proposal) -> Value {
    let mut out = summary(p);
    let changes: Vec<Value> = p
        .changes
        .iter()
        .map(|c| {
            let now = s.store.read_versioned(&c.db, &c.key).rev;
            json!({
                "db": c.db, "key": c.key, "base": c.base, "value": c.value,
                "remove": c.value.is_none(), "state": c.state,
                "label": label(s, c),
                "stale": c.state == State::Open && now != c.base,
                "current": now,
            })
        })
        .collect();
    out["changes"] = Value::Array(changes);
    out
}

/// Takes a proposal: `{ title, note?, changes: [{ db, key, base?, value }] }`.
/// Changes that would leave a record as it is are left out; a proposal that
/// changes nothing is refused. `own` is the author's own answers record,
/// the one `availability.*` they may name.
pub fn create(
    s: &Shared,
    by: Author,
    own: Option<String>,
    body: &Value,
) -> Result<Proposal, Refused> {
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .map(|t| clip(t, MAX_TITLE))
        .unwrap_or_default();
    let note = body
        .get("note")
        .and_then(Value::as_str)
        .map(|n| clip(n, MAX_NOTE))
        .filter(|n| !n.is_empty());
    let Some(list) = body.get("changes").and_then(Value::as_array) else {
        return Err(refused(400, "changes: a list is needed"));
    };
    if list.len() > MAX_CHANGES {
        return Err(refused(413, format!("at most {MAX_CHANGES} changes")));
    }
    let mut changes: Vec<Change> = Vec::new();
    for item in list {
        let text = |name: &str| match item.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(v)) => Ok(Some(v.clone())),
            Some(_) => Err(refused(400, format!("{name}: a string or null"))),
        };
        let db = text("db")?.unwrap_or_default();
        let key = text("key")?.unwrap_or_default();
        if !is_database(&db) || !is_valid_key(&key) {
            return Err(refused(400, format!("bad db or key: {db}/{key}")));
        }
        if db == "user" && key.starts_with("availability.") && own.as_deref() != Some(&key) {
            return Err(refused(403, format!("{db}/{key}: not yours")));
        }
        let (base, value) = (text("base")?, text("value")?);
        if changes.iter().any(|c| c.db == db && c.key == key) {
            return Err(refused(400, format!("{db}/{key}: twice")));
        }
        let current = s.store.read_versioned(&db, &key).rev;
        let after = match &value {
            Some(v) => Some(
                s.store
                    .revision_for(&db, &key, v)
                    .map_err(|e| refused(400, format!("{db}/{key}: {e}")))?,
            ),
            None => None,
        };
        if after == current {
            continue;
        }
        changes.push(Change {
            db,
            key,
            base,
            value,
            state: State::Open,
        });
    }
    if changes.is_empty() {
        return Err(refused(400, "nothing changed"));
    }
    let _lock = s.proposals.locked();
    let open = s
        .proposals
        .list()
        .iter()
        .filter(|p| p.status == Status::Open && p.by.id == by.id)
        .count();
    if open >= MAX_OPEN {
        return Err(refused(429, format!("at most {MAX_OPEN} open proposals")));
    }
    let mut id = new_id();
    while s.proposals.path(&id).exists() {
        id = new_id();
    }
    let p = Proposal {
        id,
        title: if title.is_empty() {
            format!("{} Änderungen", changes.len())
        } else {
            title
        },
        note,
        by,
        created_at: iso(now_ms()),
        status: Status::Open,
        decided_at: None,
        decided_by: None,
        changes,
    };
    s.proposals
        .save(&p)
        .map_err(|e| refused(500, e.to_string()))?;
    s.listeners.send(event_line(&p.id, Some(p.status)));
    Ok(p)
}

/// What accepting wrote, and what it could not.
pub struct Accepted {
    pub proposal: Proposal,
    /// `db/key` of each record written.
    pub applied: Vec<String>,
    /// Changes whose record moved since their base: `{ db, key, base, value, rev }`
    /// with `value`/`rev` what is there now.
    pub conflicts: Vec<Value>,
}

/// Writes the open changes of a proposal (`only`: just these `db/key`), each
/// on its base — or over whatever is there, with `force`. The proposal is
/// accepted once nothing of it is open.
pub fn accept(
    s: &Shared,
    id: &str,
    only: Option<&[String]>,
    force: bool,
    decided_by: Option<&str>,
    client: Option<&str>,
) -> Result<Accepted, Refused> {
    let _lock = s.proposals.locked();
    let Some(mut p) = s.proposals.read(id) else {
        return Err(refused(404, "no such proposal"));
    };
    if p.status != Status::Open {
        return Err(refused(409, "already decided"));
    }
    let by = p.by.name.clone();
    let mut applied = Vec::new();
    let mut conflicts = Vec::new();
    for c in p.changes.iter_mut() {
        if c.state != State::Open || only.is_some_and(|o| !o.contains(&c.id())) {
            continue;
        }
        let expect = if force {
            Expect::Any
        } else {
            Expect::Rev(c.base.clone())
        };
        let stamp = c.id();
        s.self_writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                stamp.clone(),
                (client.map(String::from), std::time::Instant::now()),
            );
        let outcome = match &c.value {
            Some(v) => s.store.write(&c.db, &c.key, v, expect),
            None => s.store.remove(&c.db, &c.key, expect),
        };
        match outcome {
            Ok(Outcome::Done { .. }) => {
                c.state = State::Applied;
                // Under the author's name: it is their change, the owner let it in.
                let action = if c.value.is_some() { "write" } else { "remove" };
                s.journal.record(&c.db, &c.key, by.as_deref(), action, true);
                s.listeners
                    .send(crate::http::change_line(&c.db, Some(&c.key), client));
                applied.push(stamp);
            }
            Ok(Outcome::Conflict(now)) => {
                s.self_writes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stamp);
                conflicts.push(json!({
                    "db": c.db, "key": c.key, "base": c.base, "value": now.value, "rev": now.rev,
                }));
            }
            Err(e) => {
                s.self_writes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stamp);
                // What was written so far stays written, and says so.
                let _ = s.proposals.save(&p);
                return Err(refused(500, format!("{stamp}: {e}")));
            }
        }
    }
    if p.changes.iter().all(|c| c.state == State::Applied) {
        p.status = Status::Accepted;
        p.decided_at = Some(iso(now_ms()));
        p.decided_by = decided_by.map(String::from);
    }
    s.proposals
        .save(&p)
        .map_err(|e| refused(500, e.to_string()))?;
    if p.status != Status::Open {
        s.proposals.prune();
    }
    s.listeners.send(event_line(&p.id, Some(p.status)));
    Ok(Accepted {
        proposal: p,
        applied,
        conflicts,
    })
}

/// Closes an open proposal (the owner rejects, the author withdraws), or
/// removes a decided one for good.
pub fn close(
    s: &Shared,
    id: &str,
    status: Status,
    decided_by: Option<&str>,
) -> Result<Option<Proposal>, Refused> {
    let _lock = s.proposals.locked();
    let Some(mut p) = s.proposals.read(id) else {
        return Err(refused(404, "no such proposal"));
    };
    if p.status != Status::Open {
        std::fs::remove_file(s.proposals.path(id)).map_err(|e| refused(500, e.to_string()))?;
        s.listeners.send(event_line(id, None));
        return Ok(None);
    }
    p.status = status;
    p.decided_at = Some(iso(now_ms()));
    p.decided_by = decided_by.map(String::from);
    s.proposals
        .save(&p)
        .map_err(|e| refused(500, e.to_string()))?;
    s.proposals.prune();
    s.listeners.send(event_line(&p.id, Some(p.status)));
    Ok(Some(p))
}

/// A record's value as a person reads it: a song's ChordPro text, anything
/// else as pretty-printed JSON (or as it is, when it is not JSON).
pub(crate) fn readable(db: &str, key: &str, value: &str) -> String {
    if db == "library" && key.starts_with("doc.") {
        if let Some(text) = song_text(value) {
            return text;
        }
    }
    match serde_json::from_str::<Value>(value) {
        Ok(v) => pretty(&v),
        Err(_) => value.to_string(),
    }
}

/// A named event, like the stage's: apps that only listen with `onmessage`
/// never see it.
pub fn event_line(id: &str, status: Option<Status>) -> String {
    format!(
        "event: proposal\ndata: {}\n\n",
        json!({ "id": id, "status": status })
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = new_id();
        assert!(is_valid_id(&id), "{id}");
        assert_eq!(id.len(), "p20261104-101530-abcd".len());
        assert!(!is_valid_id("../x"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("P1"));
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clip("  Neue\u{7}Akkorde \n", 120), "NeueAkkorde");
        assert_eq!(clip(&"x".repeat(200), 120).len(), 120);
    }
}
