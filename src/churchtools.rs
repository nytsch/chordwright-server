//! ChurchTools, through the server: dates, who serves at them, songs.
//!
//! A library can be given the address of a ChurchTools instance and a login
//! token of a ChurchTools user (`ChurchTools`, kept with the members in the
//! add-on's /data/instances.json). Like the assistant's key it stays here:
//! the app asks the server, the server asks ChurchTools with the token, and
//! what comes back is ChurchTools' own answer (`{ data, meta }`), passed on.
//! The token never goes to the app and never into a backup. Only GETs go
//! through, each route by name: what the app sends reaches ChurchTools only
//! as the ids and filters below — and, for `file`, as an address that must be
//! on this ChurchTools instance (the `fileUrl` of an arrangement's file), so
//! the token is never sent anywhere else.
//!
//! ```text
//!   GET /api/churchtools                 → { url, user }: whom the token belongs to — a connection check
//!   GET /api/churchtools/events?from=&to=&person=&page=&limit=
//!                                        → ChurchTools' /events with eventServices
//!   GET /api/churchtools/events/:id      → { event, agenda }: the date, who serves, the songs planned
//!   GET /api/churchtools/songs?q=&page=&limit=
//!                                        → ChurchTools' /songs with their arrangements
//!   GET /api/churchtools/songs/:id       → one song
//!   GET /api/churchtools/file?url=       → a file of an arrangement (ChordPro, SongBeamer, PDF…), as it is
//!   GET /api/churchtools/mine?from=&to=  → the dates the caller serves at
//! ```
//!
//! A musician is linked to a ChurchTools person by its id (`Member::churchtools`,
//! set in the web UI). Each service at a date then carries `member`: the id of
//! the musician who is that person — so the app can put the people of a date
//! into a set, and a musician find their dates (`mine`). `person=` keeps only
//! the dates a person serves at. Both look at the page ChurchTools returned.
//!
//! Who may: the owner and whoever leads (`Can::leads`) everything; any
//! musician linked to a person their own dates (`mine`). Otherwise 403
//! `leaders only`. Without ChurchTools 503 `churchtools off`; when ChurchTools
//! says no 502 `{ error: "upstream", reason: "token" | "failed" | "unreachable" }`,
//! and 404 when it has no such thing.

use std::time::Duration;

use bytes::Bytes;
use hyper::Request;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::outbound::{self, endpoint_of, Reply};
use crate::Member;

/// One request to ChurchTools.
const TIMEOUT: Duration = Duration::from_secs(30);
/// At most this much comes back, JSON or file.
const MAX_BYTES: usize = 16 << 20;
/// Pages: ChurchTools' own limit is the upper bound, this the server's.
const MAX_LIMIT: u32 = 500;

/// What a library talks to ChurchTools with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChurchTools {
    /// `https://<name>.church.tools`, without `/api` and without `/` at the end.
    pub url: String,
    /// A ChurchTools login token: sent as `Authorization: Login <token>`.
    pub token: String,
    /// Whom the token belonged to when it was saved, for the web UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl ChurchTools {
    /// Checked and cleaned; `user` stays empty until `whoami` says.
    pub fn new(url: &str, token: &str) -> Result<ChurchTools, String> {
        let url = clean_url(url)?;
        let token = token.trim();
        if !looks_like_token(token) {
            return Err("Das sieht nicht nach einem ChurchTools-Login-Token aus.".into());
        }
        Ok(ChurchTools {
            url,
            token: token.to_string(),
            user: None,
        })
    }

    /// `…abcd`, for the web UI.
    pub fn masked(&self) -> String {
        let tail: Vec<char> = self.token.chars().rev().take(4).collect();
        format!("…{}", tail.into_iter().rev().collect::<String>())
    }
}

