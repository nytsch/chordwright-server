//! chordwright serve — a folder of `.chordpro` files, served to the app.
//!
//! One server, three places: the Chordwright desktop app runs it inside its own
//! process (a library kept as a folder on that machine), the Home Assistant
//! add-on runs the program (`src/main.rs --addon`), and anyone can run the
//! program by hand. Up to 1.7.0 this was a Node server; the files on disk, the
//! routes and the answers are the same, and test/server.test.mjs checks them
//! from the outside.
//!
//! `Server::start` runs it on a tokio runtime of its own; dropping the
//! `Server` stops it.
//!
//! Next to the library it was started with, a server can serve more of them
//! on the same port (`Hub::add`): each under `/i/<id>`, with a folder, a
//! token, backups, a change journal and a stage of its own. An app connects
//! to `https://host:4174/i/<id>` as to any server — it only ever appends
//! `/api/…` to the address. The Home Assistant add-on manages them from its
//! web UI (src/instances.rs).
//!
//! Each library can also be opened by its musicians (`Member`), each with a
//! token of their own: they read everything and play along on the shared
//! stage, but the only record they write is their own answers — when they can
//! play (`user/availability.<id>`). So a library can be handed to the band,
//! also over a tunnel from the internet, without anyone rearranging the
//! songs. A musician's access ends when its time is up or when they are taken
//! off the list (`Hub::set_members`).

mod backups;
mod certs;

pub use certs::{certificate_names, ensure_own_certificate, Own};
pub use pairing::new_code as new_pairing_code;
pub use site::{Files, Site};
mod http;
mod journal;
mod pairing;
mod site;
mod stage;
mod store;
mod util;

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::rt::TokioIo;
use notify::{RecursiveMode, Watcher};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use backups::Backups;
use http::{Listeners, Shared};
use journal::Journal;
use stage::Stage;
use store::Store;

/// What the server is started with — the program's command line.
#[derive(Clone, Debug)]
pub struct Options {
    pub dir: PathBuf,
    pub host: String,
    /// 0: the system picks one.
    pub port: u16,
    /// Required for any host but loopback, unless `insecure`.
    pub token: String,
    pub insecure: bool,
    /// https with this certificate and key (PEM).
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    /// The authority that signed `cert`, handed out at /ca.crt.
    pub ca_file: Option<PathBuf>,
    /// https with a certificate authority of its own, kept in this folder.
    pub own_ca: Option<PathBuf>,
    /// More names for that certificate than this machine's own.
    pub hostnames: Vec<String>,
    /// The same server once more, as plain http on 127.0.0.1 — for the
    /// program that started it, which then needs no certificate.
    pub loopback_port: Option<u16>,
    /// The same server once more, as plain http on `host` — for a browser
    /// that has not installed the certificate authority. Whatever it sends,
    /// the token too, crosses the network unencrypted.
    pub plain_port: Option<u16>,
    /// The app itself under `/`, for a browser (site.rs).
    pub site: Option<Site>,
    /// A browser that types this code gets a guest token (pairing.rs).
    pub pairing: Option<String>,
    /// The musicians, each with a token that reads and writes only their answers.
    pub members: Vec<Member>,
    /// A snapshot this often (hours) when something changed; 0 never.
    pub backup_every: f64,
    /// How many scheduled snapshots stay.
    pub backup_keep: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            dir: PathBuf::from("./data"),
            host: "127.0.0.1".into(),
            port: 4174,
            token: String::new(),
            insecure: false,
            cert: None,
            key: None,
            ca_file: None,
            own_ca: None,
            hostnames: Vec::new(),
            loopback_port: None,
            plain_port: None,
            site: None,
            pairing: None,
            members: Vec::new(),
            backup_every: 24.0,
            backup_keep: 14,
        }
    }
}

