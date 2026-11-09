//! What a change was, in a few words — for the list of recent changes.
//!
//! The journal knows that a record was written; this says what moved in it: a
//! tag added, a song taken out of a set, a song's key. Structured, not prose —
//! the app says it in the reader's language:
//!
//! ```text
//!   { kind: "created" }                            the record is new
//!   { kind: "added",   text, name?, in? }          a tag, a set, a song in a set
//!   { kind: "removed", text, name?, in? }
//!   { kind: "renamed", from, to, name?, in? }
//!   { kind: "field",   name?, from?, to?, in? }    a value; from/to only when short
//!   { kind: "order",   name?, in? }                the same items, another order
//!   { kind: "lines",   count }                     lines of a song's text
//! ```
//!
//! `in` is what the part happened inside (a set's name, a song's title),
//! `name` the field it happened in. Nothing about a record's shape is required:
//! what is not recognised is described as a changed field, or not at all.

use serde_json::{json, Map, Value};

/// A change to a whole library rarely needs more to be recognised.
const MAX_PARTS: usize = 12;
/// A value longer than this is named, not quoted.
const MAX_SHOWN: usize = 40;
/// Fields that move with every save and say nothing.
/// An id that changed under a part matched by its name is no news either.
const NOISE: [&str; 4] = ["updatedAt", "createdAt", "modifiedAt", "id"];
/// Beyond this many cells the line count is an estimate, not a diff.
const MAX_CELLS: usize = 1_000_000;

/// What changed from `before` to `after` (None: not there). Empty when
/// nothing did, or nothing that can be said. `titles` names a song by its id.
pub fn change(
    db: &str,
    key: &str,
    before: Option<&str>,
    after: Option<&str>,
    titles: &dyn Fn(&str) -> Option<String>,
) -> Vec<Value> {
    let Some(after) = after else {
        return Vec::new();
    };
    let mut diff = Diff {
        titles,
        parts: Vec::new(),
    };
    let Some(before) = before else {
        // New: say so, and for a record what it starts out with — the first
        // set, the tags of the first song — as if it had been there empty.
        diff.parts.push(json!({ "kind": "created" }));
        if !(db == "library" && key.starts_with("doc.")) {
            if let Ok(a) = serde_json::from_str::<Value>(after) {
                let a = payload(&a);
                diff.value(None, None, &empty_like(a), a);
            }
        }
        return diff.parts;
    };
    if before == after {
        return Vec::new();
    }
    if db == "library" && key.starts_with("doc.") {
        diff.chordpro(before, after);
    } else if let (Ok(b), Ok(a)) = (
        serde_json::from_str::<Value>(before),
        serde_json::from_str::<Value>(after),
    ) {
        diff.value(None, None, payload(&b), payload(&a));
    }
    diff.parts
}

/// The app's records are `{ v, data }`; older ones are the payload itself.
fn payload(value: &Value) -> &Value {
    match value {
        Value::Object(m) if m.get("v").is_some_and(Value::is_number) && m.contains_key("data") => {
            &m["data"]
        }
        _ => value,
    }
}

/// Nothing, in the shape of `value`.
fn empty_like(value: &Value) -> Value {
    match value {
        Value::Array(_) => json!([]),
        Value::Object(_) => json!({}),
        _ => Value::Null,
    }
}

fn is_short(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(s) => s.chars().count() <= MAX_SHOWN,
        _ => false,
    }
}

/// An item of a list with an identity: its id, how to call it, the item.
type Item<'v> = (String, String, &'v Map<String, Value>);

struct Diff<'a> {
    titles: &'a dyn Fn(&str) -> Option<String>,
    parts: Vec<Value>,
}

