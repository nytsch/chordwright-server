//! Push notifications for the band's phones, without anyone but this server
//! reading them.
//!
//! ```text
//!   app ──APNs token──▶ relay ──▶ handle (sealed by the relay, bound to our serverId)
//!   app ──PUT /api/push/<device> { handle, key, kinds }──▶ here (.chordwright/push.json)
//!   here: a write to user/lineups or user/setlists, a new message
//!         → who is concerned → AES-256-GCM(key, { v, kind, … })
//!   here ──POST /v1/send (HMAC of our secret)──▶ relay ──▶ APNs ──▶ the phone
//! ```
//!
//! The relay keeps nothing: the device token is inside the handle, sealed
//! with its own key, and our `secret` is derived from `serverId`. What it and
//! Apple see is a handle, a ciphertext and a placeholder text; the phone's
//! notification extension opens the ciphertext with `key`, which only the
//! phone and this server know, and writes the text in its own language.
//!
//! Outside `library/` and `user/`, like the messages: the watcher does not see
//! the devices and no backup carries them.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ring::aead;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::http::Shared;
use crate::proposals::{clip, Refused};
use crate::util::{hex, iso, now_ms, pretty, write_atomic};
use crate::Member;

const DEVICES: &str = ".chordwright/push.json";
const ACCOUNT: &str = ".chordwright/push-relay.json";
/// What a device can be told about.
pub const KINDS: [&str; 4] = ["assigned", "changed", "message", "day"];
/// The relay the add-on uses unless told otherwise.
pub const DEFAULT_RELAY: &str = "https://push.chordwright.app";
/// Edits to one set within this long go out as one notification.
const SETTLE: Duration = Duration::from_secs(300);
/// A device the relay has not taken anything for in this long is gone.
const STALE_MS: i64 = 90 * 24 * 3600 * 1000;
/// Of a message, this much goes into a notification.
const MESSAGE_CHARS: usize = 400;
const RELAY_TIMEOUT: Duration = Duration::from_secs(20);
const OWNER: &str = "owner";
/// After a failed sign-up at the relay, this long until the next try.
const RETRY_MS: i64 = 10 * 60 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Random per installation and library — not the phone's own id.
    pub device: String,
    /// A musician's id, or `owner`.
    pub member: String,
    pub handle: String,
    /// 32 bytes, base64: what the payload is sealed with.
    pub key: String,
    pub kinds: Vec<String>,
    pub created_at: String,
    /// When the relay last took something for it (ms); the registration at first.
    pub ok_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Account {
    relay: String,
    server_id: String,
    secret: String,
}

/// One notification for one person.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub member: String,
    pub kind: &'static str,
    /// What the phone shows, before it is sealed: `{ v, kind, … }`.
    pub body: Value,
    /// A later one with the same id replaces it on the phone.
    pub collapse: Option<String>,
}

pub struct Push {
    devices_file: PathBuf,
    account_file: PathBuf,
    lock: Mutex<()>,
    relay: Mutex<Option<String>>,
    settle: Mutex<Duration>,
    /// Waiting notes by collapse id and member, with the generation that may send them.
    waiting: Mutex<HashMap<String, (u64, Note)>>,
    generation: AtomicU64,
    joining: tokio::sync::Mutex<()>,
    /// When the last sign-up failed (ms): the next waits `RETRY_MS`.
    failed_at: std::sync::atomic::AtomicI64,
}

