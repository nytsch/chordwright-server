//! Die Web-UI des Add-ons (Home Assistant: „Web-UI öffnen", per Ingress): ein
//! QR-Code, mit dem sich ein Gerät verbindet, und bei eigener CA einer fürs
//! Zertifikat davor. Mehr nicht — eingestellt wird alles unter „Konfiguration".
//!
//! Die Seite zeigt den Token. Das darf sie, weil Home Assistant sie nur
//! angemeldeten Nutzern ausliefert (wie das Protokoll, in dem er auch steht);
//! deshalb nimmt sie nur Anfragen von Home Assistants Ingress-Proxy an.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::time::Duration;

use qrcodegen::{QrCode, QrCodeEcc};

/// Wohin der QR-Code führt: die Web-App, gleich auf der Datenquellen-Seite,
/// mit Adresse und Token schon eingetragen. Dasselbe wie `connectLink` in der
/// App (app/src/app/localLibrary.ts); die App liest beides auch aus ihrem
/// eigenen Scanner. Bis es eine stabile Fassung gibt, ist das die Beta.
pub const APP_URL: &str = "https://beta.chordwright.app/";

/// `application/x-www-form-urlencoded`, wie `URLSearchParams` es schreibt.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn connect_link(server: &str, token: &str) -> String {
    format!(
        "{APP_URL}#/settings/source?server={}&token={}",
        form_encode(server),
        form_encode(token)
    )
}

/// Eine Adresse als URL — IPv6 in eckigen Klammern.
pub fn address_url(scheme: &str, address: &str, port: &str) -> String {
    if address.contains(':') {
        format!("{scheme}://[{address}]:{port}")
    } else {
        format!("{scheme}://{address}:{port}")
    }
}

/// Welche Adresse zuerst kommt: IPv4 (geht überall), dann Namen (`.local` löst
/// nicht jedes Android auf), IPv6 zuletzt. Doppelte nur einmal.
pub fn order_addresses(addresses: &[String]) -> Vec<String> {
    let rank = |a: &str| {
        let parts: Vec<&str> = a.split('.').collect();
        if parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        {
            0
        } else if a.contains(':') {
            2
        } else {
            1
        }
    };
    let mut out: Vec<String> = Vec::new();
    for a in addresses {
        if !out.contains(a) {
            out.push(a.clone());
        }
    }
    out.sort_by_key(|a| rank(a));
    out
}

fn esc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            c => c.to_string(),
        })
        .collect()
}

/// Schwarz auf weiß in jedem Theme: manche Kameras lesen einen hellen Code auf dunkel nicht.
fn qr_svg(text: &str, label: &str) -> String {
    let Ok(qr) = QrCode::encode_text(text, QrCodeEcc::Medium) else {
        return String::new();
    };
    let border = 2;
    let size = qr.size() + 2 * border;
    let mut path = String::new();
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{} {}h1v1h-1z", x + border, y + border));
            }
        }
    }
    format!(
        "<svg class=\"qr\" role=\"img\" aria-label=\"{}\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\">\
         <rect width=\"{size}\" height=\"{size}\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>",
        esc(label)
    )
}

/// Was die Seite zeigt.
#[derive(Clone)]
pub struct Panel {
    pub addresses: Vec<String>,
    pub scheme: String,
    /// Der Port von außen (unter „Netzwerk“ umstellbar).
    pub port: String,
    pub token: String,
    pub own_ca: bool,
}

impl Panel {
    /// Die Seite. `pick` ist die Nummer der gewählten Adresse (`?a=`), falls es
    /// mehrere gibt — der Code gilt immer für genau eine.
    pub fn page(&self, pick: usize) -> String {
        let list = order_addresses(&self.addresses);
        let chosen = list.get(pick).or(list.first());
        let server = chosen.map(|a| address_url(&self.scheme, a, &self.port));

        let chips = if list.len() > 1 {
            let links: String = list
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let current = if Some(a) == chosen {
                        " aria-current=\"true\""
                    } else {
                        ""
                    };
                    format!("<a href=\"?a={i}\"{current}>{}</a>", esc(a))
                })
                .collect();
            format!("<nav class=\"chips\" aria-label=\"Adresse\">{links}</nav>")
        } else {
            String::new()
        };

        let body = match &server {
            None => "<p class=\"note\">Die Adresse dieses Rechners ist unbekannt. Trag unter <b>Konfiguration → hostnames</b> \
                     den Namen ein, unter dem du Home Assistant erreichst, und starte das Add-on neu.</p>"
                .to_string(),
            Some(server) => {
                let ca = format!("{server}/ca.crt");
                let link = connect_link(server, &self.token);
                let ca_step = if self.own_ca {
                    format!(
                        "<section>\n  <h2><span class=\"n\">1</span>Zertifikat <small>einmal pro Gerät</small></h2>\n  {}\n  \
                         <p>Mit der Kamera scannen und öffnen. Am iPhone danach unter <b>Einstellungen → Allgemein → Info →\n  \
                         Zertifikatsvertrauenseinstellungen</b> „Chordwright lokale CA“ einschalten.</p>\n  \
                         <p class=\"value\"><a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">{}</a></p>\n</section>",
                        qr_svg(&ca, "QR-Code: Zertifikat laden"),
                        esc(&ca),
                        esc(&ca)
                    )
                } else {
                    String::new()
                };
                format!(
                    "{ca_step}\n<section>\n  <h2>{}Verbinden</h2>\n  {}\n  \
                     <p>Mit der Kamera scannen, oder in Chordwright unter <b>Einstellungen → Datenquelle → Server hinzufügen</b>\n  \
                     „QR-Code scannen“. Auf diesem Gerät: <a class=\"connect\" href=\"{}\" target=\"_blank\" rel=\"noreferrer\">hier öffnen</a>.</p>\n  \
                     <dl>\n    <dt>Adresse</dt><dd class=\"value\">{}</dd>\n    <dt>Token</dt><dd class=\"value\">{}</dd>\n  </dl>\n</section>",
                    if self.own_ca { "<span class=\"n\">2</span>" } else { "" },
                    qr_svg(&link, "QR-Code: Chordwright mit diesem Server verbinden"),
                    esc(&link),
                    esc(server),
                    esc(&self.token)
                )
            }
        };

        format!("{HEAD}<h1>Chordwright Data</h1>\n{chips}\n<div class=\"steps\">\n{body}\n</div>\n</main>\n</body>\n</html>\n")
    }
}

