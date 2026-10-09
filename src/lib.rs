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

mod backups;
mod certs;

pub use certs::{certificate_names, ensure_own_certificate, Own};
mod http;
mod journal;
mod stage;
mod store;
mod util;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use notify::{RecursiveMode, Watcher};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use tokio::net::TcpListener;
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
            backup_every: 24.0,
            backup_keep: 14,
        }
    }
}

/// Where other devices reach a server that listens beyond this machine.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Lan {
    pub port: u16,
    pub urls: Vec<String>,
    pub ca: bool,
}

/// Where the server listens, once it does.
#[derive(Clone, Debug)]
pub struct Ready {
    /// The main listener: `http(s)://<host>:<port>`, without /api.
    pub url: String,
    pub port: u16,
    pub loopback_port: Option<u16>,
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
    shared: Arc<Shared>,
    _watchers: Vec<notify::RecommendedWatcher>,
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
}

impl Drop for Server {
    fn drop(&mut self) {
        self._watchers.clear();
        self.shared.journal.flush();
        if let Some(runtime) = self.runtime.take() {
            // Off this thread: a runtime may not be dropped inside another one.
            let _ =
                std::thread::spawn(move || runtime.shutdown_timeout(Duration::from_millis(500)))
                    .join();
        }
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

    let store = Store::new(&root);
    store
        .ensure_layout()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let listeners = Arc::new(Listeners::default());
    let stage_listeners = listeners.clone();
    let shared = Arc::new(Shared {
        root: root.clone(),
        token: o.token.clone(),
        backup_every: o.backup_every,
        backup_keep: o.backup_keep,
        ca_der,
        store,
        backups: Backups::new(&root, o.backup_keep),
        journal: Journal::open(&root),
        stage: Stage::new(move |snapshot| stage_listeners.send(http::stage_line(&snapshot))),
        listeners,
        self_writes: Mutex::new(HashMap::new()),
        restoring_until: AtomicI64::new(0),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("chordwright-server")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    let (main, loopback) = runtime.block_on(async {
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
        Ok::<_, String>((main, loopback))
    })?;
    let port = main.local_addr().map_err(|e| e.to_string())?.port();
    let loopback_port = loopback
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port());

    let is_tls = tls.is_some();
    runtime.spawn(serve(main, tls, shared.clone()));
    if let Some(loopback) = loopback {
        runtime.spawn(serve(loopback, None, shared.clone()));
    }
    runtime.spawn(background(shared.clone()));
    let watchers = watch(&shared);

    let scheme = if is_tls { "https" } else { "http" };
    let lan = exposed.then(|| Lan {
        port,
        urls: lan_addresses()
            .iter()
            .map(|a| format!("{scheme}://{a}:{port}"))
            .collect(),
        ca: shared.ca_der.is_some(),
    });
    let ready = Ready {
        url: format!("{scheme}://{}:{port}", o.host),
        port,
        loopback_port,
        dir: root,
        tls: is_tls,
        lan,
    };
    Ok(Server {
        runtime: Some(runtime),
        shared,
        _watchers: watchers,
        ready,
    })
}

async fn serve(listener: TcpListener, tls: Option<TlsAcceptor>, shared: Arc<Shared>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of file handles, say: wait a moment rather than spin.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let shared = shared.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| http::handle(shared.clone(), req));
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
async fn background(shared: Arc<Shared>) {
    let saver = shared.clone();
    tokio::spawn(async move {
        loop {
            saver.journal.wake.notified().await;
            let s = saver.clone();
            let _ = tokio::task::spawn_blocking(move || s.journal.flush()).await;
        }
    });
    let pinger = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(25));
        tick.tick().await;
        loop {
            tick.tick().await;
            pinger.listeners.send(": ping\n\n".into());
        }
    });
    let sweeper = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            sweeper.stage.sweep();
        }
    });
    // Checked every hour rather than slept for `every` hours: a server that
    // restarts daily would otherwise never reach the end of a day.
    let every = shared.backup_every;
    if every > 0.0 {
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
                        "Backup {} (auto): {} songs, {} sets",
                        entry.id, entry.songs, entry.sets
                    )
                }
                Ok(Err(e)) => eprintln!("Backup failed: {e}"),
                _ => {}
            }
        }
    }
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