/// What a musician plays, by letter: drums, piano, acoustic and electric
/// guitar, bass, singer.
pub const ROLES: [(&str, &str); 6] = [
    ("D", "Drum"),
    ("P", "Piano"),
    ("A", "A-Git"),
    ("E", "E-Git"),
    ("B", "Bass"),
    ("S", "Singer"),
];

/// What a musician may change besides their own answers. Backups and
/// deleting songs stay the owner's, whatever this says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Can {
    /// Nothing else.
    #[default]
    Read,
    /// The sets and who plays in them (`user/setlists`, `user/lineups`).
    Sets,
    /// Also songs, their arrangements, and the rest of the user database.
    All,
}

impl Can {
    pub const ALL: [Can; 3] = [Can::Read, Can::Sets, Can::All];

    pub fn is_read(&self) -> bool {
        *self == Can::Read
    }

    /// As a form sends it (`read`, `sets`, `all`); anything else reads only.
    pub fn parse(text: &str) -> Can {
        match text {
            "sets" => Can::Sets,
            "all" => Can::All,
            _ => Can::Read,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Can::Read => "read",
            Can::Sets => "sets",
            Can::All => "all",
        }
    }

    /// For the web UI.
    pub fn label(&self) -> &'static str {
        match self {
            Can::Read => "Nur lesen",
            Can::Sets => "Sets bearbeiten",
            Can::All => "Songs und Sets bearbeiten",
        }
    }
}

/// A musician's way into a library: everything to read, the shared stage to
/// lead and follow, their own answers to write — and, as `can` says, the sets
/// or the songs too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Member {
    /// Like an instance id (`is_instance_id`); names their answers' record.
    pub id: String,
    pub name: String,
    pub token: String,
    /// Letters from `ROLES`.
    #[serde(default)]
    pub roles: Vec<String>,
    /// When the access ends, ms since the epoch; None: until taken back.
    #[serde(default)]
    pub until: Option<i64>,
    #[serde(default, skip_serializing_if = "Can::is_read")]
    pub can: Can,
}

impl Member {
    pub fn expired(&self, now: i64) -> bool {
        self.until.is_some_and(|u| now >= u)
    }

    /// The one record they may write: `user/<this>`.
    pub fn answers_key(&self) -> String {
        format!("availability.{}", self.id)
    }

    /// Whether they may write (`remove`: delete) `db/key`. Another musician's
    /// answers are never theirs; in the library nothing goes away — an index
    /// that drops a song is caught where the write happens (`drops_songs`).
    pub fn may_write(&self, db: &str, key: &str, remove: bool) -> bool {
        if db == "user" && key == self.answers_key() {
            return true;
        }
        match (self.can, db) {
            (Can::Read, _) => false,
            (_, "user") if key.starts_with("availability.") => false,
            (Can::Sets, "user") => key == "setlists" || key == "lineups",
            (Can::All, "user") => true,
            (Can::All, "library") => !remove,
            _ => false,
        }
    }
}

/// Whether a new song index leaves out a song the old one had — that is how
/// the app deletes one, and deleting songs stays the owner's. Songs are told
/// apart by `chordProId`, wherever in the value they sit.
pub fn drops_songs(old: &str, new: &str) -> bool {
    fn ids(text: &str) -> std::collections::HashSet<String> {
        fn walk(v: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        match (k.as_str(), v) {
                            ("chordProId", serde_json::Value::String(id)) => {
                                out.insert(id.clone());
                            }
                            _ => walk(v, out),
                        }
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut out = std::collections::HashSet::new();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            walk(&v, &mut out);
        }
        out
    }
    let now = ids(new);
    ids(old).iter().any(|id| !now.contains(id))
}

/// Where other devices reach a server that listens beyond this machine.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Lan {
    pub port: u16,
    pub urls: Vec<String>,
    pub ca: bool,
    /// Where a browser opens the app (`site`): the plain listener's
    /// addresses when there is one, else the main one's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub site: Vec<String>,
}