const HEAD: &str = r#"<!doctype html>
<html lang="de">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Chordwright Data</title>
<style>
  :root { color-scheme: light dark; --fg: #1c1c1e; --muted: #6b6b70; --line: #d9d9de; --accent: #0a7c6b; --bg: #fafafa; }
  @media (prefers-color-scheme: dark) { :root { --fg: #ececf0; --muted: #a0a0a8; --line: #3a3a40; --accent: #4cc7b2; --bg: #111214; } }
  body { margin: 0; padding: 24px 16px; background: var(--bg); color: var(--fg); font: 15px/1.45 system-ui, sans-serif; }
  main { max-width: 760px; margin: 0 auto; }
  h1 { font-size: 20px; margin: 0 0 16px; }
  .steps { display: flex; flex-wrap: wrap; gap: 24px; }
  section { flex: 1 1 280px; min-width: 0; }
  h2 { font-size: 16px; margin: 0 0 12px; display: flex; align-items: center; gap: 8px; }
  h2 small { color: var(--muted); font-weight: normal; }
  .n { display: inline-grid; place-items: center; width: 22px; height: 22px; border-radius: 50%; background: var(--accent); color: var(--bg); font-size: 13px; }
  .qr { display: block; width: 100%; max-width: 260px; height: auto; border-radius: 8px; }
  p { margin: 12px 0; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 12px 0; }
  dt { color: var(--muted); }
  dd { margin: 0; }
  .value { font-family: ui-monospace, monospace; font-size: 13px; overflow-wrap: anywhere; }
  a { color: var(--accent); }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 20px; }
  .chips a { padding: 6px 12px; border: 1px solid var(--line); border-radius: 999px; text-decoration: none; color: var(--fg); font-size: 13px; }
  .chips a[aria-current] { border-color: var(--accent); color: var(--accent); }
  .note { color: var(--muted); }
</style>
</head>
<body>
<main>
"#;

/// Nur Home Assistants Ingress-Proxy (und der eigene Rechner, für die Tests).
fn allowed(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    ip.is_loopback() || ip == IpAddr::from([172, 30, 32, 2])
}

fn answer(mut stream: TcpStream, panel: &Panel) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut words = line.split(' ');
    let (method, target) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or("/"),
    );
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let respond = |stream: &mut TcpStream, status: &str, headers: &str, body: &str| {
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    };
    if !stream.peer_addr().is_ok_and(|a| allowed(a.ip())) {
        return respond(&mut stream, "403 Forbidden", "", "");
    }
    if method != "GET" || path != "/" {
        return respond(&mut stream, "404 Not Found", "", "");
    }
    // `parseInt(a) || 0`: führende Ziffern zählen, alles andere ist die erste Adresse.
    let pick = query
        .split('&')
        .find_map(|p| p.strip_prefix("a="))
        .map(|v| {
            v.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(0);
    respond(
        &mut stream,
        "200 OK",
        "content-type: text/html; charset=utf-8\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\n",
        &panel.page(pick),
    );
}

/// Startet die Web-UI auf `port`, in Threads neben dem Server.
pub fn start(port: u16, panel: Panel) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let panel = panel.clone();
            std::thread::spawn(move || answer(stream, &panel));
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_order() {
        assert_eq!(
            connect_link("https://[fd00::5]:4174", "a b"),
            "https://beta.chordwright.app/#/settings/source?server=https%3A%2F%2F%5Bfd00%3A%3A5%5D%3A4174&token=a+b"
        );
        let ordered = order_addresses(&[
            "fd00::5".into(),
            "ha.local".into(),
            "10.0.0.9".into(),
            "ha.local".into(),
        ]);
        assert_eq!(ordered, ["10.0.0.9", "ha.local", "fd00::5"]);
        assert!(allowed("::ffff:172.30.32.2".parse().unwrap()));
        assert!(!allowed("192.168.1.2".parse().unwrap()));
    }
}
