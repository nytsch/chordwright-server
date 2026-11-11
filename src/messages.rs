//! Short messages between the band and those who lead it: a musician
//! reports a mistake, the leader tells someone they were put into a set —
//! and whoever got one says that they read it.
//!
//! ```text
//!   .chordwright/messages.json
//!     [{ id, kind, text, from: { id, name }, to, createdAt, readBy: [{ id, name, at }] }]
//! ```
//!
//! `to` is a musician's id, `leaders` (the owner and every musician who may
//! edit the sets) or `all`. A musician who does not lead writes to the
//! leaders only. Outside `library/` and `user/`, like the proposals: the
//! watcher does not see them and no backup carries them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::http::Shared;
use crate::proposals::{clip, Author, Refused};
use crate::util::{iso, new_id, now_ms, pretty, write_atomic};

const FILE: &str = ".chordwright/messages.json";
const MAX_TEXT: usize = 2000;
const MAX_KIND: usize = 20;
/// Messages kept per sender; their oldest go first.
const KEEP_PER_SENDER: usize = 100;
pub const LEADERS: &str = "leaders";
pub const ALL: &str = "all";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Read {
    pub id: String,
    pub name: Option<String>,
    pub at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    /// What it is about, for the app to show it by: `note`, `bug`, `assigned`, …
    pub kind: String,
    pub text: String,
    pub from: Author,
    pub to: String,
    pub created_at: String,
    #[serde(default)]
    pub read_by: Vec<Read>,
}

/// Who asks: a musician or the owner (`id` `owner`).
#[derive(Clone)]
pub struct Reader {
    pub id: String,
    pub name: Option<String>,
    pub owner: bool,
    /// The owner, or a musician who may edit the sets.
    pub leads: bool,
}

impl Reader {
    /// The owner sees every message, anyone else what they sent and what
    /// went to them.
    fn sees(&self, m: &Message) -> bool {
        self.owner
            || m.from.id == self.id
            || m.to == self.id
            || m.to == ALL
            || (m.to == LEADERS && self.leads)
    }
}

pub struct Messages {
    file: PathBuf,
    /// One change at a time: the file is read, changed and written whole.
    lock: Mutex<()>,
}

impl Messages {
    pub fn new(root: &Path) -> Self {
        Messages {
            file: root.join(FILE),
            lock: Mutex::new(()),
        }
    }

    /// Every message, oldest first.
    fn load(&self) -> Vec<Message> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, all: &[Message]) -> Result<(), Refused> {
        let failed = |e: std::io::Error| refused(500, e.to_string());
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        let value = serde_json::to_value(all).map_err(|e| refused(500, e.to_string()))?;
        write_atomic(&self.file, pretty(&value).as_bytes()).map_err(failed)
    }
}

fn refused(status: u16, message: impl Into<String>) -> Refused {
    Refused {
        status,
        message: message.into(),
    }
}

/// A message as `who` gets it: `read` whether they read it (their own:
/// always), `readBy` only for its sender and those who lead.
pub fn view(m: &Message, who: &Reader) -> Value {
    let mine = m.from.id == who.id;
    let mut out = json!({
        "id": m.id, "kind": m.kind, "text": m.text, "from": m.from, "to": m.to,
        "createdAt": m.created_at,
        "read": mine || m.read_by.iter().any(|r| r.id == who.id),
    });
    if mine || who.leads {
        out["readBy"] = json!(m.read_by);
    }
    out
}

/// What `who` sees, newest first.
pub fn list(s: &Shared, who: &Reader) -> Vec<Value> {
    let all = s.messages.load();
    all.iter()
        .rev()
        .filter(|m| who.sees(m))
        .map(|m| view(m, who))
        .collect()
}

/// Sends one: `{ text, to?, kind? }`. Without `to` it goes to the leaders.
pub fn create(s: &Shared, who: &Reader, body: &Value) -> Result<Value, Refused> {
    let word = |name: &str| body.get(name).and_then(Value::as_str).unwrap_or_default();
    let text = clip(word("text"), MAX_TEXT);
    if text.is_empty() {
        return Err(refused(400, "text: needed"));
    }
    let kind = match word("kind") {
        "" => "note".to_string(),
        k if k.len() <= MAX_KIND
            && k.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') =>
        {
            k.to_string()
        }
        _ => return Err(refused(400, "kind: a short lowercase word")),
    };
    let to = match word("to") {
        "" => LEADERS.to_string(),
        to => to.to_string(),
    };
    if to != LEADERS {
        if !who.leads {
            return Err(refused(403, "only to the leaders"));
        }
        if to != ALL && (to == who.id || !s.members().iter().any(|m| m.id == to)) {
            return Err(refused(400, "to: no such musician"));
        }
    }
    let _lock = s.messages.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = s.messages.load();
    let mut id = new_id('m');
    while all.iter().any(|m| m.id == id) {
        id = new_id('m');
    }
    let message = Message {
        id,
        kind,
        text,
        from: Author {
            id: who.id.clone(),
            name: who.name.clone(),
        },
        to,
        created_at: iso(now_ms()),
        read_by: Vec::new(),
    };
    all.push(message.clone());
    let sent = all.iter().filter(|m| m.from.id == who.id).count();
    let mut extra = sent.saturating_sub(KEEP_PER_SENDER);
    all.retain(|m| {
        let drop = extra > 0 && m.from.id == who.id;
        extra -= usize::from(drop);
        !drop
    });
    s.messages.save(&all)?;
    s.listeners.send(event_line(&message.id, false));
    Ok(view(&message, who))
}

/// `who` has read it. Again changes nothing; the sender's own neither.
pub fn mark_read(s: &Shared, who: &Reader, id: &str) -> Result<Value, Refused> {
    let _lock = s.messages.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = s.messages.load();
    let Some(m) = all.iter_mut().find(|m| m.id == id && who.sees(m)) else {
        return Err(refused(404, "no such message"));
    };
    if m.from.id != who.id && !m.read_by.iter().any(|r| r.id == who.id) {
        m.read_by.push(Read {
            id: who.id.clone(),
            name: who.name.clone(),
            at: iso(now_ms()),
        });
        let out = view(m, who);
        s.messages.save(&all)?;
        s.listeners.send(event_line(id, false));
        return Ok(out);
    }
    Ok(view(m, who))
}

/// Removes one: its sender, or the owner.
pub fn remove(s: &Shared, who: &Reader, id: &str) -> Result<(), Refused> {
    let _lock = s.messages.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = s.messages.load();
    let Some(at) = all.iter().position(|m| m.id == id && who.sees(m)) else {
        return Err(refused(404, "no such message"));
    };
    if !who.owner && all[at].from.id != who.id {
        return Err(refused(403, "not yours"));
    }
    all.remove(at);
    s.messages.save(&all)?;
    s.listeners.send(event_line(id, true));
    Ok(())
}

/// A named event, like the proposals': only the id, so nobody learns from
/// the stream what was written to someone else.
pub fn event_line(id: &str, removed: bool) -> String {
    format!(
        "event: message\ndata: {}\n\n",
        json!({ "id": id, "removed": removed })
    )
}
