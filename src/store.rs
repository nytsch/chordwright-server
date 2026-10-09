//! The folder layout, and the mapping between a storage key and a file on disk.
//!
//! `DocumentStore` speaks in keys and opaque strings; a plain key-value dump
//! would have written the ChordPro text JSON-escaped inside a `.json` file —
//! technically a file, useless to a human. So one key shape is special-cased:
//!
//! ```text
//!   library / index          → library/index.json             (the record, pretty-printed)
//!   library / doc.<id>       → library/songs/<id>.chordpro    (the text itself)
//!   user    / <key>          → user/<key>.json
//! ```
//!
//! The two fields of a song that are not in the file — where it came from, and
//! which envelope version wrote it — live in a small sidecar
//! (`songs/_documents.json`), so a round trip through the server changes
//! nothing. A file dropped into `songs/` by hand has no entry there and is
//! reported as an import, which is exactly what it is.
//!
//! The same layout as the Node server up to 1.7.0: a folder it wrote is served
//! as it is.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::util::{iso, ms_of, pretty, revision_of, write_atomic};

pub const DATABASES: [&str; 2] = ["library", "user"];

const DOCUMENT_PREFIX: &str = "doc.";
const SONGS_DIR: &str = "songs";
const SIDECAR: &str = "_documents.json";
/// Matches the client's ENVELOPE_VERSION; only used for files that lack a sidecar entry.
const DEFAULT_ENVELOPE_VERSION: i64 = 1;

pub fn is_database(db: &str) -> bool {
    DATABASES.contains(&db)
}

/// Keys are file names. Anything that could climb out of the folder is refused.
pub fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !key.contains("..")
}

/// A record and its revision; both None when it is not there.
#[derive(Default)]
pub struct Versioned {
    pub value: Option<String>,
    pub rev: Option<String>,
}

/// What a conditional write expects to find.
pub enum Expect {
    /// No condition: the write happens whatever is there.
    Any,
    /// `If-Match: "<rev>"` (Some) or `If-None-Match: *` (None).
    Rev(Option<String>),
}

pub enum Outcome {
    Done {
        rev: Option<String>,
    },
    /// Nothing changed; this is what is there now.
    Conflict(Versioned),
}

struct Location {
    /// The song id, for a key that is a ChordPro file rather than a record.
    song: Option<String>,
    path: PathBuf,
}

pub struct Store {
    root: PathBuf,
    /// One change at a time. A conditional write is a compare and a write, and
    /// two of those interleaved would both see the old revision and both
    /// succeed — exactly the lost edit the revision is there to catch. The
    /// sidecar is one file shared by every song, a read-modify-write, and
    /// changes to it go under the same lock.
    writing: Mutex<()>,
}

impl Store {
    pub fn new(root: &Path) -> Self {
        Store {
            root: root.to_path_buf(),
            writing: Mutex::new(()),
        }
    }