/// Where the server listens, once it does.
#[derive(Clone, Debug)]
pub struct Ready {
    /// The main listener: `http(s)://<host>:<port>`, without /api.
    pub url: String,
    pub port: u16,
    pub loopback_port: Option<u16>,
    pub plain_port: Option<u16>,
    pub dir: PathBuf,
    pub tls: bool,
    pub lan: Option<Lan>,
}

impl Ready {
    /// The way in for the program that started it: the loopback listener when
    /// there is one, else the main one.
    pub fn local_url(&self) -> String {
        match self.loopback_port {
            Some(port) => format!("http://127.0.0.1:{port}"),
            None => self.url.clone(),
        }
    }
}

/// A running server. Dropping it stops it.
pub struct Server {
    runtime: Option<tokio::runtime::Runtime>,
    main: Option<Instance>,
    hub: Hub,
    ready: Ready,
}

impl Server {
    /// Starts serving and returns once it listens. Can be called from
    /// anywhere, also from inside another tokio runtime.
    pub fn start(options: Options) -> Result<Server, String> {
        std::thread::spawn(move || start(options))
            .join()
            .map_err(|_| "the server panicked while starting".to_string())?
    }

    pub fn ready(&self) -> &Ready {
        &self.ready
    }

    /// The libraries it serves, to add and remove more while it runs.
    pub fn hub(&self) -> Hub {
        self.hub.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.hub.close();
        drop(self.main.take());
        if let Some(runtime) = self.runtime.take() {
            // Off this thread: a runtime may not be dropped inside another one.
            let _ =
                std::thread::spawn(move || runtime.shutdown_timeout(Duration::from_millis(500)))
                    .join();
        }
    }
}

/// One more library for a running server: its folder and its token.
#[derive(Clone, Debug)]
pub struct Library {
    pub dir: PathBuf,
    /// Never empty: the instances listen wherever the server does.
    pub token: String,
    pub backup_every: f64,
    pub backup_keep: usize,
}

/// `^[a-z0-9][a-z0-9-]{0,31}$` — one path segment, the same in every URL.
pub fn is_instance_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// A library being served, with what runs for it: the file watch and the
/// timers. Dropping it stops those and writes its journal.
struct Instance {
    shared: Arc<Shared>,
    watchers: Vec<notify::RecommendedWatcher>,
    tasks: Vec<JoinHandle<()>>,
}

impl Instance {
    fn open(shared: Arc<Shared>, runtime: &tokio::runtime::Handle) -> Instance {
        Instance {
            tasks: background(&shared, runtime),
            watchers: watch(&shared),
            shared,
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.watchers.clear();
        for task in &self.tasks {
            task.abort();
        }
        self.shared.journal.flush();
    }
}

/// The libraries of one server: the one it was started with, and those under
/// `/i/<id>`. Cheap to clone; every clone is the same server.
#[derive(Clone)]
pub struct Hub(Arc<HubState>);

struct HubState {
    main: Arc<Shared>,
    instances: RwLock<BTreeMap<String, Instance>>,
    runtime: tokio::runtime::Handle,
    ca_der: Option<Vec<u8>>,
    started: i64,
}

/// One change, for a person to read.
#[derive(Serialize, Clone, Debug)]
pub struct Change {
    /// What it was: a song's title, "Sets", the record's key.
    pub label: String,
    pub db: String,
    pub key: String,
    /// ms since the epoch.
    pub at: i64,
    pub by: Option<String>,
    /// `write`, `remove`, `file`, `restore`.
    pub action: String,
}

/// A stage room that is open.
#[derive(Serialize, Clone, Debug)]
pub struct Room {
    pub room: String,
    pub leader: Option<String>,
    pub followers: usize,
}

/// What a library holds and who uses it, right now.
#[derive(Serialize, Clone, Debug)]
pub struct Stats {
    pub dir: PathBuf,
    pub songs: u64,
    pub sets: u64,
    /// library/ and user/ together.
    pub bytes: u64,
    /// Open event streams: apps connected this moment.
    pub devices: usize,
    pub backups: usize,
    /// ms since the epoch.
    pub last_backup: Option<i64>,
    pub rooms: Vec<Room>,
    /// Newest first.
    pub changes: Vec<Change>,
}

impl Hub {
    /// Serves `library` under `/i/<id>` from now on.
    pub fn add(&self, id: &str, library: Library) -> Result<(), String> {
        if !is_instance_id(id) {
            return Err(format!("{id}: not a name for an instance"));
        }
        if library.token.is_empty() {
            return Err(format!("{id}: an instance needs a token"));
        }
        let root = resolve(&library.dir);
        let mut instances = self.instances_mut();
        if instances.contains_key(id) {
            return Err(format!("{id}: already served"));
        }
        if root == self.0.main.root || instances.values().any(|i| i.shared.root == root) {
            return Err(format!("{}: already served", root.display()));
        }
        let shared = library_state(
            root,
            library.token,
            library.backup_every,
            library.backup_keep,
            self.0.ca_der.clone(),
            None,
            None,
        )?;
        instances.insert(id.to_string(), Instance::open(shared, &self.0.runtime));
        Ok(())
    }

