//! The routes — the same since the Node server up to 1.7.0, so apps of any
//! age talk to this one.
//!
//! ```text
//!   GET    /api/:db                → { key: value } for the whole database
//!   GET    /api/:db?revs=1         → { records, revs, changes }
//!   GET    /api/:db/record/:key    → { value, rev, change } | 404
//!   GET    /api/:db/record/:key?quiet=1 → a miss as 200 { value: null, rev: null, change, missing: true }
//!   PUT    /api/:db/record/:key    → body is the raw value string
//!   DELETE /api/:db/record/:key
//!   GET    /api/events             → SSE: {"db","key","client"} on every change, `event: stage` for the stage
//!   GET    /api/health             → { ok, databases, dir, watching, revisions, backups, changes, stage }
//!   GET    /api/changes?limit=50   → { changes: [...] }, newest first
//!   GET    /api/changes?db=&key=   → { change }
//!   GET    /api/time               → { now }
//!   GET    /api/stage              → { now, rooms }
//!   GET    /api/stage/:room        → the room
//!   PUT    /api/stage/:room        → the leader's state; body { state }
//!   POST   /api/stage/:room/lead   → take or renew the lead; body { name, force }
//!   DELETE /api/stage/:room/lead
//!   POST   /api/stage/:room/here   → a follower is there; body { name, attached }
//!   DELETE /api/stage/:room/here
//!   GET    /api/backups            → { backups, everyHours, keep }
//!   POST   /api/backups            → a snapshot now → 201
//!   POST   /api/backups/:id/restore → { restored, safety, changed }
//!   DELETE /api/backups/:id
//!   GET    /ca.crt                 → the authority that signed the certificate, as DER
//! ```
//!
//! PUT and DELETE take `If-Match: "<rev>"` or `If-None-Match: *`; when the
//! record is no longer what the writer saw, nothing changes and the answer is
//! 412 with what is there now.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Frame, Incoming};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::backups::Backups;
use crate::journal::Journal;
use crate::stage::{clean_room, Answer, Stage};
use crate::store::{is_database, is_valid_key, key_for_path, Expect, Outcome, Store, DATABASES};
use crate::util::{decode_component, now_ms, number, parse_query};

pub type Body = BoxBody<Bytes, Infallible>;

/// Everyone listening on /api/events.
#[derive(Default)]
pub struct Listeners(Mutex<Vec<mpsc::UnboundedSender<Bytes>>>);

impl Listeners {
    pub fn send(&self, text: String) {
        let bytes = Bytes::from(text);
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // A listener that went away is noticed here, at the next line for it.
        all.retain(|tx| tx.send(bytes.clone()).is_ok());
    }

    fn add(&self) -> mpsc::UnboundedReceiver<Bytes> {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(Bytes::from_static(b": connected\n\n"));
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
        rx
    }

    fn count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|tx| !tx.is_closed())
            .count()
    }
}

/// A change to a record (`key` None: the whole database moved).
pub fn change_line(db: &str, key: Option<&str>, client: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({ "db": db, "key": key, "client": client })
    )
}

/// A named event: an app that only listens with `onmessage` never sees it.
pub fn stage_line(snapshot: &Value) -> String {
    format!("event: stage\ndata: {snapshot}\n\n")
}

pub struct Shared {
    pub root: PathBuf,
    pub token: String,
    pub backup_every: f64,
    pub backup_keep: usize,
    pub ca_der: Option<Vec<u8>>,
    pub store: Store,
    pub backups: Backups,
    pub journal: Journal,
    pub stage: Stage,
    pub listeners: Arc<Listeners>,
    /// A write we just made comes back through the watcher too; the client id
    /// rides along so the tab that wrote is not told about its own change.
    pub self_writes: Mutex<HashMap<String, (Option<String>, Instant)>>,
    /// A restore writes the files itself; the watcher would book each one as
    /// an edit by hand. Until then (ms), it is the restore's.
    pub restoring_until: AtomicI64,
}