/// Whether `token` could be a login token: one word, not too short or long.
pub fn looks_like_token(token: &str) -> bool {
    (8..=500).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

/// `feg-ettlingen.church.tools`, `https://…/api/`, … → `https://feg-ettlingen.church.tools`.
/// https only, but for this machine (for the tests).
pub fn clean_url(text: &str) -> Result<String, String> {
    let bad = || {
        format!(
            "„{}“ ist keine Adresse einer ChurchTools-Instanz.",
            text.trim()
        )
    };
    let mut url = text.trim().trim_end_matches('/').to_string();
    if url.is_empty()
        || url
            .chars()
            .any(|c| c.is_whitespace() || c == '?' || c == '#' || c == '@')
    {
        return Err(bad());
    }
    if let Some(rest) = url.strip_suffix("/api") {
        url = rest.to_string();
    }
    if !url.contains("://") {
        url = format!("https://{url}");
    }
    let (tls, host, port, prefix) = endpoint_of(&url).map_err(|_| bad())?;
    let local = host == "localhost" || host == "127.0.0.1";
    if !tls && !local {
        return Err("ChurchTools nur über https.".into());
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(bad());
    }
    let scheme = if tls { "https" } else { "http" };
    let authority = match (tls, port) {
        (true, 443) | (false, 80) => host.to_lowercase(),
        _ => format!("{}:{port}", host.to_lowercase()),
    };
    Ok(format!("{scheme}://{authority}{prefix}"))
}

/// What went wrong, as `{ status, body }` for the app.
#[derive(Debug)]
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

fn upstream(reason: &str) -> Refusal {
    Refusal {
        status: 502,
        body: json!({ "error": "upstream", "reason": reason }),
    }
}

/// What the app gets.
pub enum Answer {
    Json(Value),
    /// A file, as ChurchTools sent it.
    File {
        content_type: String,
        body: Bytes,
    },
}

/// `a b/ä` → `a%20b%2F%C3%A4`.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One GET to ChurchTools at `path` (from the instance's root), with the token.
async fn get(config: &ChurchTools, path: &str, json: bool) -> Result<Reply, Refusal> {
    let accept = if json { "application/json" } else { "*/*" };
    let request = Request::get("/")
        .header("authorization", format!("Login {}", config.token))
        .header("accept", accept)
        .header(
            "user-agent",
            concat!("chordwright-server/", env!("CARGO_PKG_VERSION")),
        );
    let reply = outbound::fetch(&config.url, request, path, Bytes::new(), TIMEOUT, MAX_BYTES)
        .await
        .map_err(|e| {
            eprintln!("churchtools: {e}");
            upstream("unreachable")
        })?;
    match reply.status {
        200..=299 => Ok(reply),
        401 | 403 => Err(upstream("token")),
        404 => Err(refuse(404, "not found")),
        status => {
            eprintln!("churchtools: {path}: {status}");
            Err(upstream("failed"))
        }
    }
}

async fn get_json(config: &ChurchTools, path: &str) -> Result<Value, Refusal> {
    let reply = get(config, path, true).await?;
    match reply.json() {
        Value::Null => Err(upstream("failed")),
        v => Ok(v),
    }
}

/// Whom the token belongs to: ChurchTools' person (`id`, `firstName`, `lastName`, …).
pub async fn whoami(config: &ChurchTools) -> Result<Value, Refusal> {
    let me = get_json(config, "/api/whoami").await?;
    let me = me["data"].clone();
    // Without a login ChurchTools answers as the anonymous user, id -1.
    if me["id"].as_i64().is_none_or(|id| id <= 0) {
        return Err(upstream("token"));
    }
    Ok(me)
}

/// `Vorname Nachname` of a person as ChurchTools has it.
pub fn person_name(p: &Value) -> String {
    let name = format!(
        "{} {}",
        p["firstName"].as_str().unwrap_or(""),
        p["lastName"].as_str().unwrap_or("")
    );
    let name = name.trim();
    if name.is_empty() {
        format!("#{}", p["id"])
    } else {
        name.to_string()
    }
}

/// What a person (by ChurchTools id) is called in the band.
fn members_by_person(members: &[Member]) -> impl Fn(i64) -> Option<String> + Sync + '_ {
    move |person| {
        members
            .iter()
            .find(|m| {
                m.churchtools
                    .is_some_and(|p| i64::try_from(p) == Ok(person))
            })
            .map(|m| m.id.clone())
    }
}