impl Push {
    pub fn new(root: &Path) -> Self {
        Push {
            devices_file: root.join(DEVICES),
            account_file: root.join(ACCOUNT),
            lock: Mutex::new(()),
            relay: Mutex::new(None),
            settle: Mutex::new(SETTLE),
            waiting: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            joining: tokio::sync::Mutex::new(()),
            failed_at: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// The relay to send through; None: push is off.
    pub fn set_relay(&self, relay: Option<String>) {
        *self.relay.lock().unwrap_or_else(|e| e.into_inner()) = relay
            .map(|r| r.trim().trim_end_matches('/').to_string())
            .filter(|r| r.starts_with("https://") || r.starts_with("http://"));
    }

    pub fn relay(&self) -> Option<String> {
        self.relay.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How long edits to a set are gathered (tests make it short).
    pub fn set_settle(&self, settle: Duration) {
        *self.settle.lock().unwrap_or_else(|e| e.into_inner()) = settle;
    }

    pub fn devices(&self) -> Vec<Device> {
        std::fs::read_to_string(&self.devices_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, all: &[Device]) -> Result<(), String> {
        if let Some(dir) = self.devices_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let value = serde_json::to_value(all).map_err(|e| e.to_string())?;
        write_atomic(&self.devices_file, pretty(&value).as_bytes()).map_err(|e| e.to_string())
    }

    fn change(&self, f: impl FnOnce(&mut Vec<Device>)) -> Result<(), String> {
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.devices();
        let before = all.clone();
        f(&mut all);
        if all == before {
            return Ok(());
        }
        self.save(&all)
    }

    /// Our account at the current relay, if we have one.
    fn account(&self) -> Option<Account> {
        let relay = self.relay()?;
        std::fs::read_to_string(&self.account_file)
            .ok()
            .and_then(|t| serde_json::from_str::<Account>(&t).ok())
            .filter(|a| a.relay == relay)
    }

    /// The id the app binds its handle to — for `/api/health`.
    pub fn server_id(&self) -> Option<String> {
        self.account().map(|a| a.server_id)
    }
}

/// One JSON POST to the relay: the status and the JSON back.
async fn post_json(
    url: &str,
    path: &str,
    headers: &[(&str, String)],
    body: String,
    timeout: Duration,
) -> Result<(u16, Value), String> {
    let mut request = hyper::Request::post("/").header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let reply = crate::outbound::fetch(
        url,
        request,
        path,
        bytes::Bytes::from(body),
        timeout,
        1 << 20,
    )
    .await?;
    Ok((reply.status, reply.json()))
}

fn refused(status: u16, message: &str) -> Refused {
    Refused {
        status,
        message: message.into(),
    }
}

/// Signs up at the relay once (`POST /v1/server`) and keeps what it gave.
pub async fn ensure_account(s: &Arc<Shared>) -> Option<String> {
    let relay = s.push.relay()?;
    if let Some(id) = s.push.server_id() {
        return Some(id);
    }
    let _one = s.push.joining.lock().await;
    if let Some(id) = s.push.server_id() {
        return Some(id);
    }
    if now_ms() - s.push.failed_at.load(Ordering::Relaxed) < RETRY_MS {
        return None;
    }
    let id = sign_up(s, relay).await;
    if id.is_none() {
        s.push.failed_at.store(now_ms(), Ordering::Relaxed);
    }
    id
}

async fn sign_up(s: &Arc<Shared>, relay: String) -> Option<String> {
    match post_json(&relay, "/v1/server", &[], "{}".into(), RELAY_TIMEOUT).await {
        Ok((200, reply)) => {
            let (Some(id), Some(secret)) = (reply["serverId"].as_str(), reply["secret"].as_str())
            else {
                eprintln!("push: the relay answered without an account");
                return None;
            };
            let account = Account {
                relay,
                server_id: id.into(),
                secret: secret.into(),
            };
            let saved = serde_json::to_value(&account)
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    if let Some(dir) = s.push.account_file.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    }
                    write_atomic(&s.push.account_file, pretty(&v).as_bytes())
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = saved {
                eprintln!("push: {e}");
                return None;
            }
            Some(account.server_id)
        }
        Ok((status, _)) => {
            eprintln!("push: the relay refused to sign us up ({status})");
            None
        }
        Err(e) => {
            eprintln!("push: {e}");
            None
        }
    }
}

fn valid_device(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `PUT /api/push/<device>`: `{ handle, key, kinds? }` for `member`. Replaces
/// what the device had — also if another musician had it before.
pub fn register(s: &Shared, member: &str, device: &str, body: &Value) -> Result<Value, Refused> {
    if s.push.relay().is_none() {
        return Err(refused(404, "push is off"));
    }
    if !valid_device(device) {
        return Err(refused(400, "bad device"));
    }
    let handle = body["handle"]
        .as_str()
        .filter(|h| !h.is_empty() && h.len() <= 4096)
        .ok_or_else(|| refused(400, "bad handle"))?;
    let key = body["key"]
        .as_str()
        .filter(|k| B64.decode(k).is_ok_and(|b| b.len() == 32))
        .ok_or_else(|| refused(400, "bad key"))?;
    let kinds: Vec<String> = match &body["kinds"] {
        Value::Null => KINDS.iter().map(|k| k.to_string()).collect(),
        Value::Array(list) => {
            let given: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            if given.iter().any(|k| !KINDS.contains(k)) {
                return Err(refused(400, "bad kinds"));
            }
            KINDS
                .iter()
                .filter(|k| given.contains(k))
                .map(|k| k.to_string())
                .collect()
        }
        _ => return Err(refused(400, "bad kinds")),
    };
    let now = now_ms();
    let entry = Device {
        device: device.into(),
        member: member.into(),
        handle: handle.into(),
        key: key.into(),
        kinds: kinds.clone(),
        created_at: iso(now),
        ok_at: now,
    };
    s.push
        .change(|all| {
            all.retain(|d| d.device != entry.device);
            all.push(entry);
        })
        .map_err(|e| refused(500, &e))?;
    Ok(json!({ "device": device, "kinds": kinds }))
}

/// `DELETE /api/push/<device>`: the device's own musician, or the owner.
pub fn unregister(s: &Shared, member: &str, device: &str) -> Result<(), Refused> {
    let mut found = false;
    s.push
        .change(|all| {
            all.retain(|d| {
                let mine = d.device == device && (d.member == member || member == OWNER);
                found |= mine;
                !mine
            })
        })
        .map_err(|e| refused(500, &e))?;
    if found {
        Ok(())
    } else {
        Err(refused(404, "no such device"))
    }
}

/// Seals `body` for one device: base64(nonce ‖ ciphertext ‖ tag), the device id as AAD.
pub fn seal(key_b64: &str, device: &str, body: &Value) -> Option<String> {
    let key = B64.decode(key_b64).ok()?;
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &key).ok()?);
    let mut nonce = [0u8; 12];
    SystemRandom::new().fill(&mut nonce).ok()?;
    let mut data = body.to_string().into_bytes();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(device.as_bytes()),
        &mut data,
    )
    .ok()?;
    let mut out = nonce.to_vec();
    out.extend(data);
    Some(B64.encode(out))
}

/// Opens what `seal` made — the phone's side, here for the tests.
pub fn open(key_b64: &str, device: &str, sealed: &str) -> Option<Value> {
    let key = B64.decode(key_b64).ok()?;
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &key).ok()?);
    let mut data = B64.decode(sealed).ok()?;
    if data.len() < 12 {
        return None;
    }
    let rest = data.split_off(12);
    let nonce = aead::Nonce::try_assume_unique_for_key(&data).ok()?;
    let mut rest = rest;
    let plain = key
        .open_in_place(nonce, aead::Aad::from(device.as_bytes()), &mut rest)
        .ok()?;
    serde_json::from_slice(plain).ok()
}