    /// Stops serving `/i/<id>`. Its folder stays as it is.
    pub fn remove(&self, id: &str) -> bool {
        self.instances_mut().remove(id).is_some()
    }

    /// The musicians of `/i/<id>` (None: the server's own library) from now
    /// on. Their open event streams end and reconnect with what holds now.
    /// False when there is no such library.
    pub fn set_members(&self, id: Option<&str>, members: Vec<Member>) -> bool {
        let shared = match id {
            None => self.0.main.clone(),
            Some(id) => match self.instances().get(id) {
                Some(i) => i.shared.clone(),
                None => return false,
            },
        };
        shared.set_members(members);
        true
    }

    /// The musicians of `/i/<id>` (None: the server's own).
    pub fn members(&self, id: Option<&str>) -> Option<Vec<Member>> {
        match id {
            None => Some(self.0.main.members()),
            Some(id) => Some(self.instances().get(id)?.shared.members()),
        }
    }

    pub fn ids(&self) -> Vec<String> {
        self.instances().keys().cloned().collect()
    }

    /// When the server started, ms since the epoch.
    pub fn started(&self) -> i64 {
        self.0.started
    }

    /// The library under `/i/<id>`, or the server's own for None.
    pub fn stats(&self, id: Option<&str>) -> Option<Stats> {
        let shared = match id {
            None => self.0.main.clone(),
            Some(id) => self.instances().get(id)?.shared.clone(),
        };
        Some(stats_of(&shared))
    }