/// Long enough for the watcher to fire, short enough that a later edit by
/// hand is not mistaken for this client's own write.
const SELF_WRITE_WINDOW: Duration = Duration::from_secs(1);

impl Shared {
    pub fn self_write(&self, stamp: &str) -> Option<Option<String>> {
        let writes = self.self_writes.lock().unwrap_or_else(|e| e.into_inner());
        writes
            .get(stamp)
            .filter(|(_, at)| at.elapsed() < SELF_WRITE_WINDOW)
            .map(|(client, _)| client.clone())
    }

    /// A file changed on disk (`rel` relative to the root).
    pub fn file_changed(&self, rel: &str) {
        let Some((db, key)) = key_for_path(rel) else {
            return;
        };
        let stamp = format!("{db}/{key}");
        let ours = self.self_write(&stamp);
        // Not ours and not a restore: someone saved the file by hand.
        if ours.is_none() && now_ms() > self.restoring_until.load(Ordering::Relaxed) {
            self.journal.record(db, &key, None, "file", true);
        }
        self.listeners
            .send(change_line(db, Some(&key), ours.flatten().as_deref()));
    }
}

/// The SSE stream: whatever the listeners get, as it comes.
struct Stream(mpsc::UnboundedReceiver<Bytes>);

impl hyper::body::Body for Stream {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(cx)
            .map(|line| line.map(|b| Ok(Frame::data(b))))
    }
}

fn respond(status: u16, body: Option<&Value>, extra: &[(&str, String)]) -> Response<Body> {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut res = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("content-type", "application/json; charset=utf-8")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-allow-headers",
            "authorization, content-type, x-chordwright-client, x-chordwright-user, if-match, if-none-match",
        )
        .header("access-control-expose-headers", "etag")
        .header("access-control-allow-methods", "GET, PUT, POST, DELETE, OPTIONS")
        .header("cache-control", "no-store");
    for (name, value) in extra {
        res = res.header(*name, value.as_str());
    }
    res.body(Full::new(Bytes::from(payload)).boxed())
        .expect("valid response")
}

fn send(status: u16, body: Value) -> Response<Body> {
    respond(status, Some(&body), &[])
}

fn error(status: u16, message: &str) -> Response<Body> {
    send(status, json!({ "error": message }))
}

fn answer(a: Answer) -> Response<Body> {
    send(a.status, a.body)
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

/// The name the app sends with its writes. Encoded by the client, because a
/// header only carries Latin-1 and people are called Jürgen.
fn user_of(headers: &HeaderMap) -> Option<String> {
    let raw = header(headers, "x-chordwright-user").filter(|r| !r.is_empty())?;
    Some(decode_component(&raw).unwrap_or(raw))
}

/// `If-Match: "abc"` → abc, `If-None-Match: *` → must not exist, neither → no condition.
fn expected_revision(headers: &HeaderMap) -> Expect {
    if let Some(m) = header(headers, "if-match")
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
    {
        let m = m.strip_prefix("W/").unwrap_or(&m);
        let m = m.strip_prefix('"').unwrap_or(m);
        return Expect::Rev(Some(m.strip_suffix('"').unwrap_or(m).to_string()));
    }
    if header(headers, "if-none-match").is_some_and(|m| m.trim() == "*") {
        return Expect::Rev(None);
    }
    Expect::Any
}

/// Runs the file work off the async threads.
async fn blocking<T: Send + 'static>(
    s: &Arc<Shared>,
    work: impl FnOnce(&Shared) -> T + Send + 'static,
) -> T {
    let s = s.clone();
    tokio::task::spawn_blocking(move || work(&s))
        .await
        .expect("file work panicked")
}

pub async fn handle(s: Arc<Shared>, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    Ok(route(s, req).await)
}