// ---- Who is concerned --------------------------------------------------------

/// A record as stored, `{ v, data }`, or the bare value.
fn payload(value: &Value) -> &Value {
    match value {
        Value::Object(m) if m.get("v").is_some_and(Value::is_number) && m.contains_key("data") => {
            &m["data"]
        }
        other => other,
    }
}

fn parse(text: Option<&str>) -> Value {
    text.and_then(|t| serde_json::from_str::<Value>(t).ok())
        .map(|v| payload(&v).clone())
        .unwrap_or(Value::Null)
}

fn sets_by_id(sets: &Value) -> BTreeMap<String, &Value> {
    sets.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s["id"].as_str()?.to_string(), s)))
        .collect()
}

/// Played today or later, or a set without a day.
fn upcoming(set: &Value, today: &str) -> bool {
    match set["eventEndDate"].as_str().or(set["eventDate"].as_str()) {
        Some(last) if !last.is_empty() => last >= today,
        _ => true,
    }
}

fn draft(set: &Value) -> bool {
    set["draft"] == Value::Bool(true)
}

/// Whether the set asks `member` when they can — as `asks` in the app.
pub fn asks(set: &Value, member: &str) -> bool {
    if draft(set) || set["settled"] == Value::Bool(true) || set["ask"] == json!("closed") {
        return false;
    }
    match set["ask"].as_array() {
        Some(list) => list.iter().any(|m| m == member),
        None => true,
    }
}