    fn instances(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Instance>> {
        self.0.instances.read().unwrap_or_else(|e| e.into_inner())
    }

    fn instances_mut(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Instance>> {
        self.0.instances.write().unwrap_or_else(|e| e.into_inner())
    }

    fn close(&self) {
        let all = std::mem::take(&mut *self.instances_mut());
        drop(all);
    }

    /// Which library a request is for: `/i/<id>/…` one of the instances, with
    /// the prefix taken off the path; anything else the server's own.
    fn pick<B>(&self, req: &mut Request<B>) -> Arc<Shared> {
        let path = req.uri().path();
        let Some((id, rest)) = path
            .strip_prefix("/i/")
            .map(|r| r.split_once('/').map_or((r, ""), |(id, rest)| (id, rest)))
        else {
            return self.0.main.clone();
        };
        let Some(shared) = self.instances().get(id).map(|i| i.shared.clone()) else {
            return self.0.main.clone();
        };
        let target = match req.uri().query() {
            Some(q) => format!("/{rest}?{q}"),
            None => format!("/{rest}"),
        };
        if let Ok(uri) = target.parse() {
            *req.uri_mut() = uri;
        }
        shared
    }
}

fn stats_of(s: &Shared) -> Stats {
    let (songs, sets, bytes) = s.backups.tally();
    let backups = s.backups.list();
    let changes = s
        .journal
        .recent(8)
        .into_iter()
        .filter_map(|c| {
            let db = c["db"].as_str()?.to_string();
            let key = c["key"].as_str()?.to_string();
            Some(Change {
                label: s.store.label(&db, &key),
                at: c["at"].as_str().and_then(util::parse_iso).unwrap_or(0),
                by: c["by"].as_str().map(String::from),
                action: c["action"].as_str().unwrap_or("write").to_string(),
                db,
                key,
            })
        })
        .collect();
    let rooms = s
        .stage
        .list()
        .into_iter()
        .filter_map(|r| {
            Some(Room {
                room: r["room"].as_str()?.to_string(),
                leader: r["leader"]["name"].as_str().map(String::from),
                followers: r["followers"].as_u64().unwrap_or(0) as usize,
            })
        })
        .collect();
    Stats {
        dir: s.root.clone(),
        songs,
        sets,
        bytes,
        devices: s.listeners.count(),
        last_backup: backups.first().and_then(|b| util::parse_iso(&b.created_at)),
        backups: backups.len(),
        rooms,
        changes,
    }
}

/// `path.resolve`: absolute, with `.` and `..` worked out, links left alone.
fn resolve(dir: &Path) -> PathBuf {
    let joined = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(dir)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Where devices in the network reach this machine: its name (also as
/// `<name>.local`, which macOS, iOS and Windows resolve over mDNS) and its
/// IPv4 addresses. IPv6 is left out: a server bound to 0.0.0.0 does not
/// answer on it.
fn lan_addresses() -> Vec<String> {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let base = host
        .strip_suffix(".local")
        .or_else(|| host.strip_suffix(".LOCAL"))
        .unwrap_or(&host)
        .to_string();
    let mut all = vec![format!("{base}.local"), base];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let IpAddr::V4(ip) = iface.ip() {
            if !iface.is_loopback() {
                all.push(ip.to_string());
            }
        }
    }
    let mut seen = Vec::new();
    all.retain(|a| {
        !a.is_empty() && !seen.contains(a) && {
            seen.push(a.clone());
            true
        }
    });
    all
}

fn tls_config(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn start(mut o: Options) -> Result<Server, String> {
    // Binding to anything but loopback puts every song in the venue's wifi
    // within reach of anyone on it. Refuse rather than warn: a warning
    // scrolls past.
    let exposed = o.host != "127.0.0.1" && o.host != "localhost";
    if exposed && o.token.is_empty() && !o.insecure {
        return Err(format!(
            "Refusing to listen on {} without --token.\n\
             Anyone on the network could read and rewrite the library.\n\
             Pass --token <secret>, or --insecure if you really mean it.",
            o.host
        ));
    }
    if o.cert.is_some() != o.key.is_some() {
        return Err("--cert and --key go together: pass both, or neither.".into());
    }
    let root = resolve(&o.dir);

    // `own_ca`: https with a certificate authority of its own — the same one
    // the Home Assistant add-on makes. The CA is installed once per device;
    // the certificate it issues names every address of this machine and is
    // issued anew when one changes.
    if let (None, Some(dir)) = (&o.cert, &o.own_ca) {
        let mut addresses = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        addresses.extend(lan_addresses());
        addresses.extend(o.hostnames.iter().cloned());
        let own = certs::ensure_own_certificate(&resolve(dir), &addresses)?;
        o.cert = Some(own.cert);
        o.key = Some(own.key);
        o.ca_file.get_or_insert(own.ca);
    }
    let tls = match (&o.cert, &o.key) {
        (Some(cert), Some(key)) => Some(TlsAcceptor::from(tls_config(cert, key)?)),
        _ => None,
    };
    let ca_der = match &o.ca_file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .ok()
                .and_then(|t| certs::pem_block(&t, "CERTIFICATE"))
                .ok_or_else(|| format!("{}: no certificate", path.display()))?,
        ),
        None => None,
    };

    let shared = library_state(
        root.clone(),
        o.token.clone(),
        o.backup_every,
        o.backup_keep,
        ca_der,
        o.site.clone(),
        o.pairing
            .clone()
            .filter(|c| !c.is_empty())
            .map(pairing::Pairing::new),
    )?;
    shared.set_members(o.members.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("chordwright-server")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    let (main, loopback, plain) = runtime.block_on(async {
        let main = TcpListener::bind((o.host.as_str(), o.port))
            .await
            .map_err(|e| format!("listen {}:{}: {e}", o.host, o.port))?;
        let loopback = match o.loopback_port {
            Some(port) => Some(
                TcpListener::bind(("127.0.0.1", port))
                    .await
                    .map_err(|e| format!("listen 127.0.0.1:{port}: {e}"))?,
            ),
            None => None,
        };
        let plain = match o.plain_port {
            Some(port) => Some(
                TcpListener::bind((o.host.as_str(), port))
                    .await
                    .map_err(|e| format!("listen {}:{port}: {e}", o.host))?,
            ),
            None => None,
        };
        Ok::<_, String>((main, loopback, plain))
    })?;
    let port = main.local_addr().map_err(|e| e.to_string())?.port();
    let loopback_port = loopback
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port());
    let plain_port = plain
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port());

    let is_tls = tls.is_some();
    let hub = Hub(Arc::new(HubState {
        main: shared.clone(),
        instances: RwLock::new(BTreeMap::new()),
        runtime: runtime.handle().clone(),
        ca_der: shared.ca_der.clone(),
        started: util::now_ms(),
    }));
    runtime.spawn(serve(main, tls, hub.clone()));
    if let Some(loopback) = loopback {
        runtime.spawn(serve(loopback, None, hub.clone()));
    }
    if let Some(plain) = plain {
        runtime.spawn(serve(plain, None, hub.clone()));
    }
    let main = Instance::open(shared.clone(), runtime.handle());

    let scheme = if is_tls { "https" } else { "http" };
    let lan = exposed.then(|| {
        let addresses = lan_addresses();
        let urls: Vec<String> = addresses
            .iter()
            .map(|a| format!("{scheme}://{a}:{port}"))
            .collect();
        let site = match (&o.site, plain_port) {
            (None, _) => Vec::new(),
            (Some(_), Some(plain)) => addresses
                .iter()
                .map(|a| format!("http://{a}:{plain}"))
                .collect(),
            (Some(_), None) => urls.clone(),
        };
        Lan {
            port,
            urls,
            ca: shared.ca_der.is_some(),
            site,
        }
    });
    let ready = Ready {
        url: format!("{scheme}://{}:{port}", o.host),
        port,
        loopback_port,
        plain_port,
        dir: root,
        tls: is_tls,
        lan,
    };
    Ok(Server {
        runtime: Some(runtime),
        main: Some(main),
        hub,
        ready,
    })
}

/// What one library needs while it is served, its folder laid out.
fn library_state(
    root: PathBuf,
    token: String,
    backup_every: f64,
    backup_keep: usize,
    ca_der: Option<Vec<u8>>,
    site: Option<Site>,
    pairing: Option<pairing::Pairing>,
) -> Result<Arc<Shared>, String> {
    let store = Store::new(&root);
    store
        .ensure_layout()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let listeners = Arc::new(Listeners::default());
    let stage_listeners = listeners.clone();
    Ok(Arc::new(Shared {
        token,
        backup_every,
        backup_keep,
        ca_der,
        site,
        pairing,
        store,
        backups: Backups::new(&root, backup_keep),
        journal: Journal::open(&root),
        members: RwLock::new(Vec::new()),
        stage: Stage::new(move |snapshot| stage_listeners.send(http::stage_line(&snapshot))),
        listeners,
        self_writes: Mutex::new(HashMap::new()),
        restoring_until: AtomicI64::new(0),
        root,
    }))
}

async fn serve(listener: TcpListener, tls: Option<TlsAcceptor>, hub: Hub) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of file handles, say: wait a moment rather than spin.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let hub = hub.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let service = service_fn(move |mut req| {
                let shared = hub.pick(&mut req);
                http::handle(shared, req)
            });
            let builder = http1::Builder::new();
            match tls {
                // A handshake that fails — plain http on the https port, a
                // device without the CA — just ends the connection.
                Some(tls) => {
                    if let Ok(stream) = tls.accept(stream).await {
                        let _ = builder
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    }
                }
                None => {
                    let _ = builder
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                }
            }
        });
    }
}