/// The services of one event, each with `member` where a musician is that person.
fn mark_services(event: &mut Value, member_of: &(dyn Fn(i64) -> Option<String> + Sync)) {
    for service in event["eventServices"].as_array_mut().into_iter().flatten() {
        if let Some(id) = service["personId"].as_i64().and_then(member_of) {
            service["member"] = json!(id);
        }
    }
}

fn serves(event: &Value, person: i64) -> bool {
    event["eventServices"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["personId"].as_i64() == Some(person))
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn is_id(s: &str) -> bool {
    (1..=12).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// `from`, `to`, `page`, `limit` from the app's query, checked, as ChurchTools takes them.
fn paging(param: &dyn Fn(&str) -> Option<String>, dates: bool) -> Result<String, Refusal> {
    let mut out = String::new();
    if dates {
        for name in ["from", "to"] {
            if let Some(d) = param(name) {
                if !is_date(&d) {
                    return Err(refuse(400, "dates as YYYY-MM-DD"));
                }
                out.push_str(&format!("&{name}={d}"));
            }
        }
    }
    for name in ["page", "limit"] {
        if let Some(n) = param(name) {
            let n: u32 = n
                .parse()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| refuse(400, "page and limit are numbers"))?;
            let n = if name == "limit" { n.min(MAX_LIMIT) } else { n };
            out.push_str(&format!("&{name}={n}"));
        }
    }
    Ok(out)
}

/// The events between `from` and `to`, with who serves; `person`: only theirs.
async fn events(
    config: &ChurchTools,
    member_of: &(dyn Fn(i64) -> Option<String> + Sync),
    query: &str,
    person: Option<i64>,
) -> Result<Value, Refusal> {
    let mut answer = get_json(
        config,
        &format!("/api/events?include%5B%5D=eventServices{query}"),
    )
    .await?;
    if let Some(list) = answer["data"].as_array_mut() {
        if let Some(person) = person {
            list.retain(|e| serves(e, person));
        }
        for event in list.iter_mut() {
            mark_services(event, member_of);
        }
    }
    Ok(answer)
}

/// One request of the app to `/api/churchtools/<parts>`. `caller`: None the
/// owner, else the musician.
pub async fn answer(
    config: Option<ChurchTools>,
    members: &[Member],
    caller: Option<&Member>,
    parts: &[String],
    query: &[(String, String)],
) -> Result<Answer, Refusal> {
    let param = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let config = config.ok_or_else(|| refuse(503, "churchtools off"))?;
    let part = |i: usize| parts.get(i).map(String::as_str);
    let member_of = members_by_person(members);
    let leads = caller.is_none_or(|m| m.can.leads());
    if part(0) == Some("mine") && parts.len() == 1 {
        let person = caller
            .and_then(|m| m.churchtools)
            .and_then(|p| i64::try_from(p).ok())
            .ok_or_else(|| refuse(404, "no person"))?;
        let query = paging(&param, true)?;
        return events(&config, &member_of, &query, Some(person))
            .await
            .map(Answer::Json);
    }
    if !leads {
        return Err(refuse(403, "leaders only"));
    }
    let json = match (part(0), part(1), parts.len()) {
        (None, _, 0) => {
            let me = whoami(&config).await?;
            json!({ "url": config.url, "user": me })
        }
        (Some("events"), None, 1) => {
            let person = match param("person") {
                Some(p) if is_id(&p) => p.parse().ok(),
                Some(_) => return Err(refuse(400, "bad person")),
                None => None,
            };
            let query = paging(&param, true)?;
            events(&config, &member_of, &query, person).await?
        }
        (Some("events"), Some(id), 2) if is_id(id) => {
            let mut event = get_json(
                &config,
                &format!("/api/events/{id}?include%5B%5D=eventServices"),
            )
            .await?;
            mark_services(&mut event["data"], &member_of);
            // Not every date has an agenda; then there is none.
            let agenda = match get_json(&config, &format!("/api/events/{id}/agenda")).await {
                Ok(a) => a["data"].clone(),
                Err(no) if no.status == 404 => Value::Null,
                Err(no) => return Err(no),
            };
            json!({ "event": event["data"], "agenda": agenda })
        }
        (Some("songs"), None, 1) => {
            let mut query = paging(&param, false)?;
            if let Some(q) = param("q").filter(|q| !q.trim().is_empty()) {
                if q.len() > 200 {
                    return Err(refuse(400, "query too long"));
                }
                query.push_str(&format!("&name={}", enc(q.trim())));
            }
            let query = query.strip_prefix('&').unwrap_or(&query);
            let path = if query.is_empty() {
                "/api/songs".to_string()
            } else {
                format!("/api/songs?{query}")
            };
            get_json(&config, &path).await?
        }
        (Some("songs"), Some(id), 2) if is_id(id) => {
            get_json(&config, &format!("/api/songs/{id}")).await?
        }
        (Some("file"), None, 1) => {
            let url = param("url").ok_or_else(|| refuse(400, "url missing"))?;
            // Only files of this instance: the token goes nowhere else.
            let path = url
                .strip_prefix(&config.url)
                .filter(|rest| rest.starts_with('/'))
                .filter(|_| !url.contains(['\r', '\n', ' ', '#']))
                .ok_or_else(|| refuse(400, "not a file of this ChurchTools"))?;
            let reply = get(&config, path, false).await?;
            return Ok(Answer::File {
                content_type: reply
                    .content_type
                    .unwrap_or_else(|| "application/octet-stream".into()),
                body: reply.body,
            });
        }
        _ => return Err(refuse(404, "not found")),
    };
    Ok(Answer::Json(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let ok = |s: &str| clean_url(s).unwrap();
        assert_eq!(
            ok("feg-ettlingen.church.tools"),
            "https://feg-ettlingen.church.tools"
        );
        assert_eq!(
            ok(" https://FEG-Ettlingen.church.tools/api/ "),
            "https://feg-ettlingen.church.tools"
        );
        assert_eq!(ok("https://example.org/ct/"), "https://example.org/ct");
        assert_eq!(ok("http://127.0.0.1:9"), "http://127.0.0.1:9");
        assert!(clean_url("http://feg-ettlingen.church.tools").is_err());
        assert!(clean_url("https://evil@feg.church.tools").is_err());
        assert!(clean_url("https://x.church.tools/?q=1").is_err());
        assert!(clean_url("").is_err());
    }

    #[test]
    fn tokens_and_names() {
        assert!(ChurchTools::new("feg-ettlingen.church.tools", "kurz").is_err());
        assert!(ChurchTools::new("feg-ettlingen.church.tools", "mit leerzeichen drin").is_err());
        let ct = ChurchTools::new("feg-ettlingen.church.tools", " AbCdEf123456wxyz ").unwrap();
        assert_eq!(ct.token, "AbCdEf123456wxyz");
        assert_eq!(ct.masked(), "…wxyz");
        assert_eq!(
            person_name(&json!({ "id": 7, "firstName": "Anna", "lastName": "Berg" })),
            "Anna Berg"
        );
        assert_eq!(person_name(&json!({ "id": 7 })), "#7");
    }

    #[test]
    fn queries() {
        assert_eq!(
            enc("Herr, ich komme / ä"),
            "Herr%2C%20ich%20komme%20%2F%20%C3%A4"
        );
        assert!(is_date("2026-12-24") && !is_date("24.12.2026") && !is_date("2026-12-2x"));
        let given = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            paging(&given(&[("from", "2026-12-01"), ("limit", "9999")]), true).unwrap(),
            "&from=2026-12-01&limit=500"
        );
        assert_eq!(
            paging(&given(&[("to", "morgen")]), true)
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(
            paging(&given(&[("page", "0")]), false).unwrap_err().status,
            400
        );
    }
}