fn day_of(set: &Value) -> Value {
    set["eventDate"]
        .as_str()
        .filter(|d| !d.is_empty())
        .map_or(Value::Null, |d| json!(d))
}

/// Roles per member in one set's lineup, in the order of `ROLES`.
fn roles_of(lineup: &Value, member: &str) -> Vec<String> {
    crate::ROLES
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| {
            lineup[*r]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == member))
        })
        .map(String::from)
        .collect()
}

fn lineup_members(lineup: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (role, _) in crate::ROLES {
        for id in lineup[role]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !out.iter().any(|o| o == id) {
                out.push(id.into());
            }
        }
    }
    out
}

/// A write to `user/lineups`: who was newly put into an upcoming set.
pub fn after_lineups(
    before: Option<&str>,
    after: Option<&str>,
    sets: &Value,
    by: &str,
    today: &str,
) -> Vec<Note> {
    let (before, after) = (parse(before), parse(after));
    let sets = sets_by_id(sets);
    let mut notes = Vec::new();
    for (id, lineup) in after.as_object().into_iter().flatten() {
        let Some(set) = sets.get(id).filter(|s| !draft(s) && upcoming(s, today)) else {
            continue;
        };
        for member in lineup_members(lineup) {
            if member == by {
                continue;
            }
            let was = roles_of(&before[id], &member);
            let added: Vec<String> = roles_of(lineup, &member)
                .into_iter()
                .filter(|r| !was.contains(r))
                .collect();
            if added.is_empty() {
                continue;
            }
            notes.push(Note {
                member,
                kind: "assigned",
                body: json!({ "v": 1, "kind": "assigned", "setId": id, "name": set["name"], "day": day_of(set), "roles": added }),
                collapse: Some(format!("assigned:{id}")),
            });
        }
    }
    notes
}

/// A write to `user/setlists`: sets that changed for those in them
/// (`changed`), and sets that newly ask someone without an answer for the day (`day`).
pub fn after_setlists(
    before: Option<&str>,
    after: Option<&str>,
    lineups: &Value,
    members: &[Member],
    answered: &dyn Fn(&str, &str) -> bool,
    by: &str,
    today: &str,
) -> Vec<Note> {
    let (before, after) = (parse(before), parse(after));
    let (old, new) = (sets_by_id(&before), sets_by_id(&after));
    let mut notes = Vec::new();
    for (id, set) in &new {
        if !upcoming(set, today) {
            continue;
        }
        let was = old.get(id);
        if was.is_some_and(|w| w != set) && !draft(set) {
            for member in lineup_members(&lineups[id.as_str()]) {
                if member == by {
                    continue;
                }
                notes.push(Note {
                    member,
                    kind: "changed",
                    body: json!({ "v": 1, "kind": "changed", "setId": id, "name": set["name"], "day": day_of(set) }),
                    collapse: Some(format!("changed:{id}")),
                });
            }
        }
        let Some(day) = set["eventDate"]
            .as_str()
            .filter(|d| !d.is_empty() && *d >= today)
        else {
            continue;
        };
        for m in members.iter().filter(|m| m.plays && m.id != by) {
            if asks(set, &m.id) && !was.is_some_and(|w| asks(w, &m.id)) && !answered(&m.id, day) {
                notes.push(Note {
                    member: m.id.clone(),
                    kind: "day",
                    body: json!({ "v": 1, "kind": "day", "day": day, "names": [set["name"]] }),
                    collapse: Some(format!("day:{day}")),
                });
            }
        }
    }
    notes
}