/// The timers: the journal written behind, a comment on the event streams
/// every 25 s (proxies and phones drop an idle stream), stage leases run out,
/// scheduled backups.
fn background(shared: &Arc<Shared>, runtime: &tokio::runtime::Handle) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::new();
    let saver = shared.clone();
    tasks.push(runtime.spawn(async move {
        loop {
            saver.journal.wake.notified().await;
            let s = saver.clone();
            let _ = tokio::task::spawn_blocking(move || s.journal.flush()).await;
        }
    }));
    let pinger = shared.clone();
    tasks.push(runtime.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(25));
        tick.tick().await;
        loop {
            tick.tick().await;
            pinger.listeners.send(": ping\n\n".into());
        }
    }));
    let sweeper = shared.clone();
    tasks.push(runtime.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            sweeper.stage.sweep();
        }
    }));
    // Checked every hour rather than slept for `every` hours: a server that
    // restarts daily would otherwise never reach the end of a day.
    let every = shared.backup_every;
    if every > 0.0 {
        let shared = shared.clone();
        tasks.push(runtime.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                tick.tick().await;
                let s = shared.clone();
                match tokio::task::spawn_blocking(move || {
                    s.backups.create_if_due(every * 60.0 * 60.0 * 1000.0)
                })
                .await
                {
                    Ok(Ok(Some(entry))) => {
                        println!(
                            "Backup {} (auto, {}): {} songs, {} sets",
                            entry.id,
                            shared.root.display(),
                            entry.songs,
                            entry.sets
                        )
                    }
                    Ok(Err(e)) => eprintln!("Backup failed ({}): {e}", shared.root.display()),
                    _ => {}
                }
            }
        }));
    }
    tasks
}

