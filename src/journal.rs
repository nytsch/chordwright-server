//! Who changed what, and when.
//!
//! ```text
//!   .chordwright/changes.json
//!     records   "<db>/<key>" → { at, by, action }   the last change to each record
//!     recent    [{ db, key, at, by, action }]         newest first, at most `keep`
//! ```
//!
//! `by` is the name the app sends in `X-Chordwright-User` — set per server in
//! the app, not an account. Null when nobody said, and for a file saved by hand
//! (`action: "file"`), which the watcher sees but cannot put a name to.
//!
//! Kept in memory and written behind: `record` marks the state dirty and wakes
//! the writer, which writes the newest state once — fifty songs arriving at
//! once cost fifty map updates and a couple of file writes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};
use tokio::sync::Notify;

use crate::util::{iso, now_ms, parse_iso, pretty, write_atomic};

const DIR: &str = ".chordwright";
const FILE: &str = "changes.json";
/// Saving a song while typing sends a write every few seconds. In the list of
/// recent changes those are one change: the same record, the same person, the
/// same kind of change within this window only moves the time forward.
const MERGE_WINDOW_MS: i64 = 5 * 60 * 1000;
const MAX_NAME: usize = 60;
const KEEP: usize = 500;

/// What goes into `by`: no control characters, trimmed, bounded, or None.
pub fn clean_name(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let stripped: String = raw
        .chars()
        .filter(|c| !matches!(*c as u32, 0..=0x1f | 0x7f))
        .collect();
    let name: String = crate::util::js_trim(&stripped)
        .chars()
        .take(MAX_NAME)
        .collect();
    (!name.is_empty()).then_some(name)
}

struct State {
    records: Map<String, Value>,
    recent: Vec<Value>,
    dirty: bool,
}

pub struct Journal {
    dir: PathBuf,
    state: Mutex<State>,
    pub wake: Notify,
}

impl Journal {
    pub fn open(root: &Path) -> Self {
        let dir = root.join(DIR);
        let parsed = std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let records = match parsed.as_ref().and_then(|p| p.get("records")) {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        let recent = match parsed.as_ref().and_then(|p| p.get("recent")) {
            Some(Value::Array(list)) => list.clone(),
            _ => Vec::new(),
        };
        Journal {
            dir,
            state: Mutex::new(State {
                records,
                recent,
                dirty: false,
            }),
            wake: Notify::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Note a change. `listed: false` updates the record's last change without
    /// an entry of its own in `recent` — a restore lists itself once, not once
    /// per file.
    pub fn record(&self, db: &str, key: &str, by: Option<&str>, action: &str, listed: bool) {
        let at = iso(now_ms());
        let by = clean_name(by);
        let mut state = self.state();
        state.records.insert(
            format!("{db}/{key}"),
            json!({ "at": at, "by": by, "action": action }),
        );
        if listed {
            let merged = match state.recent.first_mut() {
                Some(last)
                    if last["db"] == db
                        && last["key"] == key
                        && last["by"] == json!(by)
                        && last["action"] == action
                        && matches!(
                            (parse_iso(&at), last["at"].as_str().and_then(parse_iso)),
                            (Some(now), Some(then)) if now - then < MERGE_WINDOW_MS
                        ) =>
                {
                    last["at"] = json!(at);
                    true
                }
                _ => false,
            };
            if !merged {
                state.recent.insert(
                    0,
                    json!({ "db": db, "key": key, "at": at, "by": by, "action": action }),
                );
                state.recent.truncate(KEEP);
            }
        }
        state.dirty = true;
        drop(state);
        self.wake.notify_one();
    }

    /// The last change to one record, or null when none is on file.
    pub fn of(&self, db: &str, key: &str) -> Value {
        self.state()
            .records
            .get(&format!("{db}/{key}"))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// `{ key: { at, by, action } }` for every record of `db` with a change on file.
    pub fn for_db(&self, db: &str) -> Map<String, Value> {
        let prefix = format!("{db}/");
        self.state()
            .records
            .iter()
            .filter_map(|(id, change)| {
                Some((id.strip_prefix(&prefix)?.to_string(), change.clone()))
            })
            .collect()
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        self.state().recent.iter().take(limit).cloned().collect()
    }

    /// Writes what was recorded since the last write, if anything was.
    pub fn flush(&self) {
        let text = {
            let mut state = self.state();
            if !state.dirty {
                return;
            }
            state.dirty = false;
            pretty(&json!({ "records": state.records, "recent": state.recent }))
        };
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|_| write_atomic(&self.dir.join(FILE), text.as_bytes()));
        if let Err(err) = written {
            eprintln!("Could not write the change journal: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(clean_name(Some(" Jürgen\n")).as_deref(), Some("Jürgen"));
        assert_eq!(clean_name(Some("   ")), None);
        assert_eq!(
            clean_name(Some(&"x".repeat(100))).map(|n| n.len()),
            Some(60)
        );
        assert_eq!(clean_name(None), None);
    }

    #[test]
    fn records_merge_and_persist() {
        let root =
            std::env::temp_dir().join(format!("cw-journal-{}-{}", std::process::id(), now_ms()));
        let journal = Journal::open(&root);
        journal.record("library", "doc.a", Some("Anna"), "write", true);
        journal.record("library", "doc.a", Some("Anna"), "write", true);
        journal.record("library", "doc.b", None, "file", true);
        journal.record("library", "doc.a", Some("Anna"), "remove", true);
        let recent: Vec<String> = journal
            .recent(10)
            .iter()
            .map(|c| {
                format!(
                    "{}:{}",
                    c["key"].as_str().unwrap(),
                    c["action"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(recent, ["doc.a:remove", "doc.b:file", "doc.a:write"]);
        assert_eq!(journal.of("library", "doc.a")["action"], "remove");
        assert_eq!(journal.of("library", "nope"), Value::Null);
        assert_eq!(journal.for_db("library").len(), 2);
        journal.flush();
        assert_eq!(
            Journal::open(&root).of("library", "doc.b")["action"],
            "file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
