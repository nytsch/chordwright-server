//! Snapshots of the whole library — songs, sets, settings — as plain folders.
//!
//! ```text
//!   backups/
//!     20260928-101530/
//!       backup.json        when, why, how many songs and sets
//!       library/…          a copy of library/ at that moment
//!       user/…             a copy of user/
//! ```
//!
//! A snapshot is a folder, not an archive: it opens like the live one, and a
//! single song comes back by dragging one file.
//!
//! Three reasons a snapshot is taken:
//! - `auto`: on a schedule, and only when something changed since the last
//!   one. The newest `keep` are kept.
//! - `manual`: asked for from the app. Kept until someone deletes it.
//! - `restore`: what was there just before a restore, so that a restore can
//!   itself be undone. Kept like a manual one.
//!
//! A restore writes file by file into the live folders instead of swapping
//! them: the watcher watches those directories, and a directory renamed away
//! takes the watch with it.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf, MAIN_SEPARATOR_STR};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use crate::util::{hex, iso, now_ms, parse_iso, pretty, write_atomic};

pub const BACKUP_DIR: &str = "backups";
const TREES: [&str; 2] = ["library", "user"];
const META: &str = "backup.json";
const REASONS: [&str; 3] = ["auto", "manual", "restore"];

/// `^\d{8}-\d{6}(-\d+)?$`
pub fn is_valid_backup_id(id: &str) -> bool {
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = id.splitn(3, '-');
    let (Some(date), Some(time)) = (parts.next(), parts.next()) else {
        return false;
    };
    digits(date, 8)
        && digits(time, 6)
        && parts
            .next()
            .is_none_or(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Serialize, Clone)]
pub struct Entry {
    pub id: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub reason: String,
    pub songs: u64,
    pub sets: u64,
    pub bytes: u64,
    pub fingerprint: String,
}

#[derive(Serialize)]
pub struct Restored {
    pub restored: Entry,
    pub safety: Entry,
    /// Relative paths, with this system's separator.
    pub changed: Vec<String>,
}

struct Survey {
    files: Vec<(String, Vec<u8>)>,
    bytes: u64,
    songs: u64,
    sets: u64,
    fingerprint: String,
}

/// Every file under `dir`, as paths relative to it, sorted. Temp files and
/// anything else starting with a dot are not files.
fn walk(dir: &Path) -> Vec<String> {
    fn visit(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir.join(prefix)) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}{MAIN_SEPARATOR_STR}{name}")
            };
            match entry.file_type() {
                Ok(t) if t.is_dir() => visit(dir, &rel, out),
                Ok(t) if t.is_file() => out.push(rel),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(dir, "", &mut out);
    out.sort();
    out
}

fn join(tree: &str, rel: &str) -> String {
    format!("{tree}{MAIN_SEPARATOR_STR}{rel}")
}

/// `20260928-101530` from `2026-09-28T10:15:30.123Z`.
fn stamp_of(at: &str) -> String {
    format!(
        "{}-{}",
        at[..10].replace('-', ""),
        at[11..19].replace(':', "")
    )
}

pub struct Backups {
    root: PathBuf,
    base: PathBuf,
    keep: usize,
    /// One at a time: two snapshots in the same second would want the same
    /// name, and a restore must not run while one is being copied.
    serial: Mutex<()>,
}