/// Edits made to the files directly — "save in vim, watch the app update".
///
/// Not one recursive watch: a directory watch sees every entry in it however
/// it was written, and the layout is fixed, so three directories are the whole
/// tree.
fn watch(shared: &Arc<Shared>) -> Vec<notify::RecommendedWatcher> {
    // Events may name the folder by its real path (/private/var on macOS for
    // /var), so both spellings of the root count.
    let roots: Vec<PathBuf> = [Some(shared.root.clone()), shared.root.canonicalize().ok()]
        .into_iter()
        .flatten()
        .collect();
    let mut watchers = Vec::new();
    for sub in ["library", "library/songs", "user"] {
        let target = shared.root.join(sub);
        let s = shared.clone();
        let roots = roots.clone();
        let handler = move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in &event.paths {
                if let Some(rel) = roots.iter().find_map(|r| path.strip_prefix(r).ok()) {
                    s.file_changed(&rel.to_string_lossy());
                }
            }
        };
        let watched = notify::recommended_watcher(handler).and_then(|mut w| {
            w.watch(&target, RecursiveMode::NonRecursive)?;
            Ok(w)
        });
        match watched {
            Ok(w) => watchers.push(w),
            Err(e) => {
                eprintln!("File watching unavailable for {sub} ({e}); live reload is off there.")
            }
        }
    }
    watchers
}