    pub fn ensure_layout(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.root.join("library").join(SONGS_DIR))?;
        std::fs::create_dir_all(self.root.join("user"))
    }

    fn sidecar_path(&self) -> PathBuf {
        self.root.join("library").join(SONGS_DIR).join(SIDECAR)
    }

    fn read_sidecar(&self) -> Map<String, Value> {
        std::fs::read_to_string(self.sidecar_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn write_sidecar(&self, meta: Map<String, Value>) -> io::Result<()> {
        let path = self.sidecar_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_atomic(&path, pretty(&Value::Object(meta)).as_bytes())
    }

    fn locate(&self, db: &str, key: &str) -> Location {
        if db == "library" {
            if let Some(id) = key.strip_prefix(DOCUMENT_PREFIX) {
                let path = self
                    .root
                    .join("library")
                    .join(SONGS_DIR)
                    .join(format!("{id}.chordpro"));
                return Location {
                    song: Some(id.to_string()),
                    path,
                };
            }
        }
        Location {
            song: None,
            path: self.root.join(db).join(format!("{key}.json")),
        }
    }

    /// Rebuild the envelope the client wrote, from the file plus its sidecar entry.
    fn read_document(
        &self,
        id: &str,
        path: &Path,
        meta: &Map<String, Value>,
    ) -> io::Result<(String, String)> {
        let bytes = std::fs::read(path)?;
        let modified = std::fs::metadata(path)?.modified()?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let side = meta.get(id);
        let field = |name: &str| {
            side.and_then(|s| s.get(name))
                .filter(|v| !v.is_null())
                .cloned()
        };
        // In the client's field order: v, data { id, text, origin, updatedAt }.
        let mut data = Map::new();
        data.insert("id".into(), json!(id));
        data.insert("text".into(), json!(text));
        data.insert(
            "origin".into(),
            field("origin").unwrap_or(json!("imported")),
        );
        // The file's own timestamp, so editing it in vim really does count as a
        // newer version than what the app last wrote.
        data.insert("updatedAt".into(), json!(iso(ms_of(modified))));
        let mut envelope = Map::new();
        envelope.insert(
            "v".into(),
            field("v").unwrap_or(json!(DEFAULT_ENVELOPE_VERSION)),
        );
        envelope.insert("data".into(), Value::Object(data));
        let rev = revision_of(text.as_bytes());
        Ok((Value::Object(envelope).to_string(), rev))
    }

    pub fn read_versioned(&self, db: &str, key: &str) -> Versioned {
        let at = self.locate(db, key);
        let read = match &at.song {
            Some(id) => self.read_document(id, &at.path, &self.read_sidecar()),
            None => std::fs::read(&at.path).map(|bytes| {
                let value = String::from_utf8_lossy(&bytes).into_owned();
                let rev = revision_of(value.as_bytes());
                (value, rev)
            }),
        };
        match read {
            Ok((value, rev)) => Versioned {
                value: Some(value),
                rev: Some(rev),
            },
            Err(_) => Versioned::default(),
        }
    }

    /// Every record of `db` and its revision.
    pub fn read_all_versioned(&self, db: &str) -> (Map<String, Value>, Map<String, Value>) {
        let mut records = Map::new();
        let mut revs = Map::new();
        let base = self.root.join(db);
        for (name, path) in files_in(&base) {
            if let Some(key) = name.strip_suffix(".json") {
                if let Ok(bytes) = std::fs::read(&path) {
                    let value = String::from_utf8_lossy(&bytes).into_owned();
                    revs.insert(key.into(), json!(revision_of(value.as_bytes())));
                    records.insert(key.into(), json!(value));
                }
            }
        }
        if db != "library" {
            return (records, revs);
        }
        let meta = self.read_sidecar();
        for (name, path) in files_in(&base.join(SONGS_DIR)) {
            let Some(id) = name.strip_suffix(".chordpro") else {
                continue;
            };
            if let Ok((value, rev)) = self.read_document(id, &path, &meta) {
                let key = format!("{DOCUMENT_PREFIX}{id}");
                records.insert(key.clone(), json!(value));
                revs.insert(key, json!(rev));
            }
        }
        (records, revs)
    }

    /// Write a record. With `Expect::Rev` the write only happens if the record
    /// is still what the writer last saw; otherwise nothing is written and the
    /// current state comes back. A write that would put exactly the bytes
    /// already there is no conflict, whatever the writer last saw.
    pub fn write(
        &self,
        db: &str,
        key: &str,
        value: &str,
        expect: Expect,
    ) -> Result<Outcome, String> {
        let at = self.locate(db, key);
        let (body, sidecar_entry) = match &at.song {
            Some(_) => {
                let envelope: Value = serde_json::from_str(value).map_err(|e| e.to_string())?;
                if envelope.is_null() {
                    return Err("Cannot read properties of null (reading 'data')".into());
                }
                let doc = envelope
                    .get("data")
                    .filter(|d| !d.is_null())
                    .unwrap_or(&envelope);
                let text = match doc.get("text") {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("the song's text is not a string".into()),
                };
                let pick = |v: Option<&Value>, fallback: Value| {
                    v.filter(|v| !v.is_null()).cloned().unwrap_or(fallback)
                };
                let mut entry = Map::new();
                entry.insert("origin".into(), pick(doc.get("origin"), json!("imported")));
                entry.insert(
                    "v".into(),
                    pick(envelope.get("v"), json!(DEFAULT_ENVELOPE_VERSION)),
                );
                (text, Some(entry))
            }
            // Pretty-print anything that parses, so the file is readable. The
            // client parses it back, so the whitespace costs nothing.
            None => match serde_json::from_str::<Value>(value) {
                Ok(parsed) => (pretty(&parsed), None),
                Err(_) => (value.to_string(), None),
            },
        };
        let rev = revision_of(body.as_bytes());

        let _lock = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if let Expect::Rev(expected) = &expect {
            let current = self.read_versioned(db, key);
            if current.rev != *expected && current.rev.as_deref() != Some(rev.as_str()) {
                return Ok(Outcome::Conflict(current));
            }
        }
        if let Some(dir) = at.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        write_atomic(&at.path, body.as_bytes()).map_err(|e| e.to_string())?;
        if let (Some(id), Some(entry)) = (&at.song, sidecar_entry) {
            let mut meta = self.read_sidecar();
            meta.insert(id.clone(), Value::Object(entry));
            self.write_sidecar(meta).map_err(|e| e.to_string())?;
        }
        Ok(Outcome::Done { rev: Some(rev) })
    }

    /// Remove a record; `expect` as for `write`. Removing what is already gone succeeds.
    pub fn remove(&self, db: &str, key: &str, expect: Expect) -> Result<Outcome, String> {
        let at = self.locate(db, key);
        let _lock = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if let Expect::Rev(expected) = &expect {
            let current = self.read_versioned(db, key);
            if current.rev.is_some() && current.rev != *expected {
                return Ok(Outcome::Conflict(current));
            }
        }
        match std::fs::remove_file(&at.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
        if let Some(id) = &at.song {
            let mut meta = self.read_sidecar();
            if meta.shift_remove(id).is_some() {
                self.write_sidecar(meta).map_err(|e| e.to_string())?;
            }
        }
        Ok(Outcome::Done { rev: None })
    }
}

/// Map a changed file (relative to the root, either separator) back to the
/// key it represents. None for anything else.
pub fn key_for_path(relative: &str) -> Option<(&'static str, String)> {
    let parts: Vec<&str> = relative
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect();
    let (&first, rest) = parts.split_first()?;
    let db = DATABASES.into_iter().find(|d| *d == first)?;
    if rest.is_empty() {
        return None;
    }
    if db == "library" && rest[0] == SONGS_DIR {
        let name = rest.get(1)?;
        if *name == SIDECAR {
            return None;
        }
        let id = name.strip_suffix(".chordpro")?;
        return Some((db, format!("{DOCUMENT_PREFIX}{id}")));
    }
    if rest.len() != 1 {
        return None;
    }
    Some((db, rest[0].strip_suffix(".json")?.to_string()))
}

/// The plain files directly in `dir`, by name; nothing when it does not exist.
fn files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cw-store-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn doc(id: &str, text: &str) -> String {
        json!({ "v": 1, "data": { "id": id, "text": text, "origin": "seed", "updatedAt": "x" } })
            .to_string()
    }

    #[test]
    fn keys() {
        assert!(is_valid_key("doc.abc-1_2"));
        assert!(!is_valid_key(".hidden"));
        assert!(!is_valid_key("a..b"));
        assert!(!is_valid_key("a/b"));
        assert_eq!(
            key_for_path("library/songs/x.chordpro"),
            Some(("library", "doc.x".into()))
        );
        assert_eq!(
            key_for_path("library\\index.json"),
            Some(("library", "index".into()))
        );
        assert_eq!(key_for_path("library/songs/_documents.json"), None);
        assert_eq!(key_for_path("library/songs/.x.chordpro.1.2.tmp"), None);
        assert_eq!(key_for_path("user/a/b.json"), None);
    }

    #[test]
    fn songs_round_trip_and_conflicts() {
        let root = temp();
        let store = Store::new(&root);
        store.ensure_layout().unwrap();
        let Outcome::Done { rev: Some(rev) } = store
            .write("library", "doc.a", &doc("a", "eins"), Expect::Rev(None))
            .unwrap()
        else {
            panic!("create-only write failed")
        };
        assert_eq!(
            std::fs::read_to_string(root.join("library/songs/a.chordpro")).unwrap(),
            "eins"
        );
        let read = store.read_versioned("library", "doc.a");
        assert_eq!(read.rev.as_deref(), Some(rev.as_str()));
        let value: Value = serde_json::from_str(read.value.as_deref().unwrap()).unwrap();
        assert_eq!(value["data"]["origin"], "seed");
        assert_eq!(value["data"]["text"], "eins");

        // Create-only again: a conflict, unless it is the same bytes.
        assert!(matches!(
            store
                .write("library", "doc.a", &doc("a", "zwei"), Expect::Rev(None))
                .unwrap(),
            Outcome::Conflict(_)
        ));
        assert!(matches!(
            store
                .write("library", "doc.a", &doc("a", "eins"), Expect::Rev(None))
                .unwrap(),
            Outcome::Done { .. }
        ));
        assert!(matches!(
            store
                .write(
                    "library",
                    "doc.a",
                    &doc("a", "zwei"),
                    Expect::Rev(Some(rev.clone()))
                )
                .unwrap(),
            Outcome::Done { .. }
        ));
        assert!(matches!(
            store
                .remove("library", "doc.a", Expect::Rev(Some(rev)))
                .unwrap(),
            Outcome::Conflict(_)
        ));
        assert!(matches!(
            store.remove("library", "doc.a", Expect::Any).unwrap(),
            Outcome::Done { .. }
        ));
        assert!(store.read_versioned("library", "doc.a").value.is_none());
        assert_eq!(
            std::fs::read_to_string(root.join("library/songs/_documents.json")).unwrap(),
            "{}\n"
        );

        store
            .write("user", "settings", r#"{"b":1,"a":[]}"#, Expect::Any)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("user/settings.json")).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": []\n}\n"
        );
        let (records, revs) = store.read_all_versioned("user");
        assert!(records.contains_key("settings") && revs.contains_key("settings"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
