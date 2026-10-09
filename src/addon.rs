//! Einstieg des Home-Assistant-Add-ons (`chordwright-server --addon`): liest
//! die Optionen, die Home Assistant nach /data/options.json schreibt, und
//! startet damit den Server.
//!
//! Alles, was man sonst von Hand übergeben müsste, erledigt sich hier selbst:
//! - Ohne Token wird einmal eins erzeugt, in /data/token gemerkt und im Log
//!   gezeigt. Ein Server im Heimnetz ohne Token startet gar nicht erst.
//! - Mit `ssl` und Zertifikaten in /ssl (Let's Encrypt, DuckDNS) spricht er https
//!   mit denen. Fehlen sie, legt er eine eigene kleine Zertifizierungsstelle an
//!   und stellt sich damit ein Zertifikat für seine eigenen Adressen aus. Die CA
//!   installiert man einmal pro Gerät (sie liegt unter /ca.crt und in share) —
//!   danach vertraut jeder Browser dem Server, auch eine App vom Home-Bildschirm.
//!
//! Die Pfade sind über Umgebungsvariablen umbiegbar — für docker compose und
//! die Tests.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chordwright_server::{ensure_own_certificate, Options};
use serde_json::Value;

/// Namen, die jedes Zertifikat trägt — die üblichen Adressen eines Home Assistant.
const DEFAULT_NAMES: [&str; 3] = ["homeassistant.local", "homeassistant", "localhost"];

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn read_options(data: &Path) -> Value {
    std::fs::read_to_string(data.join("options.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// (Token, selbst erzeugt?)
fn resolve_token(configured: Option<&str>, data: &Path) -> Result<(String, bool), String> {
    if let Some(token) = configured.filter(|t| !t.is_empty()) {
        return Ok((token.to_string(), false));
    }
    let file = data.join("token");
    if let Ok(saved) = std::fs::read_to_string(&file) {
        let saved = saved.trim();
        if !saved.is_empty() {
            return Ok((saved.to_string(), true));
        }
    }
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|e| e.to_string())?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let fail = |e: std::io::Error| format!("{}: {e}", file.display());
    std::fs::create_dir_all(data).map_err(fail)?;
    write_private(&file, &format!("{token}\n")).map_err(fail)?;
    Ok((token, true))
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(text.as_bytes())
}

/// Ein GET mit `Connection: close`, für genau eine Frage an den Supervisor.
fn http_get_json(url: &str, bearer: Option<&str>) -> Option<Value> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest
        .split_once('/')
        .map(|(a, p)| (a, format!("/{p}")))
        .unwrap_or((rest, "/".into()));
    let target = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    let timeout = Duration::from_secs(3);
    let addr = target.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let auth = bearer
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(stream, "GET {path} HTTP/1.1\r\nhost: {authority}\r\n{auth}accept: application/json\r\nconnection: close\r\n\r\n").ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    let mut body = raw[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = unchunk(&body)?;
    }
    serde_json::from_slice(&body).ok()
}

fn unchunk(mut input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = input.windows(2).position(|w| w == b"\r\n")?;
        let size_text = String::from_utf8_lossy(&input[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(input.get(..size)?);
        input = input.get(size + 2..)?;
    }
}

/// Die IP-Adressen des Home-Assistant-Rechners. Das Add-on selbst läuft in
/// einem Container und sieht sie nicht; der Supervisor kennt sie. Scheitert
/// die Frage, geht es ohne weiter — dann tragen nur die Namen.
fn host_addresses() -> Vec<String> {
    let token = std::env::var("SUPERVISOR_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let url = std::env::var("ADDON_SUPERVISOR_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if token.is_none() && url.is_none() {
        return Vec::new();
    }
    let base = url.unwrap_or_else(|| "http://supervisor".into());
    let Some(body) = http_get_json(&format!("{base}/network/info"), token.as_deref()) else {
        return Vec::new();
    };
    let mut ips = Vec::new();
    for iface in body["data"]["interfaces"].as_array().into_iter().flatten() {
        for family in ["ipv4", "ipv6"] {
            for address in iface[family]["address"].as_array().into_iter().flatten() {
                let Some(text) = address.as_str() else {
                    continue;
                };
                let ip = text.split('/').next().unwrap_or_default();
                // Link-local IPv6 taugt nicht als Adresse, die jemand eintippt.
                if !ip.is_empty() && !ip.to_lowercase().starts_with("fe80") {
                    ips.push(ip.to_string());
                }
            }
        }
    }
    ips
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect()
}

/// Die Optionen des Servers, und vorher die Übersicht im Log.
pub fn options() -> Result<Options, String> {
    let data = PathBuf::from(env_or("ADDON_DATA_DIR", "/data"));
    let share = PathBuf::from(env_or("ADDON_SHARE_DIR", "/share"));
    let ssl = PathBuf::from(env_or("ADDON_SSL_DIR", "/ssl"));
    let port: u16 = env_or("ADDON_PORT", "4174")
        .parse()
        .map_err(|_| "ADDON_PORT is no port".to_string())?;

    let options = read_options(&data);
    let folder = options["folder"]
        .as_str()
        .filter(|f| !f.is_empty())
        .unwrap_or("chordwright")
        .to_string();
    let dir = share.join(&folder);
    let (token, generated) = resolve_token(options["token"].as_str(), &data)?;

    // (Zertifikat, Schlüssel, eigene CA mit Namen und IPs)
    let mut tls: Option<(PathBuf, PathBuf, Option<chordwright_server::Own>)> = None;
    if options["ssl"] != Value::Bool(false) {
        let cert = ssl.join(options["certfile"].as_str().unwrap_or("fullchain.pem"));
        let key = ssl.join(options["keyfile"].as_str().unwrap_or("privkey.pem"));
        if cert.exists() && key.exists() {
            tls = Some((cert, key, None));
        } else {
            // Das Zertifikat: für jede Adresse, unter der man ihn erreicht. Aus
            // den Optionen (Home Assistant) oder CHORDWRIGHT_HOSTNAMES (docker compose).
            let mut addresses: Vec<String> = DEFAULT_NAMES.iter().map(|n| n.to_string()).collect();
            addresses.push("127.0.0.1".into());
            addresses.extend(host_addresses());
            addresses.extend(strings(&options["hostnames"]));
            addresses.extend(
                env_or("CHORDWRIGHT_HOSTNAMES", "")
                    .split(',')
                    .map(|h| h.trim().to_string()),
            );
            let own = ensure_own_certificate(&data, &addresses)?;
            tls = Some((own.cert.clone(), own.key.clone(), Some(own)));
        }
    }
    let own = tls.as_ref().and_then(|(_, _, own)| own.as_ref());
    let scheme = if tls.is_some() { "https" } else { "http" };

    // Die CA auch als Datei in share: am Mac per Samba in den Schlüsselbund ziehen.
    if let Some(own) = own {
        let _ = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::copy(&own.ca, dir.join("chordwright-ca.crt")));
    }

    // Unter Home Assistant gibt es den Supervisor-Token; sonst (docker compose,
    // von Hand) ist es der eigene Rechner, und Samba gibt es nicht.
    let in_home_assistant = std::env::var("SUPERVISOR_TOKEN").is_ok_and(|t| !t.is_empty());
    let addresses: Vec<String> = match own {
        Some(own) => own
            .ips
            .iter()
            .filter(|ip| *ip != "127.0.0.1")
            .cloned()
            .chain([if in_home_assistant {
                "homeassistant.local"
            } else {
                "localhost"
            }
            .to_string()])
            .collect(),
        None => Vec::new(),
    };
    let rule = "-".repeat(60);
    println!("{rule}");
    println!("Chordwright Data");
    let samba = if in_home_assistant {
        format!("  (per Samba: share/{folder})")
    } else {
        String::new()
    };
    println!("  Ordner   {}{samba}", dir.display());
    if addresses.is_empty() {
        println!("  Adresse  {scheme}://<deine-Home-Assistant-Adresse>:{port}");
    }
    for a in &addresses {
        let host = if a.contains(':') {
            format!("[{a}]")
        } else {
            a.clone()
        };
        println!("  Adresse  {scheme}://{host}:{port}");
    }
    println!(
        "  Token    {token}{}",
        if generated {
            "   (automatisch erzeugt)"
        } else {
            ""
        }
    );
    if let Some(own) = own {
        println!("  Zertifikat: von der eigenen Chordwright-CA — einmal pro Gerät installieren:");
        let ca_file = if in_home_assistant {
            format!("per Samba: share/{folder}/chordwright-ca.crt")
        } else {
            "oder die Datei chordwright-ca.crt im Datenordner".to_string()
        };
        let first = addresses.first().map(String::as_str).unwrap_or("<Adresse>");
        println!("    {scheme}://{first}:{port}/ca.crt   ({ca_file})");
        println!(
            "  Gilt für: {}",
            own.names
                .iter()
                .chain(&own.ips)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if tls.is_none() {
        println!("  Ohne ssl erreicht die App auf einer https-Seite den Server nicht.");
    }
    println!("In der App: Einstellungen → Datenquelle → Adresse (ohne /api) und Token eintragen.");
    println!("{rule}");

    let defaults = Options::default();
    // Ältere Optionen kennen die beiden nicht; dann gilt der Standard des Servers.
    let every = options["backup_every_hours"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0);
    let keep = options["backup_keep"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 1.0);
    let ca_file = own.map(|o| o.ca.clone());
    let (cert, key) = match tls {
        Some((cert, key, _)) => (Some(cert), Some(key)),
        None => (None, None),
    };
    Ok(Options {
        dir,
        host: "0.0.0.0".into(),
        port,
        token,
        cert,
        key,
        ca_file,
        backup_every: every.unwrap_or(defaults.backup_every),
        backup_keep: keep.map(|k| k as usize).unwrap_or(defaults.backup_keep),
        ..defaults
    })
}
