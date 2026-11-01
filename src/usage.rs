//! Who used a library, when, and how much: for the add-on's web UI, so the
//! person who runs the server sees who reaches the songs and what that costs
//! the machine.
//!
//! ```text
//!   .chordwright/usage.json
//!     hours     { "<hour>": { "<who>": { requests, kinds, busyUs, bytes } } }   the last 31 days
//!     people    { "<who>": { name, role, first, last } }
//!     visits    [{ who, name, role, from, to, requests, kinds }]                  newest first
//!     names     { "<client id>": "<name>" }
//! ```
//!
//! `<hour>` is hours since the epoch. `<who>` is `m:<id>` for a musician,
//! `o:<name>` for the library's token, `g:<name>` for a browser paired with a
//! code and `x:` for a request turned away. The library's token is shared by
//! every device of its owner; the name is the one the app sends with its
//! writes (`X-Chordwright-User`), remembered per app (`X-Chordwright-Client`)
//! so its reads count for the same person.
//!
//! Only the add-on's page reads this; no route serves it. Kept in memory and
//! written once a minute when something changed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::journal::clean_name;
use crate::util::{now_ms, pretty, write_atomic};

const DIR: &str = ".chordwright";
const FILE: &str = "usage.json";
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
/// How far back the hourly figures go.
pub const KEEP_DAYS: i64 = 31;
/// Requests of one person less than this apart are one visit.
const VISIT_GAP_MS: i64 = 15 * 60 * 1000;
const KEEP_VISITS: usize = 300;
const KEEP_NAMES: usize = 200;

/// What a request was for.
pub const KINDS: [&str; 8] = [
    "read",
    "write",
    "live",
    "stage",
    "assistant",
    "backups",
    "check",
    "denied",
];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Tally {
    pub requests: u64,
    pub kinds: BTreeMap<String, u64>,
    /// Time spent answering, in microseconds.
    pub busy_us: u64,
    /// Request and answer bodies, as far as their size is known.
    pub bytes: u64,
}

impl Tally {
    fn add(&mut self, kind: &str, busy_us: u64, bytes: u64) {
        self.requests += 1;
        *self.kinds.entry(kind.to_string()).or_default() += 1;
        self.busy_us += busy_us;
        self.bytes += bytes;
    }

