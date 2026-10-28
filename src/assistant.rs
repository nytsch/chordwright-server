//! The AI assistant of "Plan the running order", through the server.
//!
//! The app used to call Anthropic from the device with a key of its own; a
//! limit set by whoever runs the library could not hold, since anyone with
//! the key can call the API past the app. Here the key stays on the server
//! (`Assistant`, kept with the members in the add-on's /data/instances.json
//! and never in an answer or a backup), and so does the decision what a
//! request may cost: the app sends only the system text and the history.
//!
//! ```text
//!   POST /api/assistant  → body { kind: "form" | "length", system, messages }
//!                        → { content, stop_reason, usage: { input, output, searches }, quota: { used, limit } }
//!                          429 quota · 403 no assistant · 503 assistant off · 502 upstream { reason }
//! ```
//!
//! `form` is a question in the chat and counts against the daily limit:
//! a musician's own (`Member::ai`) or the library's (`Assistant::daily`),
//! the owner's `owner_daily` (None: no limit). `length` asks for a
//! recording's length — small, fixed in size, and with a cap of its own
//! (`LENGTH_DAILY`) so it cannot stand in for the chat. A question counts
//! once an answer came; one that failed before costs nothing of the quota.
//! Days are UTC days.
//!
//! Every request, counted or not, goes into `.chordwright/assistant.jsonl`
//! with what it used — for the web UI's figures, and to know today's counts
//! again after a restart.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;

use crate::util::{iso, now_ms};

pub const MODEL: &str = "claude-opus-5-5";
/// How often a paused turn (`pause_turn`) is carried on at most.
const MAX_CONTINUES: usize = 3;
/// Recording lengths a day, for anyone.
pub const LENGTH_DAILY: u32 = 20;
/// One call to Anthropic, web search included.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
const ENDPOINT: &str = "https://api.anthropic.com";
const LOG: &str = "assistant.jsonl";

fn two() -> u32 {
    2
}

/// What a library's assistant runs with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assistant {
    /// An Anthropic API key, `sk-ant-…`. Best one of its own per library,
    /// from a workspace with a spend limit.
    pub key: String,
    /// Questions a day for a musician without a limit of their own.
    #[serde(default = "two")]
    pub daily: u32,
    /// The owner's; None: as many as they like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_daily: Option<u32>,
    /// Somewhere else than api.anthropic.com — for the tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl Assistant {
    /// `sk-ant-…abcd`, for the web UI.
    pub fn masked(&self) -> String {
        let tail: String = self
            .key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("sk-ant-…{tail}")
    }
}

/// Whether `key` looks like an Anthropic API key.
pub fn looks_like_key(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("sk-ant-")
        && (20..=300).contains(&key.len())
        && !key.chars().any(char::is_whitespace)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Form,
    Length,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        match s {
            "form" => Some(Kind::Form),
            "length" => Some(Kind::Length),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Form => "form",
            Kind::Length => "length",
        }
    }

    /// max_tokens, web searches, effort — the server's, not the app's.
    fn size(self) -> (u32, u32, &'static str) {
        match self {
            Kind::Form => (16000, 5, "medium"),
            Kind::Length => (2000, 2, "low"),
        }
    }
}

/// Who asks: `owner` or a musician's id.
#[derive(Clone, Debug)]
pub struct Asker {
    pub id: String,
    pub name: Option<String>,
    /// Questions (`form`) a day; None: no limit.
    pub limit: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub searches: u64,
}

impl Usage {
    fn add(&mut self, u: &Value) {
        let n = |v: &Value| v.as_u64().unwrap_or(0);
        self.input += n(&u["input_tokens"])
            + n(&u["cache_creation_input_tokens"])
            + n(&u["cache_read_input_tokens"]);
        self.output += n(&u["output_tokens"]);
        self.searches += n(&u["server_tool_use"]["web_search_requests"]);
    }
}

/// One line of the log.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Line {
    at: String,
    who: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    kind: String,
    /// An answer came, and for `form` it counted.
    counted: bool,
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    searches: u64,
}