/// A new message: to one musician, to all, or to those who lead — never back to its sender.
pub fn after_message(message: &Value, members: &[Member]) -> Vec<Note> {
    let from = message["from"]["id"].as_str().unwrap_or_default();
    let to = message["to"].as_str().unwrap_or_default();
    let mut who: Vec<String> = match to {
        crate::messages::ALL => std::iter::once(OWNER.to_string())
            .chain(members.iter().map(|m| m.id.clone()))
            .collect(),
        crate::messages::LEADERS => std::iter::once(OWNER.to_string())
            .chain(
                members
                    .iter()
                    .filter(|m| m.can.leads())
                    .map(|m| m.id.clone()),
            )
            .collect(),
        id => vec![id.to_string()],
    };
    who.retain(|w| w != from);
    let text = clip(message["text"].as_str().unwrap_or_default(), MESSAGE_CHARS);
    who.into_iter()
        .map(|member| Note {
            member,
            kind: "message",
            body: json!({ "v": 1, "kind": "message", "id": message["id"], "from": message["from"]["name"], "text": text }),
            collapse: None,
        })
        .collect()
}

/// Today as the app's sets write it, `YYYY-MM-DD`.
pub fn today() -> String {
    iso(now_ms())[..10].to_string()
}

// ---- Sending -----------------------------------------------------------------

/// Sends `notes` — at once, or for edits to a set after `SETTLE` without
/// another edit. Does nothing while push is off.
pub fn notify(s: &Arc<Shared>, notes: Vec<Note>) {
    if notes.is_empty() || s.push.relay().is_none() {
        return;
    }
    let (now, later): (Vec<Note>, Vec<Note>) = notes.into_iter().partition(|n| n.kind != "changed");
    if !now.is_empty() {
        let s = s.clone();
        tokio::spawn(async move { deliver(&s, now).await });
    }
    for note in later {
        let slot = format!(
            "{}|{}",
            note.collapse.as_deref().unwrap_or_default(),
            note.member
        );
        let generation = s.push.generation.fetch_add(1, Ordering::Relaxed) + 1;
        s.push
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(slot.clone(), (generation, note));
        let s = s.clone();
        let settle = *s.push.settle.lock().unwrap_or_else(|e| e.into_inner());
        tokio::spawn(async move {
            tokio::time::sleep(settle).await;
            let note = {
                let mut waiting = s.push.waiting.lock().unwrap_or_else(|e| e.into_inner());
                match waiting.get(&slot) {
                    Some((g, _)) if *g == generation => waiting.remove(&slot).map(|(_, n)| n),
                    _ => None,
                }
            };
            if let Some(note) = note {
                deliver(&s, vec![note]).await;
            }
        });
    }
}