async fn route(s: Arc<Shared>, req: Request<Incoming>) -> Response<Body> {
    // Chrome asks before a public page may reach a private address (Private
    // Network Access); this header is the server's yes.
    if req.method() == Method::OPTIONS {
        return respond(
            204,
            None,
            &[("access-control-allow-private-network", "true".into())],
        );
    }
    let path = req.uri().path().to_string();
    let query = parse_query(req.uri().query().unwrap_or(""));
    let param = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let parts: Vec<String> = path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    let part = |i: usize| parts.get(i).map(String::as_str);
    let method = req.method().clone();

    // The certificate authority, for a device to install once. Public, like
    // /api/health: a CA certificate is not a secret.
    if let Some(der) = s
        .ca_der
        .as_ref()
        .filter(|_| method == Method::GET && path == "/ca.crt")
    {
        return Response::builder()
            .status(200)
            .header("content-type", "application/x-x509-ca-cert")
            .header(
                "content-disposition",
                "attachment; filename=\"chordwright-ca.crt\"",
            )
            .header("cache-control", "no-store")
            .body(Full::new(Bytes::from(der.clone())).boxed())
            .expect("valid response");
    }
    if part(0) != Some("api") {
        return error(404, "not found");
    }
    if part(1) == Some("health") {
        return send(
            200,
            json!({
                "ok": true,
                "databases": DATABASES,
                "dir": s.root.to_string_lossy(),
                "watching": s.listeners.count(),
                "revisions": true,
                "backups": true,
                "changes": true,
                "stage": true,
            }),
        );
    }
    if !s.token.is_empty() {
        let bearer = header(req.headers(), "authorization")
            .is_some_and(|h| h == format!("Bearer {}", s.token));
        if !bearer && param("token").as_deref() != Some(s.token.as_str()) {
            return error(401, "unauthorised");
        }
    }

    match part(1) {
        Some("events") => {
            let rx = s.listeners.add();
            return Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-store")
                .header("access-control-allow-origin", "*")
                .body(BodyExt::boxed(Stream(rx)))
                .expect("valid response");
        }
        Some("time") if parts.len() == 2 && method == Method::GET => {
            return send(200, json!({ "now": now_ms() }))
        }
        Some("stage") => return stage(&s, req, &parts[2..]).await,
        Some("backups") => return backups(&s, req, &parts[2..]).await,
        Some("changes") if parts.len() == 2 && method == Method::GET => {
            let (db, key) = (param("db"), param("key"));
            if db.is_some() || key.is_some() {
                let (db, key) = (db.unwrap_or_default(), key.unwrap_or_default());
                if !is_database(&db) || !is_valid_key(&key) {
                    return error(400, "bad db or key");
                }
                return send(200, json!({ "change": s.journal.of(&db, &key) }));
            }
            let limit = param("limit")
                .map(|l| l.trim().parse::<f64>().unwrap_or(f64::NAN))
                .filter(|l| l.is_finite() && *l != 0.0)
                .unwrap_or(50.0)
                .clamp(1.0, 500.0);
            return send(200, json!({ "changes": s.journal.recent(limit as usize) }));
        }
        _ => {}
    }

    let db = part(1).unwrap_or_default().to_string();
    if !is_database(&db) {
        return error(404, "unknown database");
    }
    if parts.len() == 2 && method == Method::GET {
        if param("revs").as_deref() == Some("1") {
            let body = blocking(&s, move |s| {
                let (records, revs) = s.store.read_all_versioned(&db);
                json!({ "records": records, "revs": revs, "changes": s.journal.for_db(&db) })
            })
            .await;
            return send(200, body);
        }
        let records = blocking(&s, move |s| s.store.read_all_versioned(&db).0).await;
        return send(200, Value::Object(records));
    }
    if part(2) != Some("record") || parts.len() != 4 {
        return error(404, "not found");
    }
    let Some(key) = decode_component(&parts[3]).filter(|k| is_valid_key(k)) else {
        return error(400, "bad key");
    };
    let client = header(req.headers(), "x-chordwright-client");

    if method == Method::GET {
        let quiet = query.iter().any(|(k, _)| k == "quiet");
        let (db2, key2) = (db.clone(), key.clone());
        let read = blocking(&s, move |s| s.store.read_versioned(&db2, &key2)).await;
        // The last change goes along either way: a removed record was removed by someone.
        let change = s.journal.of(&db, &key);
        return match read.value {
            Some(value) => {
                let rev = read.rev.unwrap_or_default();
                respond(
                    200,
                    Some(&json!({ "value": value, "rev": rev, "change": change })),
                    &[("etag", format!("\"{rev}\""))],
                )
            }
            // A miss is an answer, not an error — but a browser logs every 404
            // in red, and an app asks for a dozen records nobody has set yet.
            None if quiet => send(
                200,
                json!({ "value": null, "rev": null, "change": change, "missing": true }),
            ),
            None => send(
                404,
                json!({ "error": "no such record", "rev": null, "change": change }),
            ),
        };
    }

    if method == Method::PUT || method == Method::DELETE {
        let stamp = format!("{db}/{key}");
        s.self_writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(stamp.clone(), (client.clone(), Instant::now()));
        let expect = expected_revision(req.headers());
        let by = user_of(req.headers());
        let put = method == Method::PUT;
        let value = if put {
            match req.into_body().collect().await {
                Ok(body) => String::from_utf8_lossy(&body.to_bytes()).into_owned(),
                Err(e) => return error(400, &e.to_string()),
            }
        } else {
            String::new()
        };
        let (db2, key2) = (db.clone(), key.clone());
        let result = blocking(&s, move |s| {
            if put {
                s.store.write(&db2, &key2, &value, expect)
            } else {
                s.store.remove(&db2, &key2, expect)
            }
        })
        .await;
        return match result {
            Ok(Outcome::Conflict(current)) => {
                s.self_writes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stamp);
                send(412, json!({ "value": current.value, "rev": current.rev }))
            }
            Ok(Outcome::Done { rev }) => {
                s.journal.record(
                    &db,
                    &key,
                    by.as_deref(),
                    if put { "write" } else { "remove" },
                    true,
                );
                s.listeners
                    .send(change_line(&db, Some(&key), client.as_deref()));
                let etag: Vec<(&str, String)> = rev
                    .map(|r| ("etag", format!("\"{r}\"")))
                    .into_iter()
                    .collect();
                respond(204, None, &etag)
            }
            Err(e) => {
                eprintln!("{e}");
                error(500, &e)
            }
        };
    }
    error(405, "method not allowed")
}