/// Today's counts, and the requests under way.
#[derive(Default)]
struct Day {
    /// `YYYY-MM-DD`.
    day: String,
    used: HashMap<(String, Kind), u32>,
    pending: HashMap<(String, Kind), u32>,
}

impl Day {
    fn roll(&mut self, today: &str) {
        if self.day != today {
            self.day = today.to_string();
            self.used.clear();
            // Under way stays under way: they end on the new day.
        }
    }
}

/// The assistant of one library: its settings, the counts, the log.
pub struct Desk {
    config: Mutex<Option<Assistant>>,
    day: Mutex<Day>,
    log: PathBuf,
}

/// What a figure in the web UI shows for one asker.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Tally {
    pub who: String,
    pub name: Option<String>,
    pub today: u32,
    pub month: u32,
    /// This month, every kind.
    pub usage: Usage,
}

fn today() -> String {
    iso(now_ms())[..10].to_string()
}

impl Desk {
    pub fn open(root: &Path) -> Desk {
        let log = root.join(".chordwright").join(LOG);
        let mut day = Day {
            day: today(),
            ..Day::default()
        };
        for line in read_log(&log) {
            if line.counted && line.at.starts_with(&day.day) {
                if let Some(kind) = Kind::parse(&line.kind) {
                    *day.used.entry((line.who, kind)).or_default() += 1;
                }
            }
        }
        Desk {
            config: Mutex::new(None),
            day: Mutex::new(day),
            log,
        }
    }

    pub fn config(&self) -> Option<Assistant> {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set(&self, config: Option<Assistant>) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) =
            config.filter(|c| !c.key.is_empty());
    }

    /// Questions used today, and the limit.
    pub fn quota(&self, who: &Asker) -> Value {
        let mut day = self.day.lock().unwrap_or_else(|e| e.into_inner());
        day.roll(&today());
        let used = day
            .used
            .get(&(who.id.clone(), Kind::Form))
            .copied()
            .unwrap_or(0);
        json!({ "used": used, "limit": who.limit })
    }

    /// This month's figures per asker, most questions first.
    pub fn tally(&self) -> Vec<Tally> {
        let today = today();
        let month = &today[..7];
        let mut by: HashMap<String, Tally> = HashMap::new();
        for line in read_log(&self.log) {
            if !line.at.starts_with(month) {
                continue;
            }
            let t = by.entry(line.who.clone()).or_insert_with(|| Tally {
                who: line.who.clone(),
                ..Tally::default()
            });
            if line.name.is_some() {
                t.name = line.name.clone();
            }
            if line.counted && line.kind == "form" {
                t.month += 1;
                if line.at.starts_with(&today) {
                    t.today += 1;
                }
            }
            t.usage.input += line.input;
            t.usage.output += line.output;
            t.usage.searches += line.searches;
        }
        let mut all: Vec<Tally> = by.into_values().collect();
        all.sort_by(|a, b| b.month.cmp(&a.month).then(a.who.cmp(&b.who)));
        all
    }

    /// Holds a place for one request, or says why not.
    fn reserve(&self, who: &Asker, kind: Kind) -> Result<(), Value> {
        let limit = match kind {
            Kind::Form => who.limit,
            Kind::Length => Some(LENGTH_DAILY),
        };
        let mut day = self.day.lock().unwrap_or_else(|e| e.into_inner());
        day.roll(&today());
        let slot = (who.id.clone(), kind);
        let used = day.used.get(&slot).copied().unwrap_or(0);
        let pending = day.pending.get(&slot).copied().unwrap_or(0);
        if limit.is_some_and(|l| used + pending >= l) {
            return Err(json!({ "used": used, "limit": limit }));
        }
        *day.pending.entry(slot).or_default() += 1;
        Ok(())
    }

    fn settle(&self, who: &Asker, kind: Kind, counted: bool, usage: Usage) {
        {
            let mut day = self.day.lock().unwrap_or_else(|e| e.into_inner());
            day.roll(&today());
            let slot = (who.id.clone(), kind);
            if let Some(p) = day.pending.get_mut(&slot) {
                *p = p.saturating_sub(1);
            }
            if counted {
                *day.used.entry(slot).or_default() += 1;
            }
        }
        let line = Line {
            at: iso(now_ms()),
            who: who.id.clone(),
            name: who.name.clone(),
            kind: kind.id().into(),
            counted,
            input: usage.input,
            output: usage.output,
            searches: usage.searches,
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.log.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)?;
            writeln!(file, "{}", serde_json::to_string(&line).unwrap_or_default())
        };
        if let Err(e) = write() {
            eprintln!("{}: {e}", self.log.display());
        }
    }
}