impl Diff<'_> {
    fn push(&mut self, mut part: Map<String, Value>, name: Option<&str>, scope: Option<&str>) {
        if self.parts.len() >= MAX_PARTS {
            return;
        }
        if let Some(name) = name {
            part.insert("name".into(), json!(name));
        }
        if let Some(scope) = scope {
            part.insert("in".into(), json!(scope));
        }
        let part = Value::Object(part);
        if !self.parts.contains(&part) {
            self.parts.push(part);
        }
    }

    fn kind(kind: &str) -> Map<String, Value> {
        let mut part = Map::new();
        part.insert("kind".into(), json!(kind));
        part
    }

    /// `name`: the field the value sits in; `scope`: what it belongs to.
    fn value(&mut self, scope: Option<&str>, name: Option<&str>, b: &Value, a: &Value) {
        if b == a {
            return;
        }
        match (b, a) {
            (Value::Array(b), Value::Array(a)) => self.list(scope, name, b, a),
            // A list that was not there, or is no more, is an empty one.
            (Value::Null, Value::Array(a)) => self.list(scope, name, &[], a),
            (Value::Array(b), Value::Null) => self.list(scope, name, b, &[]),
            // Only a record's own top level is opened; deeper objects are named.
            (Value::Object(b), Value::Object(a)) if name.is_none() => self.object(scope, b, a),
            _ => self.field(scope, name, b, a),
        }
    }

    fn field(&mut self, scope: Option<&str>, name: Option<&str>, b: &Value, a: &Value) {
        let mut part = Self::kind("field");
        if is_short(b) && is_short(a) {
            part.insert("from".into(), b.clone());
            part.insert("to".into(), a.clone());
        }
        self.push(part, name, scope);
    }

    fn object(&mut self, scope: Option<&str>, b: &Map<String, Value>, a: &Map<String, Value>) {
        let empty = Map::new();
        let keys = b.keys().chain(a.keys().filter(|k| !b.contains_key(*k)));
        for key in keys {
            if NOISE.contains(&key.as_str()) {
                continue;
            }
            let (bv, av) = (
                b.get(key).unwrap_or(&Value::Null),
                a.get(key).unwrap_or(&Value::Null),
            );
            if bv == av {
                continue;
            }
            // A table keyed by song (the song settings): which song, then what.
            if scope.is_none() {
                if let Some(title) = (self.titles)(key) {
                    let (bo, ao) = (bv.as_object(), av.as_object());
                    if bo.is_some() || ao.is_some() {
                        self.object(Some(&title), bo.unwrap_or(&empty), ao.unwrap_or(&empty));
                        continue;
                    }
                }
            }
            self.value(scope, Some(key), bv, av);
        }
    }

    fn list(&mut self, scope: Option<&str>, name: Option<&str>, b: &[Value], a: &[Value]) {
        if let (Some(bs), Some(as_)) = (strings(b), strings(a)) {
            let gone: Vec<&str> = bs.iter().filter(|t| !as_.contains(t)).copied().collect();
            let new: Vec<&str> = as_.iter().filter(|t| !bs.contains(t)).copied().collect();
            if let ([from], [to]) = (gone.as_slice(), new.as_slice()) {
                let mut part = Self::kind("renamed");
                part.insert("from".into(), json!(from));
                part.insert("to".into(), json!(to));
                self.push(part, name, scope);
                return;
            }
            for (kind, list) in [("removed", &gone), ("added", &new)] {
                for text in list.iter() {
                    let mut part = Self::kind(kind);
                    part.insert("text".into(), json!(text));
                    self.push(part, name, scope);
                }
            }
            if gone.is_empty() && new.is_empty() {
                self.push(Self::kind("order"), name, scope);
            }
            return;
        }
        if let (Some(bi), Some(ai)) = (self.items(b), self.items(a)) {
            // Each item now with the one it was: by id, and what is left over
            // by name — a part of a running order made anew keeps its name,
            // not its id, and is a changed part, not one gone and one new.
            let mut pair: Vec<Option<usize>> = ai
                .iter()
                .map(|(id, ..)| bi.iter().position(|(bid, ..)| bid == id))
                .collect();
            let mut taken = vec![false; bi.len()];
            for &j in pair.iter().flatten() {
                taken[j] = true;
            }
            for (i, (_, label, _)) in ai.iter().enumerate() {
                if pair[i].is_none() {
                    if let Some(j) = (0..bi.len()).find(|&j| !taken[j] && bi[j].1 == *label) {
                        pair[i] = Some(j);
                        taken[j] = true;
                    }
                }
            }
            // A running order: how long it is now, before the parts it got there by.
            if let (Some(from), Some(to)) = (bar_total(&bi), bar_total(&ai)) {
                if from != to {
                    let mut part = Self::kind("field");
                    part.insert("from".into(), number(from));
                    part.insert("to".into(), number(to));
                    self.push(part, Some("bars"), scope);
                }
            }
            for (j, (_, label, _)) in bi.iter().enumerate() {
                if !taken[j] {
                    let mut part = Self::kind("removed");
                    part.insert("text".into(), json!(label));
                    self.push(part, name, scope);
                }
            }
            for (i, (_, label, _)) in ai.iter().enumerate() {
                if pair[i].is_none() {
                    let mut part = Self::kind("added");
                    part.insert("text".into(), json!(label));
                    self.push(part, name, scope);
                }
            }
            let before = self.parts.len();
            for (i, (_, label, item)) in ai.iter().enumerate() {
                if let Some(j) = pair[i] {
                    let old = bi[j].2;
                    if old != *item {
                        // Where it is, all the way down: the song, its running order, the part.
                        let inner = match scope {
                            Some(outer) => format!("{outer} › {label}"),
                            None => label.clone(),
                        };
                        self.object(Some(&inner), old, item);
                    }
                }
            }
            let kept: Vec<usize> = pair.iter().flatten().copied().collect();
            if !kept.windows(2).all(|w| w[0] < w[1]) && self.parts.len() == before {
                self.push(Self::kind("order"), name, scope);
            }
            return;
        }
        self.field(scope, name, &Value::Null, &Value::Bool(true));
    }

    /// A list of things with an identity — sets, songs, a set's entries — as
    /// `(id, label, item)`. The same song twice in a set is two entries.
    fn items<'v>(&self, list: &'v [Value]) -> Option<Vec<Item<'v>>> {
        let mut seen: Vec<String> = Vec::new();
        list.iter()
            .map(|v| {
                let item = v.as_object()?;
                let id = item
                    .get("id")
                    .or_else(|| item.get("songId"))
                    .and_then(Value::as_str)?;
                let text = |field: &str| {
                    item.get(field)
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                };
                let label = text("name")
                    .or_else(|| text("title"))
                    .or_else(|| text("label"))
                    .or_else(|| {
                        item.get("item")?
                            .get("title")?
                            .as_str()
                            .filter(|s| !s.trim().is_empty())
                            .map(String::from)
                    })
                    .or_else(|| (self.titles)(id))
                    .unwrap_or_else(|| id.to_string());
                let n = seen.iter().filter(|s| *s == id).count();
                seen.push(id.to_string());
                Some((format!("{id}#{n}"), label, item))
            })
            .collect()
    }

    fn chordpro(&mut self, before: &str, after: &str) {
        let (bm, bl) = split(before);
        let (am, al) = split(after);
        let null = Value::Null;
        for name in bm.keys().chain(am.keys().filter(|k| !bm.contains_key(*k))) {
            let (b, a) = (bm.get(name).unwrap_or(&null), am.get(name).unwrap_or(&null));
            if b != a {
                self.field(None, Some(name), b, a);
            }
        }
        let count = changed_lines(&bl, &al);
        if count > 0 {
            let mut part = Self::kind("lines");
            part.insert("count".into(), json!(count));
            self.push(part, None, None);
        }
    }
}