async fn stage(s: &Arc<Shared>, req: Request<Incoming>, rest: &[String]) -> Response<Body> {
    let method = req.method().clone();
    if rest.is_empty() {
        return if method == Method::GET {
            send(200, json!({ "now": now_ms(), "rooms": s.stage.list() }))
        } else {
            error(405, "method not allowed")
        };
    }
    let Some(room) = decode_component(&rest[0]).and_then(|r| clean_room(&r)) else {
        return error(400, "bad room");
    };
    let client = header(req.headers(), "x-chordwright-client");
    let user = user_of(req.headers());

    let mut body = json!({});
    if method == Method::PUT || method == Method::POST {
        let raw = match req.into_body().collect().await {
            Ok(b) => String::from_utf8_lossy(&b.to_bytes()).into_owned(),
            Err(e) => return error(400, &e.to_string()),
        };
        if !raw.is_empty() {
            match serde_json::from_str::<Value>(&raw) {
                Ok(v) if v.is_object() || v.is_array() => body = v,
                Ok(_) => return error(400, "body must be an object"),
                Err(_) => return error(400, "body is not JSON"),
            }
        }
    }
    // `body.name ?? userOf(req)`: a name in the body wins, even one that is no string.
    let name = match body.get("name") {
        None | Some(Value::Null) => user,
        Some(v) => v.as_str().map(String::from),
    };
    let (c, n) = (client.as_deref(), name.as_deref());

    match (rest.get(1).map(String::as_str), rest.len()) {
        (None, 1) if method == Method::GET => send(200, s.stage.read(&room)),
        (None, 1) if method == Method::PUT => answer(s.stage.publish(&room, c, body.get("state"))),
        (None, 1) => error(405, "method not allowed"),
        (Some("lead"), 2) if method == Method::POST => {
            answer(
                s.stage
                    .lead(&room, c, n, body.get("force") == Some(&Value::Bool(true))),
            )
        }
        (Some("lead"), 2) if method == Method::DELETE => answer(s.stage.release(&room, c)),
        (Some("here"), 2) if method == Method::POST => answer(s.stage.here(
            &room,
            c,
            n,
            body.get("attached") != Some(&Value::Bool(false)),
        )),
        (Some("here"), 2) if method == Method::DELETE => answer(s.stage.leave(&room, c)),
        (Some("lead" | "here"), 2) => error(405, "method not allowed"),
        _ => error(404, "not found"),
    }
}