fn read_log(path: &Path) -> Vec<Line> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// What went wrong, as `{ status, body }` for the app.
pub struct Refusal {
    pub status: u16,
    pub body: Value,
}

fn refuse(status: u16, error: &str) -> Refusal {
    Refusal {
        status,
        body: json!({ "error": error }),
    }
}

/// What the app sent, checked: a known kind, a system text, a history that
/// ends with the user's turn.
fn read_request(body: &Value) -> Result<(Kind, String, Vec<Value>), Refusal> {
    let kind = body["kind"]
        .as_str()
        .and_then(Kind::parse)
        .ok_or_else(|| refuse(400, "bad kind"))?;
    let system = body["system"]
        .as_str()
        .filter(|s| s.len() <= 50_000)
        .ok_or_else(|| refuse(400, "bad system"))?;
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| refuse(400, "bad messages"))?;
    let roles_ok = messages
        .iter()
        .all(|m| matches!(m["role"].as_str(), Some("user" | "assistant")));
    if messages.is_empty()
        || messages.len() > 200
        || !roles_ok
        || messages.last().and_then(|m| m["role"].as_str()) != Some("user")
    {
        return Err(refuse(400, "bad messages"));
    }
    Ok((kind, system.to_string(), messages.clone()))
}

/// Why Anthropic said no, in the app's words (`AssistantFailure`).
fn reason_of(status: u16, body: &Value) -> &'static str {
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();
    match status {
        401 | 403 => "key",
        429 | 529 => "limit",
        400 if message.contains("credit") || message.contains("billing") => "limit",
        _ => "failed",
    }
}

/// Asks: the whole turn, paused turns carried on, counted and logged.
pub async fn ask(desk: Arc<Desk>, who: Asker, body: Value) -> Result<Value, Refusal> {
    let Some(config) = desk.config() else {
        return Err(refuse(503, "assistant off"));
    };
    let (kind, system, mut messages) = read_request(&body)?;
    if kind == Kind::Form && who.limit == Some(0) {
        return Err(refuse(403, "no assistant"));
    }
    if let Err(quota) = desk.reserve(&who, kind) {
        return Err(Refusal {
            status: 429,
            body: json!({ "error": "quota", "quota": quota }),
        });
    }
    let (max_tokens, searches, effort) = kind.size();
    let mut content: Vec<Value> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = Value::Null;
    let mut answered = false;
    let mut failure: Option<&'static str> = None;
    for _ in 0..=MAX_CONTINUES {
        let request = json!({
            "model": MODEL,
            "max_tokens": max_tokens,
            "system": system,
            "messages": messages,
            "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": searches }],
            "output_config": { "effort": effort },
            "fallbacks": "default",
        });
        match call(&config, &request).await {
            Ok((200, reply)) => {
                answered = true;
                usage.add(&reply["usage"]);
                let blocks = reply["content"].as_array().cloned().unwrap_or_default();
                content.extend(blocks);
                stop = reply["stop_reason"].clone();
                if stop.as_str() != Some("pause_turn") {
                    break;
                }
                // The same turn goes on: the answer so far is the last message.
                if messages.last().and_then(|m| m["role"].as_str()) == Some("assistant") {
                    messages.pop();
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            Ok((status, reply)) => {
                failure = Some(reason_of(status, &reply));
                break;
            }
            Err(e) => {
                eprintln!("assistant: {e}");
                failure = Some("failed");
                break;
            }
        }
    }
    let counted = answered && failure.is_none();
    desk.settle(&who, kind, counted, usage);
    let quota = desk.quota(&who);
    match failure {
        Some(reason) => Err(Refusal {
            status: 502,
            body: json!({ "error": "upstream", "reason": reason, "quota": quota }),
        }),
        None => Ok(json!({
            "content": content,
            "stop_reason": stop,
            "usage": usage,
            "quota": quota,
        })),
    }
}