/// A running order's length in bars — each part's `bars` times its `repeat` —
/// or None when the list is not one.
fn bar_total(items: &[Item]) -> Option<f64> {
    if items.is_empty() {
        return None;
    }
    items
        .iter()
        .map(|(.., item)| {
            let bars = item.get("bars")?.as_f64()?;
            let repeat = item.get("repeat").and_then(Value::as_f64).unwrap_or(1.0);
            Some(bars * repeat.max(1.0))
        })
        .sum()
}

/// A number as JSON, whole numbers without `.0`.
fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// All of a list's items as text, or None when one is not.
fn strings(list: &[Value]) -> Option<Vec<&str>> {
    list.iter().map(Value::as_str).collect()
}

/// The directives that are values of the song rather than part of its text.
fn meta_name(name: &str) -> Option<&str> {
    let name = match name {
        "t" => "title",
        "st" => "subtitle",
        n => n,
    };
    let known = [
        "title",
        "subtitle",
        "artist",
        "composer",
        "lyricist",
        "album",
        "year",
        "key",
        "tempo",
        "time",
        "capo",
        "duration",
        "copyright",
        "tags",
    ];
    (known.contains(&name) || name.starts_with("x_")).then_some(name)
}

/// A song's values and the lines of its text, blank lines left out.
fn split(text: &str) -> (Map<String, Value>, Vec<&str>) {
    let mut meta = Map::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(inner) = trimmed.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            let (name, value) = inner.split_once(':').unwrap_or((inner, ""));
            if let Some(name) = meta_name(&name.trim().to_lowercase()) {
                meta.insert(name.to_string(), json!(value.trim()));
                continue;
            }
        }
        lines.push(trimmed);
    }
    (meta, lines)
}