impl Backups {
    pub fn new(root: &Path, keep: usize) -> Self {
        Backups {
            root: root.to_path_buf(),
            base: root.join(BACKUP_DIR),
            keep,
            serial: Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.serial.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The library under `from` as it is now: its files, a fingerprint over
    /// them, the counts.
    fn survey(&self, from: &Path) -> io::Result<Survey> {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut survey = Survey {
            files: Vec::new(),
            bytes: 0,
            songs: 0,
            sets: 0,
            fingerprint: String::new(),
        };
        let setlists = join("user", "setlists.json");
        for tree in TREES {
            for rel in walk(&from.join(tree)) {
                let path = join(tree, &rel);
                let body = std::fs::read(from.join(&path))?;
                survey.bytes += body.len() as u64;
                for part in [path.as_bytes(), b"\0", &body, b"\0"] {
                    hash.update(part);
                }
                if tree == "library" && path.ends_with(".chordpro") {
                    survey.songs += 1;
                }
                if path == setlists {
                    if let Ok(data) = serde_json::from_slice::<Value>(&body) {
                        let list = if data.is_array() {
                            Some(&data)
                        } else {
                            data.get("data")
                        };
                        if let Some(Value::Array(list)) = list {
                            survey.sets = list.len() as u64;
                        }
                    }
                }
                survey.files.push((path, body));
            }
        }
        survey.fingerprint = hex(hash.finish().as_ref())[..16].to_string();
        Ok(survey)
    }

    fn read_meta(&self, id: &str) -> Option<Entry> {
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(self.base.join(id).join(META)).ok()?)
                .ok()?;
        if !meta.is_object() {
            return None;
        }
        let count = |v: &Value| {
            v.as_u64()
                .or_else(|| v.as_f64().filter(|f| *f > 0.0).map(|f| f as u64))
                .unwrap_or(0)
        };
        let text = |v: &Value| match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let reason = meta["reason"]
            .as_str()
            .filter(|r| REASONS.contains(r))
            .unwrap_or("manual");
        Some(Entry {
            id: id.to_string(),
            created_at: text(&meta["createdAt"]),
            reason: reason.to_string(),
            songs: count(&meta["songs"]),
            sets: count(&meta["sets"]),
            bytes: count(&meta["bytes"]),
            fingerprint: text(&meta["fingerprint"]),
        })
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Entry> {
        let Ok(entries) = std::fs::read_dir(&self.base) else {
            return Vec::new();
        };
        let mut all: Vec<Entry> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|id| is_valid_backup_id(id))
            .filter_map(|id| self.read_meta(&id))
            .collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all
    }

    /// Only auto snapshots age out; the others stay until someone deletes them.
    fn prune(&self) {
        let autos: Vec<Entry> = self
            .list()
            .into_iter()
            .filter(|b| b.reason == "auto")
            .collect();
        for old in autos.iter().skip(self.keep.max(1)) {
            let _ = std::fs::remove_dir_all(self.base.join(&old.id));
        }
    }

    fn take(&self, reason: &str, current: &Survey) -> io::Result<Entry> {
        let at = iso(now_ms());
        let stamp = stamp_of(&at);
        let mut id = stamp.clone();
        let mut n = 2;
        while self.base.join(&id).exists() {
            id = format!("{stamp}-{n}");
            n += 1;
        }
        let dir = self.base.join(&id);
        // A folder without backup.json is invisible to `list` — the meta file
        // goes last and is what makes it a snapshot.
        for (path, body) in &current.files {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, body)?;
        }
        let meta = json!({
            "createdAt": at,
            "reason": reason,
            "songs": current.songs,
            "sets": current.sets,
            "bytes": current.bytes,
            "fingerprint": current.fingerprint,
        });
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(META), pretty(&meta))?;
        Ok(Entry {
            id,
            created_at: at,
            reason: reason.to_string(),
            songs: current.songs,
            sets: current.sets,
            bytes: current.bytes,
            fingerprint: current.fingerprint.clone(),
        })
    }

    /// A snapshot now, whatever changed or did not.
    pub fn create(&self, reason: &str) -> io::Result<Entry> {
        let _serial = self.lock();
        let entry = self.take(reason, &self.survey(&self.root)?)?;
        self.prune();
        Ok(entry)
    }

    /// The scheduled one: only if the newest auto snapshot is at least
    /// `every_ms` old, and only if the library differs from the newest
    /// snapshot of any kind. An empty library is not worth keeping. None when
    /// nothing was taken.
    pub fn create_if_due(&self, every_ms: f64) -> io::Result<Option<Entry>> {
        let _serial = self.lock();
        let all = self.list();
        if let Some(last) = all.iter().find(|b| b.reason == "auto") {
            if let Some(then) = parse_iso(&last.created_at) {
                if ((now_ms() - then) as f64) < every_ms {
                    return Ok(None);
                }
            }
        }
        let current = self.survey(&self.root)?;
        if current.files.is_empty()
            || all
                .first()
                .is_some_and(|b| b.fingerprint == current.fingerprint)
        {
            return Ok(None);
        }
        let entry = self.take("auto", &current)?;
        self.prune();
        Ok(Some(entry))
    }

    /// Put a snapshot back. What is there now is kept first as a `restore`
    /// snapshot. None when there is no such snapshot.
    pub fn restore(&self, id: &str) -> io::Result<Option<Restored>> {
        let _serial = self.lock();
        if !is_valid_backup_id(id) {
            return Ok(None);
        }
        let Some(target) = self.read_meta(id) else {
            return Ok(None);
        };
        let safety = self.take("restore", &self.survey(&self.root)?)?;
        let snapshot = self.base.join(id);
        let mut wanted = HashSet::new();
        let mut changed = Vec::new();
        for tree in TREES {
            for rel in walk(&snapshot.join(tree)) {
                let path = join(tree, &rel);
                let body = std::fs::read(snapshot.join(&path))?;
                let live = self.root.join(&path);
                wanted.insert(path.clone());
                if std::fs::read(&live).is_ok_and(|old| old == body) {
                    continue;
                }
                if let Some(parent) = live.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_atomic(&live, &body)?;
                changed.push(path);
            }
        }
        for tree in TREES {
            for rel in walk(&self.root.join(tree)) {
                let path = join(tree, &rel);
                if wanted.contains(&path) {
                    continue;
                }
                match std::fs::remove_file(self.root.join(&path)) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
                changed.push(path);
            }
        }
        Ok(Some(Restored {
            restored: target,
            safety,
            changed,
        }))
    }

    /// False when there was nothing by that id.
    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let _serial = self.lock();
        if !is_valid_backup_id(id) || self.read_meta(id).is_none() {
            return Ok(false);
        }
        std::fs::remove_dir_all(self.base.join(id))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(is_valid_backup_id("20260928-101530"));
        assert!(is_valid_backup_id("20260928-101530-2"));
        assert!(!is_valid_backup_id("20260928-101530-"));
        assert!(!is_valid_backup_id("2026098-101530"));
        assert!(!is_valid_backup_id("../20260928-101530"));
        assert_eq!(stamp_of("2026-09-28T10:15:30.123Z"), "20260928-101530");
    }

    #[test]
    fn snapshot_schedule_and_restore() {
        let root =
            std::env::temp_dir().join(format!("cw-backups-{}-{}", std::process::id(), now_ms()));
        let backups = Backups::new(&root, 2);
        assert!(
            backups.create_if_due(0.0).unwrap().is_none(),
            "an empty library is not kept"
        );
        std::fs::create_dir_all(root.join("library/songs")).unwrap();
        std::fs::create_dir_all(root.join("user")).unwrap();
        std::fs::write(root.join("library/songs/a.chordpro"), "eins").unwrap();
        std::fs::write(root.join("user/setlists.json"), r#"{"v":1,"data":[{},{}]}"#).unwrap();
        let first = backups
            .create_if_due(0.0)
            .unwrap()
            .expect("a first snapshot");
        assert_eq!(
            (first.songs, first.sets, first.reason.as_str()),
            (1, 2, "auto")
        );
        assert!(backups.create_if_due(0.0).unwrap().is_none(), "unchanged");

        std::fs::write(root.join("library/songs/a.chordpro"), "zwei").unwrap();
        std::fs::write(root.join("library/songs/b.chordpro"), "neu").unwrap();
        let restored = backups.restore(&first.id).unwrap().expect("restored");
        assert_eq!(
            std::fs::read_to_string(root.join("library/songs/a.chordpro")).unwrap(),
            "eins"
        );
        assert!(!root.join("library/songs/b.chordpro").exists());
        assert_eq!(restored.changed.len(), 2);
        assert_eq!(restored.safety.reason, "restore");
        assert!(backups.remove(&restored.safety.id).unwrap());
        assert!(!backups.remove(&restored.safety.id).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