/// Seals each note for each device that wants it and hands them to the relay.
async fn deliver(s: &Arc<Shared>, notes: Vec<Note>) {
    if ensure_account(s).await.is_none() {
        return;
    }
    let Some(account) = s.push.account() else {
        return;
    };
    let members = s.members();
    let now = now_ms();
    let alive = |id: &str| id == OWNER || members.iter().any(|m| m.id == id && !m.expired(now));
    // Who is gone, or has not been reachable for long, goes first.
    let _ = s
        .push
        .change(|all| all.retain(|d| alive(&d.member) && now - d.ok_at < STALE_MS));
    let devices = s.push.devices();
    let mut items = Vec::new();
    let mut targets = Vec::new();
    for note in &notes {
        for d in devices
            .iter()
            .filter(|d| d.member == note.member && d.kinds.iter().any(|k| k == note.kind))
        {
            let Some(enc) = seal(&d.key, &d.device, &note.body) else {
                continue;
            };
            items.push(json!({ "handle": d.handle, "enc": enc, "collapse": note.collapse }));
            targets.push(d.device.clone());
        }
    }
    if items.is_empty() {
        return;
    }
    let body = json!({ "serverId": account.server_id, "items": items }).to_string();
    let key = hmac::Key::new(hmac::HMAC_SHA256, account.secret.as_bytes());
    let signature = hex(hmac::sign(&key, body.as_bytes()).as_ref());
    let sent = post_json(
        &account.relay,
        "/v1/send",
        &[("x-chordwright-signature", signature)],
        body,
        RELAY_TIMEOUT,
    )
    .await;
    let results = match sent {
        Ok((200, reply)) => reply["results"].as_array().cloned().unwrap_or_default(),
        Ok((status, _)) => {
            eprintln!("push: the relay answered {status}");
            return;
        }
        Err(e) => {
            eprintln!("push: {e}");
            return;
        }
    };
    let now = now_ms();
    let _ = s.push.change(|all| {
        for (device, result) in targets.iter().zip(results.iter()) {
            match result.as_str() {
                Some("gone") => all.retain(|d| &d.device != device),
                Some("ok") => {
                    if let Some(d) = all.iter_mut().find(|d| &d.device == device) {
                        d.ok_at = now;
                    }
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, plays: bool) -> Member {
        Member {
            id: id.into(),
            name: id.to_uppercase(),
            token: format!("{id}-t"),
            roles: vec![],
            until: None,
            can: Default::default(),
            plays,
            arrangements: vec![],
            ai: None,
        }
    }

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[test]
    fn sealed_opens_only_with_key_and_device() {
        let body = json!({ "v": 1, "kind": "message", "text": "Hallo" });
        let sealed = seal(KEY, "dev-12345", &body).unwrap();
        assert_eq!(open(KEY, "dev-12345", &sealed), Some(body));
        assert_eq!(open(KEY, "dev-other", &sealed), None);
        assert!(!sealed.contains("Hallo"));
    }

    #[test]
    fn lineups_tell_who_was_put_in() {
        let sets = json!([
            { "id": "s1", "name": "Sonntag", "eventDate": "2026-12-06" },
            { "id": "old", "name": "Alt", "eventDate": "2026-01-01" },
            { "id": "d", "name": "Entwurf", "eventDate": "2026-12-20", "draft": true }
        ]);
        let before = r#"{"v":1,"data":{"s1":{"B":["lea"]}}}"#;
        let after = r#"{"v":1,"data":{"s1":{"B":["lea"],"D":["lea","tom"]},"old":{"P":["tom"]},"d":{"P":["tom"]}}}"#;
        let notes = after_lineups(Some(before), Some(after), &sets, "owner", "2026-11-25");
        let got: Vec<(&str, &Value)> = notes
            .iter()
            .map(|n| (n.member.as_str(), &n.body["roles"]))
            .collect();
        assert_eq!(got, vec![("lea", &json!(["D"])), ("tom", &json!(["D"]))]);
        // Whoever made the change is not told.
        assert_eq!(
            after_lineups(Some(before), Some(after), &sets, "tom", "2026-11-25").len(),
            1
        );
    }

    #[test]
    fn setlists_tell_changes_and_new_dates() {
        let lineups = json!({ "s1": { "B": ["lea"] } });
        let members = vec![
            member("lea", true),
            member("tom", true),
            member("max", false),
        ];
        let before = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","eventDate":"2026-12-06","entries":[]}]}"#;
        let after = r#"{"v":1,"data":[
            {"id":"s1","name":"Sonntag","eventDate":"2026-12-06","entries":[{"songId":"x"}]},
            {"id":"s2","name":"Probe","eventDate":"2026-12-10","ask":["tom"]}
        ]}"#;
        let answered = |m: &str, day: &str| m == "lea" && day == "2026-12-06";
        let notes = after_setlists(
            Some(before),
            Some(after),
            &lineups,
            &members,
            &answered,
            "owner",
            "2026-11-25",
        );
        let got: Vec<(&str, &str)> = notes.iter().map(|n| (n.member.as_str(), n.kind)).collect();
        assert_eq!(got, vec![("lea", "changed"), ("tom", "day")]);
        // Nothing changed: nothing to tell.
        assert!(after_setlists(
            Some(after),
            Some(after),
            &lineups,
            &members,
            &answered,
            "owner",
            "2026-11-25"
        )
        .is_empty());
    }

    #[test]
    fn messages_go_to_their_readers_not_back() {
        let mut lead = member("ann", true);
        lead.can = crate::Can::Sets;
        let members = vec![member("lea", true), lead];
        let to = |to: &str, from: &str| {
            let m =
                json!({ "id": "m1", "to": to, "text": "Hi", "from": { "id": from, "name": "X" } });
            after_message(&m, &members)
                .into_iter()
                .map(|n| n.member)
                .collect::<Vec<_>>()
        };
        assert_eq!(to("all", "owner"), vec!["lea", "ann"]);
        assert_eq!(to("leaders", "lea"), vec!["owner", "ann"]);
        assert_eq!(to("lea", "owner"), vec!["lea"]);
    }
}