/// How many lines were changed, added or taken out: the larger side of what
/// the longest common run leaves over.
fn changed_lines(b: &[&str], a: &[&str]) -> usize {
    let head = b.iter().zip(a).take_while(|(x, y)| x == y).count();
    let (b, a) = (&b[head..], &a[head..]);
    let tail = b
        .iter()
        .rev()
        .zip(a.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (b, a) = (&b[..b.len() - tail], &a[..a.len() - tail]);
    if b.len().saturating_mul(a.len()) > MAX_CELLS {
        return b.len().max(a.len());
    }
    let mut row = vec![0usize; a.len() + 1];
    for x in b {
        let mut diagonal = 0;
        for (j, y) in a.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diagonal + 1 } else { up.max(row[j]) };
            diagonal = up;
        }
    }
    let common = row[a.len()];
    (b.len() - common).max(a.len() - common)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(id: &str) -> Option<String> {
        match id {
            "lord" => Some("Lord I Need You".into()),
            "herz" => Some("Der mein Herz regiert".into()),
            _ => None,
        }
    }

    fn describe(db: &str, key: &str, b: &str, a: &str) -> Vec<Value> {
        change(db, key, Some(b), Some(a), &titles)
    }

    #[test]
    fn nothing_new_says_nothing() {
        assert!(describe(
            "user",
            "tags",
            r#"{"v":1,"data":["A"]}"#,
            r#"{"v":1,"data":["A"]}"#
        )
        .is_empty());
        assert!(change("user", "tags", Some("[]"), None, &titles).is_empty());
        assert_eq!(
            change("user", "tags", None, Some("[]"), &titles),
            [json!({ "kind": "created" })]
        );
    }

    #[test]
    fn new_records_say_what_they_start_with() {
        let created = |key: &str, a: &str| change("user", key, None, Some(a), &titles);
        assert_eq!(
            created("tags", r#"{"v":1,"data":["Anbetung"]}"#),
            [
                json!({ "kind": "created" }),
                json!({ "kind": "added", "text": "Anbetung" })
            ]
        );
        assert_eq!(
            created(
                "setlists",
                r#"{"v":1,"data":[{"id":"s","name":"Sonntag","entries":[{"songId":"lord"}]}]}"#
            ),
            [
                json!({ "kind": "created" }),
                json!({ "kind": "added", "text": "Sonntag" })
            ]
        );
        assert_eq!(
            created(
                "songSettings",
                r#"{"v":1,"data":{"lord":{"tags":["Anbetung"]}}}"#
            ),
            [
                json!({ "kind": "created" }),
                json!({ "kind": "added", "text": "Anbetung", "name": "tags", "in": "Lord I Need You" }),
            ]
        );
        assert_eq!(
            change(
                "library",
                "doc.lord",
                None,
                Some("{title: Lord}\nText"),
                &titles
            ),
            [json!({ "kind": "created" })]
        );
    }

    #[test]
    fn running_orders() {
        let settings = |parts: &str| {
            format!(
                r#"{{"v":1,"data":{{"lord":{{"arrangements":[{{"id":"a1","name":"Standard","parts":[{parts}]}}]}}}}}}"#
            )
        };
        let before = settings(
            r#"{"id":"p1","label":"Verse 1","bars":8,"repeat":1},{"id":"p2","label":"Chorus","bars":8,"repeat":2},{"id":"p3","label":"Bridge","bars":4,"repeat":1}"#,
        );
        // Made anew: every part a new id, the verse longer, the bridge gone.
        let after = settings(
            r#"{"id":"n1","label":"Verse 1","bars":12,"repeat":1},{"id":"n2","label":"Chorus","bars":8,"repeat":2}"#,
        );
        // 8 + 8×2 + 4 = 28 before, 12 + 8×2 = 28 after: no new length to report.
        assert_eq!(
            describe("user", "songSettings", &before, &after),
            [
                json!({ "kind": "removed", "text": "Bridge", "name": "parts", "in": "Lord I Need You › Standard" }),
                json!({ "kind": "field", "name": "bars", "from": 8, "to": 12, "in": "Lord I Need You › Standard › Verse 1" }),
            ]
        );
        let longer = settings(
            r#"{"id":"p1","label":"Verse 1","bars":8,"repeat":1},{"id":"p2","label":"Chorus","bars":8,"repeat":3},{"id":"p3","label":"Bridge","bars":4,"repeat":1}"#,
        );
        assert_eq!(
            describe("user", "songSettings", &before, &longer),
            [
                json!({ "kind": "field", "name": "bars", "from": 28, "to": 36, "in": "Lord I Need You › Standard" }),
                json!({ "kind": "field", "name": "repeat", "from": 2, "to": 3, "in": "Lord I Need You › Standard › Chorus" }),
            ]
        );
    }

    #[test]
    fn tags() {
        let b = r#"{"v":1,"data":["Anbetung","Fest"]}"#;
        assert_eq!(
            describe(
                "user",
                "tags",
                b,
                r#"{"v":1,"data":["Anbetung","Fest","Advent"]}"#
            ),
            [json!({ "kind": "added", "text": "Advent" })]
        );
        assert_eq!(
            describe("user", "tags", b, r#"{"v":1,"data":["Anbetung","Feste"]}"#),
            [json!({ "kind": "renamed", "from": "Fest", "to": "Feste" })]
        );
        assert_eq!(
            describe("user", "tags", b, r#"{"v":1,"data":["Fest","Anbetung"]}"#),
            [json!({ "kind": "order" })]
        );
    }

    #[test]
    fn sets() {
        let b = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","entries":[{"songId":"lord"}]}]}"#;
        let a = r#"{"v":1,"data":[
            {"id":"s1","name":"Sonntag","updatedAt":"x","entries":[{"songId":"lord"},{"songId":"herz"}]},
            {"id":"s2","name":"Probe","entries":[]}
        ]}"#;
        assert_eq!(
            describe("user", "setlists", b, a),
            [
                json!({ "kind": "added", "text": "Probe" }),
                json!({ "kind": "added", "text": "Der mein Herz regiert", "name": "entries", "in": "Sonntag" }),
            ]
        );
        let moved = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","entries":[{"songId":"herz"},{"songId":"lord"}]}]}"#;
        let back = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","entries":[{"songId":"lord"},{"songId":"herz"}]}]}"#;
        assert_eq!(
            describe("user", "setlists", back, moved),
            [json!({ "kind": "order", "name": "entries", "in": "Sonntag" })]
        );
        let capo = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","entries":[{"songId":"lord","capo":2},{"songId":"herz"}]}]}"#;
        assert_eq!(
            describe("user", "setlists", back, capo),
            [
                json!({ "kind": "field", "name": "capo", "from": null, "to": 2, "in": "Sonntag › Lord I Need You" })
            ]
        );
    }

    #[test]
    fn index_and_song_settings() {
        let b = r#"{"v":1,"data":[{"id":"lord","title":"Lord I Need You","key":"G","tags":[],"updatedAt":"1"}]}"#;
        let a = r#"{"v":1,"data":[{"id":"lord","title":"Lord I Need You","key":"A","tags":["Anbetung"],"updatedAt":"2"}]}"#;
        assert_eq!(
            describe("library", "index", b, a),
            [
                json!({ "kind": "field", "name": "key", "from": "G", "to": "A", "in": "Lord I Need You" }),
                json!({ "kind": "added", "text": "Anbetung", "name": "tags", "in": "Lord I Need You" }),
            ]
        );
        assert_eq!(
            describe(
                "user",
                "songSettings",
                r#"{"v":1,"data":{}}"#,
                r#"{"v":1,"data":{"lord":{"transpose":2}}}"#
            ),
            [
                json!({ "kind": "field", "name": "transpose", "from": null, "to": 2, "in": "Lord I Need You" })
            ]
        );
        assert_eq!(
            describe(
                "user",
                "playerName",
                r#"{"v":1,"data":"Anna"}"#,
                r#"{"v":1,"data":"Ben"}"#
            ),
            [json!({ "kind": "field", "from": "Anna", "to": "Ben" })]
        );
    }

    #[test]
    fn songs() {
        let b = "{title: Lord}\n{key: G}\n\n[G]Lord I come\nI confess\n";
        let a = "{title: Lord}\n{key: A}\n{tempo: 72}\n\n[A]Lord I come\nI confess\nhow I lose my way\n";
        assert_eq!(
            describe("library", "doc.lord", b, a),
            [
                json!({ "kind": "field", "name": "key", "from": "G", "to": "A" }),
                json!({ "kind": "field", "name": "tempo", "from": null, "to": "72" }),
                json!({ "kind": "lines", "count": 2 }),
            ]
        );
    }

    #[test]
    fn line_counts() {
        assert_eq!(changed_lines(&["a", "b", "c"], &["a", "x", "c"]), 1);
        assert_eq!(changed_lines(&["a", "b"], &["a", "b", "c", "d"]), 2);
        assert_eq!(changed_lines(&["a", "b", "c"], &["c"]), 2);
        assert_eq!(changed_lines(&[], &[]), 0);
    }
}