    fn merge(&mut self, other: &Tally) {
        self.requests += other.requests;
        for (k, n) in &other.kinds {
            *self.kinds.entry(k.clone()).or_default() += n;
        }
        self.busy_us += other.busy_us;
        self.bytes += other.bytes;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct Person {
    name: String,
    role: String,
    first: i64,
    last: i64,
}

/// One visit: a person's requests with no long pause between them.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Visit {
    pub who: String,
    pub name: String,
    /// `owner`, `guest`, `member`, `denied`.
    pub role: String,
    /// ms since the epoch.
    pub from: i64,
    pub to: i64,
    pub requests: u64,
    pub kinds: BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct State {
    hours: BTreeMap<i64, BTreeMap<String, Tally>>,
    people: BTreeMap<String, Person>,
    visits: Vec<Visit>,
    names: BTreeMap<String, String>,
    #[serde(skip)]
    dirty: bool,
}

/// Who made a request, before the name is known.
#[derive(Clone, Debug, PartialEq)]
pub enum Caller {
    /// The library's token (or none needed).
    Owner,
    /// A browser paired with a code.
    Guest,
    /// A musician: id and name.
    Member(String, String),
    /// Turned away: no or a wrong token.
    Denied,
}

/// One person in a report.
#[derive(Clone, Debug, PartialEq)]
pub struct UsagePerson {
    pub who: String,
    pub name: String,
    pub role: String,
    pub last: i64,
    pub tally: Tally,
}

/// What the page shows for a span of days.
#[derive(Clone, Debug, Default)]
pub struct UsageReport {
    pub days: u32,
    pub total: Tally,
    /// Most requests first.
    pub people: Vec<UsagePerson>,
    /// Requests per day, oldest first; the last one ends now.
    pub daily: Vec<u64>,
    /// Requests per hour of the last 24, oldest first; the last one ends now.
    pub hourly: Vec<u64>,
    /// Newest first.
    pub visits: Vec<Visit>,
}

pub struct Usage {
    dir: PathBuf,
    state: Mutex<State>,
}

impl Usage {
    pub fn open(root: &Path) -> Self {
        let dir = root.join(DIR);
        let state = std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<State>(&text).ok())
            .unwrap_or_default();
        Usage {
            dir,
            state: Mutex::new(state),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Note one request. `name`: what the app sent in `X-Chordwright-User`,
    /// `client`: its `X-Chordwright-Client`.
    pub fn record(
        &self,
        caller: &Caller,
        name: Option<&str>,
        client: Option<&str>,
        kind: &str,
        busy_us: u64,
        bytes: u64,
    ) {
        self.record_at(now_ms(), caller, name, client, kind, busy_us, bytes);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_at(
        &self,
        now: i64,
        caller: &Caller,
        name: Option<&str>,
        client: Option<&str>,
        kind: &str,
        busy_us: u64,
        bytes: u64,
    ) {
        let name = clean_name(name);
        let client = client.filter(|c| !c.is_empty() && c.len() <= 100);
        let mut state = self.state();
        // A name sent with a write stands for that app's reads too.
        let name = match (name, client) {
            (Some(n), Some(c)) => {
                if state.names.get(c) != Some(&n) {
                    if state.names.len() >= KEEP_NAMES && !state.names.contains_key(c) {
                        state.names.pop_first();
                    }
                    state.names.insert(c.to_string(), n.clone());
                }
                Some(n)
            }
            (Some(n), None) => Some(n),
            (None, Some(c)) => state.names.get(c).cloned(),
            (None, None) => None,
        };
        let (who, shown, role) = match caller {
            Caller::Member(id, n) => (format!("m:{id}"), n.clone(), "member"),
            Caller::Owner => (
                format!("o:{}", name.as_deref().unwrap_or("")),
                name.unwrap_or_else(|| "Ohne Namen".into()),
                "owner",
            ),
            Caller::Guest => (
                format!("g:{}", name.as_deref().unwrap_or("")),
                name.unwrap_or_else(|| "Browser mit Code".into()),
                "guest",
            ),
            Caller::Denied => ("x:".into(), "Abgewiesen".into(), "denied"),
        };

        let hour = now.div_euclid(HOUR_MS);
        if !state.hours.contains_key(&hour) {
            let oldest = hour - KEEP_DAYS * 24;
            state.hours.retain(|h, _| *h > oldest);
        }
        state
            .hours
            .entry(hour)
            .or_default()
            .entry(who.clone())
            .or_default()
            .add(kind, busy_us, bytes);

        let person = state.people.entry(who.clone()).or_insert_with(|| Person {
            first: now,
            ..Person::default()
        });
        person.name = shown.clone();
        person.role = role.into();
        person.last = now;

        let open = state
            .visits
            .iter()
            .position(|v| v.who == who && now - v.to < VISIT_GAP_MS);
        match open {
            Some(i) => {
                let mut v = state.visits.remove(i);
                v.to = now;
                v.name = shown;
                v.requests += 1;
                *v.kinds.entry(kind.into()).or_default() += 1;
                state.visits.insert(0, v);
            }
            None => {
                state.visits.insert(
                    0,
                    Visit {
                        who,
                        name: shown,
                        role: role.into(),
                        from: now,
                        to: now,
                        requests: 1,
                        kinds: BTreeMap::from([(kind.to_string(), 1)]),
                    },
                );
                state.visits.truncate(KEEP_VISITS);
            }
        }
        state.dirty = true;
    }

    /// The last `days` days, up to now.
    pub fn report(&self, days: u32) -> UsageReport {
        self.report_at(now_ms(), days)
    }

    fn report_at(&self, now: i64, days: u32) -> UsageReport {
        let days = days.clamp(1, KEEP_DAYS as u32);
        let state = self.state();
        let this_hour = now.div_euclid(HOUR_MS);
        let first_hour = this_hour - days as i64 * 24 + 1;
        let mut total = Tally::default();
        let mut by: BTreeMap<String, Tally> = BTreeMap::new();
        let mut daily = vec![0; days as usize];
        let mut hourly = vec![0; 24];
        for (hour, people) in state.hours.range(first_hour..=this_hour) {
            let back = (this_hour - hour) as usize;
            let sum: u64 = people.values().map(|t| t.requests).sum();
            daily[days as usize - 1 - back / 24] += sum;
            if back < 24 {
                hourly[23 - back] += sum;
            }
            for (who, t) in people {
                total.merge(t);
                by.entry(who.clone()).or_default().merge(t);
            }
        }
        let mut people: Vec<UsagePerson> = by
            .into_iter()
            .map(|(who, tally)| {
                let p = state.people.get(&who).cloned().unwrap_or_default();
                UsagePerson {
                    name: if p.name.is_empty() {
                        who.clone()
                    } else {
                        p.name
                    },
                    role: p.role,
                    last: p.last,
                    who,
                    tally,
                }
            })
            .collect();
        people.sort_by(|a, b| {
            b.tally
                .requests
                .cmp(&a.tally.requests)
                .then(b.last.cmp(&a.last))
        });
        let since = now - days as i64 * DAY_MS;
        let visits = state
            .visits
            .iter()
            .filter(|v| v.to >= since)
            .cloned()
            .collect();
        UsageReport {
            days,
            total,
            people,
            daily,
            hourly,
            visits,
        }
    }

    /// Writes what was recorded since the last write, if anything was.
    pub fn flush(&self) {
        let text = {
            let mut state = self.state();
            if !state.dirty {
                return;
            }
            state.dirty = false;
            pretty(&json!(&*state))
        };
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|_| write_atomic(&self.dir.join(FILE), text.as_bytes()));
        if let Err(err) = written {
            eprintln!("Could not write the usage figures: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!(
            "cw-usage-{}-{}-{}",
            std::process::id(),
            now_ms(),
            rand_suffix()
        ))
    }

    fn rand_suffix() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn names_follow_the_app_and_visits_merge() {
        let root = temp();
        let usage = Usage::open(&root);
        let t = 1_700_000_000_000;
        // A read before any write: the app is not known by name yet.
        usage.record_at(t, &Caller::Owner, None, Some("c1"), "read", 100, 10);
        usage.record_at(
            t + 1000,
            &Caller::Owner,
            Some("Anna"),
            Some("c1"),
            "write",
            200,
            20,
        );
        usage.record_at(t + 2000, &Caller::Owner, None, Some("c1"), "read", 100, 10);
        let bob = Caller::Member("b1".into(), "Bob".into());
        usage.record_at(t + 3000, &bob, None, None, "live", 50, 0);
        usage.record_at(t + 4000, &Caller::Denied, None, None, "denied", 10, 0);
        // Much later: a new visit of Anna's.
        usage.record_at(
            t + 3 * HOUR_MS,
            &Caller::Owner,
            None,
            Some("c1"),
            "read",
            100,
            10,
        );

        let r = usage.report_at(t + 3 * HOUR_MS, 7);
        assert_eq!(r.total.requests, 6);
        assert_eq!(r.total.busy_us, 560);
        assert_eq!(r.total.kinds["read"], 3);
        let names: Vec<(&str, u64)> = r
            .people
            .iter()
            .map(|p| (p.name.as_str(), p.tally.requests))
            .collect();
        assert_eq!(
            names,
            [
                ("Anna", 3),
                ("Abgewiesen", 1),
                ("Bob", 1),
                ("Ohne Namen", 1)
            ]
        );
        assert_eq!(
            r.people.iter().find(|p| p.name == "Bob").unwrap().role,
            "member"
        );
        assert_eq!(r.daily.iter().sum::<u64>(), 6);
        assert_eq!(*r.daily.last().unwrap(), 6);
        assert_eq!(r.hourly.iter().sum::<u64>(), 6);
        assert_eq!(r.hourly[23], 1);
        let visits: Vec<(&str, u64)> = r
            .visits
            .iter()
            .map(|v| (v.name.as_str(), v.requests))
            .collect();
        assert_eq!(
            visits,
            [
                ("Anna", 1),
                ("Abgewiesen", 1),
                ("Bob", 1),
                ("Anna", 2),
                ("Ohne Namen", 1)
            ]
        );

        // A day later only the newest falls into a one-day report.
        let later = usage.report_at(t + 25 * HOUR_MS, 1);
        assert_eq!(later.total.requests, 1);

        usage.flush();
        let again = Usage::open(&root).report_at(t + 3 * HOUR_MS, 7);
        assert_eq!(again.total, r.total);
        assert_eq!(again.visits.len(), r.visits.len());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_hours_go() {
        let root = temp();
        let usage = Usage::open(&root);
        let t = 1_700_000_000_000;
        usage.record_at(t, &Caller::Guest, None, None, "read", 1, 1);
        usage.record_at(
            t + (KEEP_DAYS + 1) * DAY_MS,
            &Caller::Guest,
            None,
            None,
            "read",
            1,
            1,
        );
        assert_eq!(usage.state().hours.len(), 1);
        let r = usage.report_at(t + (KEEP_DAYS + 1) * DAY_MS, 31);
        assert_eq!(r.people[0].name, "Browser mit Code");
        assert_eq!(r.total.requests, 1);
    }
}