async fn backups(s: &Arc<Shared>, req: Request<Incoming>, rest: &[String]) -> Response<Body> {
    let method = req.method().clone();
    let fail = |e: std::io::Error| {
        eprintln!("{e}");
        error(500, &e.to_string())
    };
    if rest.is_empty() && method == Method::GET {
        let all = blocking(s, |s| s.backups.list()).await;
        return send(
            200,
            json!({ "backups": all, "everyHours": number(s.backup_every), "keep": s.backup_keep }),
        );
    }
    if rest.is_empty() && method == Method::POST {
        return match blocking(s, |s| s.backups.create("manual")).await {
            Ok(entry) => {
                println!(
                    "Backup {} (manual): {} songs, {} sets",
                    entry.id, entry.songs, entry.sets
                );
                send(201, json!(entry))
            }
            Err(e) => fail(e),
        };
    }
    if rest.len() == 2 && rest[1] == "restore" && method == Method::POST {
        let id = decode_component(&rest[0]).unwrap_or_default();
        s.restoring_until.store(i64::MAX, Ordering::Relaxed);
        let result = blocking(s, move |s| s.backups.restore(&id)).await;
        // The watcher reports a little after the write; give it that long.
        s.restoring_until.store(now_ms() + 2_000, Ordering::Relaxed);
        let restored = match result {
            Ok(Some(r)) => r,
            Ok(None) => return error(404, "no such backup"),
            Err(e) => return fail(e),
        };
        let by = user_of(req.headers());
        for path in &restored.changed {
            if let Some((db, key)) = key_for_path(path) {
                s.journal.record(db, &key, by.as_deref(), "restore", false);
            }
        }
        s.journal
            .record("*", &restored.restored.id, by.as_deref(), "restore", true);
        println!(
            "Restored backup {}; what was there before is backup {}",
            restored.restored.id, restored.safety.id
        );
        // The watcher reports every file on its own; this tells each app that
        // the whole database moved, so it reads it again in one go.
        let client = header(req.headers(), "x-chordwright-client");
        for db in DATABASES {
            s.listeners.send(change_line(db, None, client.as_deref()));
        }
        return send(200, json!(restored));
    }
    if rest.len() == 1 && method == Method::DELETE {
        let id = decode_component(&rest[0]).unwrap_or_default();
        return match blocking(s, move |s| s.backups.remove(&id)).await {
            Ok(true) => respond(204, None, &[]),
            Ok(false) => error(404, "no such backup"),
            Err(e) => fail(e),
        };
    }
    if rest.len() > 2 {
        error(404, "not found")
    } else {
        error(405, "method not allowed")
    }
}