/// `scheme://host:port/prefix`, taken apart.
fn endpoint_of(url: &str) -> Result<(bool, String, u16, String), String> {
    let (tls, rest) = match url.split_once("://") {
        Some(("https", rest)) => (true, rest),
        Some(("http", rest)) => (false, rest),
        _ => return Err(format!("{url}: not an http(s) address")),
    };
    let (authority, prefix) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("{url}: bad port"))?),
        None => (authority, if tls { 443 } else { 80 }),
    };
    let prefix = prefix.trim_end_matches('/');
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("/{prefix}")
    };
    Ok((tls, host.to_string(), port, prefix))
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    });
    tokio_rustls::TlsConnector::from(config.clone())
}

/// One POST to /v1/messages: the status and the JSON that came back.
async fn call(config: &Assistant, body: &Value) -> Result<(u16, Value), String> {
    let (tls, host, port, prefix) = endpoint_of(config.endpoint.as_deref().unwrap_or(ENDPOINT))?;
    let request = Request::post(format!("{prefix}/v1/messages"))
        .header("host", &host)
        .header("x-api-key", &config.key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .map_err(|e| e.to_string())?;
    let work = async {
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| e.to_string())?;
        let response = if tls {
            let name =
                rustls_pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
            let stream = tls_connector()
                .connect(name, stream)
                .await
                .map_err(|e| e.to_string())?;
            send(TokioIo::new(stream), request).await?
        } else {
            send(TokioIo::new(stream), request).await?
        };
        Ok::<_, String>(response)
    };
    tokio::time::timeout(CALL_TIMEOUT, work)
        .await
        .map_err(|_| "timed out".to_string())?
}

async fn send<T>(io: TokioIo<T>, request: Request<Full<Bytes>>) -> Result<(u16, Value), String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes();
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(
            endpoint_of("https://api.anthropic.com").unwrap(),
            (true, "api.anthropic.com".into(), 443, String::new())
        );
        assert_eq!(
            endpoint_of("http://127.0.0.1:9/x/").unwrap(),
            (false, "127.0.0.1".into(), 9, "/x".into())
        );
        assert!(endpoint_of("ftp://x").is_err());
    }

    #[test]
    fn keys_and_reasons() {
        assert!(looks_like_key(" sk-ant-api03-abcdefghijklmnop "));
        assert!(!looks_like_key("sk-abc"));
        assert_eq!(reason_of(401, &Value::Null), "key");
        assert_eq!(
            reason_of(
                400,
                &json!({ "error": { "message": "Your credit balance is too low" } })
            ),
            "limit"
        );
        assert_eq!(reason_of(500, &Value::Null), "failed");
        let a = Assistant {
            key: "sk-ant-api03-xyzw1234".into(),
            daily: 2,
            owner_daily: None,
            endpoint: None,
        };
        assert_eq!(a.masked(), "sk-ant-…1234");
    }

    #[test]
    fn requests_are_checked() {
        let ok = json!({ "kind": "form", "system": "S", "messages": [{ "role": "user", "content": "A" }] });
        assert!(read_request(&ok).is_ok());
        let ends_with_ai = json!({ "kind": "form", "system": "S", "messages": [{ "role": "assistant", "content": "A" }] });
        assert_eq!(read_request(&ends_with_ai).err().unwrap().status, 400);
        let bad_kind = json!({ "kind": "chat", "system": "S", "messages": [{ "role": "user", "content": "A" }] });
        assert_eq!(read_request(&bad_kind).err().unwrap().status, 400);
    }
}
