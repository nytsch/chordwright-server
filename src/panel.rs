//! Die Web-UI des Add-ons (Home Assistant: „Web-UI öffnen", per Ingress):
//! alle Bibliotheken auf einen Blick, mit Songs, Sets, verbundenen Geräten,
//! letzter Änderung und Sicherungen; zu der gewählten die QR-Codes zum
//! Verbinden (bei eigener CA einer fürs Zertifikat davor), die letzten
//! Änderungen, offene Bühnen — und für die weiteren Bibliotheken (instances.rs)
//! Anlegen, Umbenennen, neuer Token, Entfernen. Was die Hauptbibliothek
//! betrifft, steht weiter unter „Konfiguration“.
//!
//! Die Seite zeigt die Tokens. Das darf sie, weil Home Assistant sie nur
//! angemeldeten Nutzern ausliefert (wie das Protokoll, in dem der erste auch
//! steht); deshalb nimmt sie nur Anfragen von Home Assistants Ingress-Proxy
//! an. Jedes Formular trägt ein Geheimnis dieses Prozesses mit, damit keine
//! fremde Seite im Browser eines Angemeldeten etwas abschicken kann.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chordwright_server::{Can, Hub, Member, Stats, ROLES};
use qrcodegen::{QrCode, QrCodeEcc};

use crate::instances::{Entry, Registry};

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

/// Umgekehrt, für die Formulare: `+` ist ein Leerzeichen, `%XX` ein Byte.
fn form_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(bytes[i + 1]) * 16 + hex(bytes[i + 2]));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Alle Werte eines Feldes, etwa angehakte Kästchen gleichen Namens.
fn form_fields(body: &str, name: &str) -> Vec<String> {
    body.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (form_decode(k) == name).then(|| form_decode(v))
        })
        .collect()
}

fn form_field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (form_decode(k) == name).then(|| form_decode(v))
    })
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

/// Wo die App den Server findet, zur Wahl über dem QR-Code: die Adressen im
/// Heimnetz, mit Tunnel zuletzt die von draußen. Je Ort die Beschriftung und
/// die Adresse ohne Pfad.
pub fn places(
    addresses: &[String],
    scheme: &str,
    port: &str,
    public: Option<&str>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = order_addresses(addresses)
        .into_iter()
        .map(|a| {
            let url = address_url(scheme, &a, port);
            (a, url)
        })
        .collect();
    if let Some(url) = public {
        let host = url.split_once("://").map_or(url, |(_, rest)| rest);
        out.push((host.to_string(), url.to_string()));
    }
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// „vor 5 Min.“ — wie lange `then` her ist.
fn ago(then: i64, now: i64) -> String {
    let s = (now - then).max(0) / 1000;
    match s {
        0..60 => "gerade eben".into(),
        60..3600 => format!("vor {} Min.", s / 60),
        3600..86400 => format!("vor {} Std.", s / 3600),
        86400..172800 => "gestern".into(),
        _ => format!("vor {} Tagen", s / 86400),
    }
}

/// „läuft seit 3 Std.“
fn since(then: i64, now: i64) -> String {
    let s = (now - then).max(0) / 1000;
    match s {
        0..60 => "gerade gestartet".into(),
        60..3600 => format!("läuft seit {} Min.", s / 60),
        3600..86400 => format!("läuft seit {} Std.", s / 3600),
        _ => format!("läuft seit {} Tagen", s / 86400),
    }
}

/// „1,2 MB“
/// Wie lange eine Freigabe noch gilt.
fn left(until: i64, now: i64) -> String {
    let s = (until - now) / 1000;
    match s {
        i64::MIN..=0 => "abgelaufen".into(),
        1..3600 => format!("noch {} Min.", (s / 60).max(1)),
        3600..172800 => format!("noch {} Std.", s / 3600),
        _ => format!("noch {} Tage", (s + 43_200) / 86400),
    }
}

/// Die Wahl, wie lange eine Freigabe gilt; 0 heißt bis auf Widerruf.
const BAND_DAYS: [(u32, &str); 5] = [
    (1, "1 Tag"),
    (3, "3 Tage"),
    (7, "1 Woche"),
    (30, "30 Tage"),
    (0, "bis auf Widerruf"),
];

fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{} KB", (b / 1024.0).round())
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0).replace('.', ",")
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn action_label(action: &str) -> &'static str {
    match action {
        "remove" => "gelöscht",
        "file" => "von Hand gespeichert",
        "restore" => "wiederhergestellt",
        _ => "geändert",
    }
}

/// Was die Seite zeigt, solange das Add-on läuft.
#[derive(Clone)]
pub struct Panel {
    pub addresses: Vec<String>,
    pub scheme: String,
    /// Der Port von außen (unter „Netzwerk“ umstellbar).
    pub port: String,
    /// Der Token der Hauptbibliothek.
    pub token: String,
    pub own_ca: bool,
    /// Der Ordner der Hauptbibliothek in share.
    pub folder: String,
    /// Unter Home Assistant: Pfade als „share/…“, wie Samba sie zeigt.
    pub samba: bool,
    /// Wo die Bibliotheken übers Internet zu erreichen sind (ein Tunnel),
    /// ohne `/` am Ende — die Adresse im Code für die Band.
    pub public_url: Option<String>,
}

/// Die Bibliothek, um die es auf der Seite geht.
struct Shown<'a> {
    /// None: die Hauptbibliothek.
    entry: Option<&'a Entry>,
    name: String,
    token: String,
    /// `/i/<id>` oder leer.
    path: String,
    stats: Option<Stats>,
}

/// Panel samt dem, was sich ändert: die Bibliotheken und ihre Zahlen.
pub struct Ui {
    pub panel: Panel,
    pub registry: Arc<Registry>,
    pub hub: Hub,
    /// Steht in jedem Formular; ohne es nimmt die Seite nichts an.
    pub secret: String,
}

/// Eine Rückmeldung nach dem Abschicken, über `?m=` in der Adresse.
fn notice(code: &str) -> Option<&'static str> {
    Some(match code {
        "created" => "Bibliothek angelegt. Jetzt ein Gerät verbinden.",
        "renamed" => "Umbenannt.",
        "token" => "Neuer Token. Geräte mit dem alten müssen sich neu verbinden.",
        "removed" => "Entfernt. Der Ordner mit den Songs bleibt in share.",
        "member" => "Aufgenommen. Jetzt mit dem QR-Code verbinden.",
        "saved" => "Gespeichert.",
        "extended" => "Verlängert. Der Zugang bleibt derselbe.",
        "unmember" => "Von der Liste genommen. Sein Zugang gilt nicht mehr.",
        _ => return None,
    })
}

impl Ui {
    fn folder_label(&self, dir: &std::path::Path, folder: &str) -> String {
        if self.panel.samba {
            format!("share/{folder}")
        } else {
            dir.display().to_string()
        }
    }

    /// Die Seite. `id`: die gewählte Bibliothek (leer: die Hauptbibliothek),
    /// `pick`: die Nummer der gewählten Adresse (`?a=`), falls es mehrere gibt
    /// — ein Code gilt immer für genau eine.
    pub fn page(
        &self,
        id: &str,
        person: &str,
        pick: usize,
        message: Option<&str>,
        error: Option<&str>,
    ) -> String {
        let now = now_ms();
        let entries = self.registry.list();
        let entry = entries.iter().find(|e| e.id == id);
        let shown = match entry {
            Some(e) => Shown {
                entry: Some(e),
                name: e.name.clone(),
                token: e.token.clone(),
                path: format!("/i/{}", e.id),
                stats: self.hub.stats(Some(&e.id)),
            },
            None => Shown {
                entry: None,
                name: "Hauptbibliothek".into(),
                token: self.panel.token.clone(),
                path: String::new(),
                stats: self.hub.stats(None),
            },
        };
        let sel = shown.entry.map(|e| e.id.as_str()).unwrap_or("");

        // Die Übersicht: eine Karte je Bibliothek.
        let mut cards = String::new();
        let mut devices = 0;
        let all = std::iter::once((
            None,
            "Hauptbibliothek".to_string(),
            self.panel.folder.clone(),
        ))
        .chain(
            entries
                .iter()
                .map(|e| (Some(e), e.name.clone(), e.folder.clone())),
        );
        for (e, name, folder) in all {
            let eid = e.map(|e| e.id.as_str()).unwrap_or("");
            let stats = self.hub.stats(e.map(|e| e.id.as_str()));
            let (nums, last, place) = match &stats {
                Some(st) => {
                    devices += st.devices;
                    let last = match st.changes.first() {
                        Some(c) => format!(
                            "zuletzt {}{}",
                            ago(c.at, now),
                            c.by.as_deref()
                                .map(|b| format!(" von {}", esc(b)))
                                .unwrap_or_default()
                        ),
                        None => "noch keine Änderung".into(),
                    };
                    (
                        format!(
                            "{} · {} · <span title=\"Gerade verbundene Apps\">{}</span>",
                            plural(st.songs, "Song", "Songs"),
                            plural(st.sets, "Set", "Sets"),
                            plural(st.devices as u64, "Gerät", "Geräte")
                        ),
                        last,
                        self.folder_label(&st.dir, &folder),
                    )
                }
                None => (
                    "startet nicht — siehe Protokoll".into(),
                    String::new(),
                    folder.clone(),
                ),
            };
            let current = if eid == sel {
                " aria-current=\"true\""
            } else {
                ""
            };
            let live = if stats.as_ref().is_some_and(|s| s.devices > 0) {
                "<span class=\"dot\" title=\"Gerade verbunden\"></span>"
            } else {
                ""
            };
            cards.push_str(&format!(
                "<a class=\"lib\" href=\"?i={}\"{current}><b>{live}{}</b><span class=\"nums\">{nums}</span>\
                 <span class=\"muted\">{last}</span><span class=\"muted path\">{}</span></a>\n",
                esc(eid),
                esc(&name),
                esc(&place)
            ));
        }
        let create = format!(
            "<form class=\"lib new\" method=\"post\" action=\"./\">{}<input type=\"hidden\" name=\"do\" value=\"create\">\
             <label for=\"new-name\"><b>Neue Bibliothek</b></label>\
             <input id=\"new-name\" name=\"name\" required maxlength=\"60\" placeholder=\"z. B. Band, Chor\" autocomplete=\"off\">\
             <button>Anlegen</button></form>",
            self.csrf()
        );

        // Die gewählte: Adressen, QR-Codes, Zahlen, Verwalten.
        let list = order_addresses(&self.panel.addresses);
        let chosen = list.get(pick).or(list.first());
        let spots = places(
            &self.panel.addresses,
            &self.panel.scheme,
            &self.panel.port,
            self.panel.public_url.as_deref(),
        );
        let pick = if pick < spots.len() { pick } else { 0 };
        // Über den Tunnel: Cloudflare hat ein echtes Zertifikat, die eigene CA braucht es dann nicht.
        let outside = self.panel.public_url.is_some() && pick + 1 == spots.len();
        let server = spots
            .get(pick)
            .map(|(_, base)| format!("{base}{}", shown.path));
        let id_q = if sel.is_empty() {
            String::new()
        } else {
            format!("i={}&amp;", esc(sel))
        };
        let chips = if spots.len() > 1 {
            let links: String = spots
                .iter()
                .enumerate()
                .map(|(i, (label, _))| {
                    let current = if i == pick {
                        " aria-current=\"true\""
                    } else {
                        ""
                    };
                    let title = if self.panel.public_url.is_some() && i + 1 == spots.len() {
                        " title=\"Von unterwegs, über den Tunnel\""
                    } else {
                        ""
                    };
                    format!(
                        "<a href=\"?{id_q}a={i}\"{current}{title}>{}</a>",
                        esc(label)
                    )
                })
                .collect();
            format!("<nav class=\"chips\" aria-label=\"Adresse\">{links}</nav>")
        } else {
            String::new()
        };

        let steps = match &server {
            None => "<p class=\"note\">Die Adresse dieses Rechners ist unbekannt. Trag unter <b>Konfiguration → hostnames</b> \
                     den Namen ein, unter dem du Home Assistant erreichst, und starte das Add-on neu.</p>"
                .to_string(),
            Some(server) => {
                let root = server.strip_suffix(&shown.path).unwrap_or(server);
                let ca = format!("{root}/ca.crt");
                let link = connect_link(server, &shown.token);
                let ca_step = if self.panel.own_ca && !outside {
                    format!(
                        "<section>\n  <h3><span class=\"n\">1</span>Zertifikat <small>einmal pro Gerät</small></h3>\n  {}\n  \
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
                    "{ca_step}\n<section>\n  <h3>{}Verbinden</h3>\n  {}\n  \
                     <p>Mit der Kamera scannen, oder in Chordwright unter <b>Einstellungen → Datenquelle → Server hinzufügen</b>\n  \
                     „QR-Code scannen“. Auf diesem Gerät: <a class=\"connect\" href=\"{}\" target=\"_blank\" rel=\"noreferrer\">hier öffnen</a>.</p>\n  \
                     <dl>\n    <dt>Adresse</dt><dd class=\"value\">{}</dd>\n    <dt>Token</dt><dd class=\"value\">{}</dd>\n  </dl>\n</section>",
                    if self.panel.own_ca && !outside { "<span class=\"n\">2</span>" } else { "" },
                    qr_svg(&link, "QR-Code: Chordwright mit dieser Bibliothek verbinden"),
                    esc(&link),
                    esc(server),
                    esc(&shown.token)
                )
            }
        };

        let band = self.band_section(sel, person, &shown, chosen, now);

        let figures = match &shown.stats {
            None => String::new(),
            Some(st) => {
                let backup = match st.last_backup {
                    Some(at) => format!("{} · letzte {}", st.backups, ago(at, now)),
                    None => "noch keine".into(),
                };
                let folder = shown
                    .entry
                    .map(|e| e.folder.as_str())
                    .unwrap_or(&self.panel.folder);
                let tiles = [
                    ("Songs", st.songs.to_string(), ""),
                    ("Sets", st.sets.to_string(), ""),
                    ("Geräte jetzt", st.devices.to_string(), "Apps, die gerade mit dieser Bibliothek verbunden sind"),
                    ("Größe", size(st.bytes), "Songs, Abläufe und Einstellungen, ohne Sicherungen"),
                ]
                .iter()
                .map(|(k, v, title)| {
                    let title = if title.is_empty() {
                        String::new()
                    } else {
                        format!(" title=\"{}\"", esc(title))
                    };
                    format!("<div class=\"tile\"{title}><span class=\"big\">{}</span><span class=\"muted\">{k}</span></div>", esc(v))
                })
                .collect::<String>();
                let changes = if st.changes.is_empty() {
                    "<p class=\"muted\">Noch keine Änderung.</p>".to_string()
                } else {
                    let rows: String = st
                        .changes
                        .iter()
                        .map(|c| {
                            format!(
                                "<li><span class=\"what\">{}</span> <span class=\"muted\">{}{} · {}</span></li>",
                                esc(&c.label),
                                action_label(&c.action),
                                c.by.as_deref()
                                    .map(|b| format!(" von {}", esc(b)))
                                    .unwrap_or_default(),
                                ago(c.at, now)
                            )
                        })
                        .collect();
                    format!("<ul class=\"rows\">{rows}</ul>")
                };
                let rooms = if st.rooms.is_empty() {
                    String::new()
                } else {
                    let rows: String = st
                        .rooms
                        .iter()
                        .map(|r| {
                            format!(
                                "<li><span class=\"what\">{}</span> <span class=\"muted\">{} · {}</span></li>",
                                esc(&r.room),
                                r.leader
                                    .as_deref()
                                    .map(|l| format!("leitet: {}", esc(l)))
                                    .unwrap_or_else(|| "niemand leitet".into()),
                                plural(r.followers as u64, "folgt", "folgen")
                            )
                        })
                        .collect();
                    format!("<h3>Gemeinsame Bühne</h3>\n<ul class=\"rows\">{rows}</ul>")
                };
                format!(
                    "<div class=\"tiles\">{tiles}</div>\n<dl class=\"facts\">\
                     <dt>Ordner</dt><dd class=\"value\">{}</dd>\
                     <dt>Sicherungen</dt><dd>{}</dd></dl>\n\
                     <h3>Letzte Änderungen</h3>\n{changes}\n{rooms}",
                    esc(&self.folder_label(&st.dir, folder)),
                    esc(&backup)
                )
            }
        };

        let manage = match shown.entry {
            None => "<p class=\"muted\">Token, Ordner und Sicherungen der Hauptbibliothek stellst du unter <b>Konfiguration</b> ein.</p>".to_string(),
            Some(e) => {
                let id = esc(&e.id);
                let csrf = self.csrf();
                format!(
                    "<form class=\"row\" method=\"post\" action=\"./\">{csrf}<input type=\"hidden\" name=\"do\" value=\"rename\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
                     <input name=\"name\" required maxlength=\"60\" value=\"{}\" aria-label=\"Name\"><button>Umbenennen</button></form>\n\
                     <div class=\"row\">\
                     <form method=\"post\" action=\"./\" onsubmit=\"return confirm('Neuer Token? Jedes Gerät muss sich danach neu verbinden.')\">{csrf}<input type=\"hidden\" name=\"do\" value=\"token\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
                     <button class=\"quiet\" title=\"Geräte mit dem alten Token kommen nicht mehr hinein\">Neuer Token</button></form>\
                     <form method=\"post\" action=\"./\" onsubmit=\"return confirm('Entfernen? Die Bibliothek ist dann nicht mehr erreichbar. Der Ordner mit den Songs bleibt.')\">{csrf}<input type=\"hidden\" name=\"do\" value=\"remove\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
                     <button class=\"quiet danger\" title=\"Der Ordner bleibt in share; dieselbe wieder anlegen holt ihn zurück\">Entfernen</button></form>\
                     </div>",
                    esc(&e.name)
                )
            }
        };

        let banner = match (error, message) {
            (Some(e), _) => format!("<p class=\"banner error\" role=\"alert\">{}</p>\n", esc(e)),
            (None, Some(m)) => format!("<p class=\"banner\" role=\"status\">{}</p>\n", esc(m)),
            _ => String::new(),
        };
        let sub = format!(
            "Version {} · {} · {}",
            env!("CARGO_PKG_VERSION"),
            since(self.hub.started(), now),
            plural(devices as u64, "Gerät verbunden", "Geräte verbunden")
        );

        format!(
            "{HEAD}<header><h1>Chordwright Data</h1><p class=\"muted\">{sub}</p></header>\n{banner}\
             <h2>Bibliotheken</h2>\n<div class=\"libs\">\n{cards}{create}\n</div>\n\
             <article>\n<h2>{}</h2>\n{chips}\n<div class=\"steps\">\n{steps}\n</div>\n\
             <h3 id=\"band\">Band</h3>\n{band}\n<h3>Zahlen</h3>\n{figures}\n<h3>Verwalten</h3>\n{manage}\n</article>\n</main>\n</body>\n</html>\n",
            esc(&shown.name)
        )
    }

    /// Die Musiker: wer mit welchen Rollen dabei ist und wie lange, einer
    /// aufnehmen — und zum gewählten (`person`) Code, Adresse und Token,
    /// Name und Rollen, verlängern, von der Liste nehmen.
    fn band_section(
        &self,
        sel: &str,
        person: &str,
        shown: &Shown,
        chosen: Option<&String>,
        now: i64,
    ) -> String {
        let csrf = self.csrf();
        let id = esc(sel);
        let choices = |current: u32| -> String {
            BAND_DAYS
                .iter()
                .map(|(d, label)| {
                    let on = if *d == current { " selected" } else { "" };
                    format!("<option value=\"{d}\"{on}>{label}</option>")
                })
                .collect()
        };
        let boxes = |have: &[String]| -> String {
            ROLES
                .iter()
                .map(|(l, name)| {
                    let on = if have.iter().any(|h| h == l) { " checked" } else { "" };
                    format!(
                        "<label title=\"{name}\"><input type=\"checkbox\" name=\"role\" value=\"{l}\"{on}>{l}</label>"
                    )
                })
                .collect()
        };
        // Was einer außer seinen Antworten schreiben darf. Sicherungen und
        // Songs löschen bleiben beim Besitzer, gleich was hier steht.
        let rights = |current: Can| -> String {
            let options: String = Can::ALL
                .iter()
                .map(|c| {
                    let on = if *c == current { " selected" } else { "" };
                    format!("<option value=\"{}\"{on}>{}</option>", c.id(), c.label())
                })
                .collect();
            format!(
                "<select name=\"can\" aria-label=\"Darf\" title=\"Sicherungen und Songs löschen bleiben bei dir\">{options}</select>"
            )
        };
        let state = |m: &Member| match m.until {
            _ if m.expired(now) => "abgelaufen".to_string(),
            None => "bis auf Widerruf".to_string(),
            Some(until) => left(until, now),
        };
        let members = self.registry.members(sel);
        let rows: String = members
            .iter()
            .map(|m| {
                let current = if m.id == person { " aria-current=\"true\"" } else { "" };
                let letters: String = m
                    .roles
                    .iter()
                    .map(|l| format!("<span class=\"letter\">{}</span>", esc(l)))
                    .collect();
                let can = if m.can.is_read() {
                    String::new()
                } else {
                    format!(" · {}", m.can.label())
                };
                format!(
                    "<li><a class=\"who\" href=\"?i={id}&amp;p={}#band\"{current}>{}</a> {letters} <span class=\"muted\">· {}{can}</span></li>",
                    esc(&m.id),
                    esc(&m.name),
                    state(m)
                )
            })
            .collect();
        let list = if members.is_empty() {
            "<p class=\"muted\">Jeder Musiker bekommt einen eigenen Zugang: Songs, Sets und Abläufe lesen, der Bühne folgen und eintragen, wann er kann. Ob er auch Sets oder Songs bearbeiten darf, stellst du je Musiker ein.</p>".to_string()
        } else {
            format!("<ul class=\"rows\">{rows}</ul>")
        };
        let add = format!(
            "<form class=\"row\" method=\"post\" action=\"./\">{csrf}<input type=\"hidden\" name=\"do\" value=\"member-add\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
             <input name=\"name\" required maxlength=\"60\" placeholder=\"Name\" aria-label=\"Name\" autocomplete=\"off\">\
             <span class=\"roles\" role=\"group\" aria-label=\"Rollen\">{}</span>\
             {}<select name=\"days\" aria-label=\"Wie lange\">{}</select><button>Aufnehmen</button></form>",
            boxes(&[]),
            rights(Can::Read),
            choices(30)
        );
        let Some(m) = members.iter().find(|m| m.id == person) else {
            return format!("{list}\n{add}");
        };
        // Über den Tunnel, wenn es einen gibt; sonst nur im Heimnetz.
        let server = match &self.panel.public_url {
            Some(url) => Some(format!("{url}{}", shown.path)),
            None => chosen.map(|a| {
                format!(
                    "{}{}",
                    address_url(&self.panel.scheme, a, &self.panel.port),
                    shown.path
                )
            }),
        };
        let code = match &server {
            None => String::new(),
            Some(server) => {
                let link = connect_link(server, &m.token);
                let reach = if self.panel.public_url.is_some() {
                    String::new()
                } else {
                    "<p class=\"note\">Nur im Heimnetz erreichbar. Für unterwegs einen Tunnel einrichten und seine Adresse unter <b>Konfiguration → public_url</b> eintragen (siehe Dokumentation).</p>\n  ".to_string()
                };
                format!(
                    "<section>\n  {}\n  {reach}<p>Mit der Kamera scannen, oder in Chordwright unter <b>Einstellungen → Datenquelle → Server hinzufügen</b> „QR-Code scannen“.</p>\n  \
                     <dl>\n    <dt>Adresse</dt><dd class=\"value\">{}</dd>\n    <dt>Token</dt><dd class=\"value\">{}</dd>\n  </dl>\n</section>\n",
                    qr_svg(&link, &format!("QR-Code: Chordwright für {} verbinden", m.name)),
                    esc(server),
                    esc(&m.token)
                )
            }
        };
        let mid = esc(&m.id);
        let hidden = format!(
            "{csrf}<input type=\"hidden\" name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"p\" value=\"{mid}\">"
        );
        let manage = format!(
            "<section>\n  <p><b>{}</b> <span class=\"muted\">· {}</span></p>\n  \
             <form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-edit\">\
             <input name=\"name\" required maxlength=\"60\" value=\"{}\" aria-label=\"Name\">\
             <span class=\"roles\" role=\"group\" aria-label=\"Rollen\">{}</span>{}<button class=\"quiet\">Speichern</button></form>\n  \
             <form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-extend\">\
             <select name=\"days\" aria-label=\"Ab jetzt für\">{}</select><button class=\"quiet\" title=\"Der Token bleibt; verbundene Geräte bleiben verbunden\">Verlängern</button></form>\n  \
             <form class=\"row\" method=\"post\" action=\"./\" onsubmit=\"return confirm('Von der Liste nehmen? Mit diesem Zugang kommt niemand mehr hinein.')\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-remove\">\
             <button class=\"quiet danger\" title=\"Die Antworten bleiben im Ordner\">Von der Liste nehmen</button></form>\n</section>",
            esc(&m.name),
            state(m),
            esc(&m.name),
            boxes(&m.roles),
            rights(m.can),
            choices(if m.until.is_none() { 0 } else { 30 })
        );
        format!("{list}\n{add}\n<div class=\"steps\">\n{code}{manage}\n</div>")
    }

    fn csrf(&self) -> String {
        format!(
            "<input type=\"hidden\" name=\"csrf\" value=\"{}\">",
            esc(&self.secret)
        )
    }

    /// Ein abgeschicktes Formular: wohin danach (`?i=…&m=…`), oder was schiefging.
    fn act(&self, body: &str) -> Result<String, (String, String)> {
        let field = |name: &str| form_field(body, name).unwrap_or_default();
        let id = field("id");
        if field("csrf") != self.secret {
            return Err((
                id,
                "Das Formular ist abgelaufen. Lade die Seite neu und versuch es noch einmal."
                    .into(),
            ));
        }
        let fail = |e: String| (id.clone(), e);
        let person = field("p");
        let days = || field("days").trim().parse::<u32>().unwrap_or(30).min(365);
        match field("do").as_str() {
            "create" => {
                let entry = self
                    .registry
                    .create(&field("name"))
                    .map_err(|e| (String::new(), e))?;
                Ok(format!("?i={}&m=created", entry.id))
            }
            "rename" => {
                self.registry.rename(&id, &field("name")).map_err(fail)?;
                Ok(format!("?i={id}&m=renamed"))
            }
            "token" => {
                self.registry.new_token(&id).map_err(fail)?;
                Ok(format!("?i={id}&m=token"))
            }
            "member-add" => {
                let m = self
                    .registry
                    .add_member(
                        &id,
                        &field("name"),
                        &form_fields(body, "role"),
                        Can::parse(&field("can")),
                        days(),
                        now_ms(),
                    )
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={}&m=member#band", m.id))
            }
            "member-edit" => {
                self.registry
                    .edit_member(
                        &id,
                        &person,
                        &field("name"),
                        &form_fields(body, "role"),
                        Can::parse(&field("can")),
                    )
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={person}&m=saved#band"))
            }
            "member-extend" => {
                self.registry
                    .extend_member(&id, &person, days(), now_ms())
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={person}&m=extended#band"))
            }
            "member-remove" => {
                self.registry.remove_member(&id, &person).map_err(fail)?;
                Ok(format!("?i={id}&m=unmember#band"))
            }
            "remove" => {
                self.registry.remove(&id).map_err(fail)?;
                Ok("?m=removed".into())
            }
            _ => Err((id, "Unbekannte Aktion.".into())),
        }
    }
}

const HEAD: &str = r#"<!doctype html>
<html lang="de">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Chordwright Data</title>
<style>
  :root { color-scheme: light dark; --fg: #1c1c1e; --muted: #6b6b70; --line: #d9d9de; --accent: #0a7c6b; --bg: #fafafa; --card: #fff; --danger: #b3261e; }
  @media (prefers-color-scheme: dark) { :root { --fg: #ececf0; --muted: #a0a0a8; --line: #3a3a40; --accent: #4cc7b2; --bg: #111214; --card: #1a1b1e; --danger: #f2b8b5; } }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 24px 16px; background: var(--bg); color: var(--fg); font: 15px/1.45 system-ui, sans-serif; }
  main { max-width: 860px; margin: 0 auto; }
  header { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 16px; margin: 0 0 16px; }
  h1 { font-size: 20px; margin: 0; }
  header p { margin: 0; font-size: 13px; }
  h2 { font-size: 17px; margin: 24px 0 12px; }
  h3 { font-size: 15px; margin: 20px 0 10px; display: flex; align-items: center; gap: 8px; }
  h3 small { color: var(--muted); font-weight: normal; }
  .muted { color: var(--muted); }
  .libs { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 12px; }
  .lib { display: flex; flex-direction: column; gap: 2px; min-width: 0; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--card); color: var(--fg); text-decoration: none; }
  .lib > * { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .lib[aria-current] { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .lib .nums { font-size: 14px; }
  .lib .muted { font-size: 13px; }
  .lib.new { gap: 8px; }
  .path { font-family: ui-monospace, monospace; font-size: 12px !important; }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: var(--accent); margin-right: 6px; vertical-align: middle; }
  article { margin-top: 8px; }
  .steps { display: flex; flex-wrap: wrap; gap: 24px; }
  section { flex: 1 1 280px; min-width: 0; }
  .n { display: inline-grid; place-items: center; width: 22px; height: 22px; border-radius: 50%; background: var(--accent); color: var(--bg); font-size: 13px; }
  .qr { display: block; width: 100%; max-width: 260px; height: auto; border-radius: 8px; }
  p { margin: 12px 0; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 12px 0; }
  dt { color: var(--muted); }
  dd { margin: 0; min-width: 0; }
  .value { font-family: ui-monospace, monospace; font-size: 13px; overflow-wrap: anywhere; }
  a { color: var(--accent); }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 20px; }
  .chips a { padding: 6px 12px; border: 1px solid var(--line); border-radius: 999px; text-decoration: none; color: var(--fg); font-size: 13px; }
  .chips a[aria-current] { border-color: var(--accent); color: var(--accent); }
  .tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(130px, 1fr)); gap: 12px; }
  .tile { display: flex; flex-direction: column; padding: 10px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--card); }
  .big { font-size: 22px; font-weight: 600; font-variant-numeric: tabular-nums; }
  .rows { list-style: none; margin: 0; padding: 0; }
  .rows li { padding: 6px 0; border-bottom: 1px solid var(--line); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .rows li:last-child { border-bottom: 0; }
  .what { font-weight: 500; }
  .note { color: var(--muted); }
  form { margin: 0; }
  .row { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 10px; }
  .row input { flex: 1 1 200px; min-width: 0; }
  .roles { display: inline-flex; flex-wrap: wrap; gap: 6px; }
  .roles label { display: inline-flex; align-items: center; gap: 4px; min-height: 36px; padding: 0 10px; border: 1px solid var(--line); border-radius: 999px; font-weight: 600; cursor: pointer; }
  .roles input { margin: 0; flex: none; min-height: 0; padding: 0; }
  .letter { display: inline-grid; place-items: center; width: 22px; height: 22px; border: 1px solid var(--line); border-radius: 6px; font-size: 12px; font-weight: 600; }
  .who { font-weight: 600; }
  .who[aria-current] { text-decoration: underline; }
  input, select { font: inherit; padding: 8px 12px; min-height: 36px; border: 1px solid var(--line); border-radius: 8px; background: var(--bg); color: var(--fg); }
  button { font: inherit; padding: 8px 16px; min-height: 36px; border: 1px solid var(--accent); border-radius: 999px; background: var(--accent); color: var(--bg); cursor: pointer; }
  button.quiet { background: transparent; color: var(--fg); border-color: var(--line); }
  button.danger { color: var(--danger); }
  .banner { padding: 10px 14px; border-radius: 12px; border: 1px solid var(--accent); }
  .banner.error { border-color: var(--danger); color: var(--danger); }
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

/// Kopf und, bei einem POST, der Körper der Anfrage — höchstens 16 KB.
fn read_request(stream: &mut TcpStream) -> (String, String) {
    const LIMIT: usize = 16 * 1024;
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    let split = loop {
        if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if raw.len() >= LIMIT {
            break raw.len();
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break raw.len(),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
        .min(LIMIT);
    let mut body = raw.get(split + 4..).map(<[u8]>::to_vec).unwrap_or_default();
    while body.len() < length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    body.truncate(length);
    (head, String::from_utf8_lossy(&body).into_owned())
}

fn answer(mut stream: TcpStream, ui: &Ui) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let (head, body) = read_request(&mut stream);
    let line = head.lines().next().unwrap_or_default().to_string();
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
    let html = "content-type: text/html; charset=utf-8\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\n";
    if !stream.peer_addr().is_ok_and(|a| allowed(a.ip())) {
        return respond(&mut stream, "403 Forbidden", "", "");
    }
    if path != "/" {
        return respond(&mut stream, "404 Not Found", "", "");
    }
    if method == "POST" {
        return match ui.act(&body) {
            // Relativ: unter Ingress liegt die Seite unter einem Pfad, den nur Home Assistant kennt.
            Ok(to) => respond(
                &mut stream,
                "303 See Other",
                &format!("location: {to}\r\ncache-control: no-store\r\n"),
                "",
            ),
            Err((id, e)) => respond(
                &mut stream,
                "400 Bad Request",
                html,
                &ui.page(&id, "", 0, None, Some(&e)),
            ),
        };
    }
    if method != "GET" {
        return respond(&mut stream, "405 Method Not Allowed", "", "");
    }
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(name)?.strip_prefix('='))
            .map(form_decode)
    };
    // `parseInt(a) || 0`: führende Ziffern zählen, alles andere ist die erste Adresse.
    let pick = param("a")
        .map(|v| {
            v.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(0);
    let id = param("i").unwrap_or_default();
    let message = param("m").and_then(|m| notice(&m));
    respond(
        &mut stream,
        "200 OK",
        html,
        &ui.page(&id, &param("p").unwrap_or_default(), pick, message, None),
    );
}

/// Startet die Web-UI auf `port`, in Threads neben dem Server.
pub fn start(port: u16, ui: Ui) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let ui = Arc::new(ui);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let ui = ui.clone();
            std::thread::spawn(move || answer(stream, &ui));
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
        let spots = places(
            &["ha.local".into(), "10.0.0.9".into()],
            "https",
            "4174",
            Some("https://band.example.org"),
        );
        assert_eq!(
            spots,
            [
                ("10.0.0.9".to_string(), "https://10.0.0.9:4174".to_string()),
                ("ha.local".to_string(), "https://ha.local:4174".to_string()),
                (
                    "band.example.org".to_string(),
                    "https://band.example.org".to_string()
                ),
            ]
        );
        assert_eq!(places(&[], "https", "4174", None), []);
        assert!(allowed("::ffff:172.30.32.2".parse().unwrap()));
        assert!(!allowed("192.168.1.2".parse().unwrap()));
    }

    #[test]
    fn forms_and_figures() {
        assert_eq!(form_decode("Die+Gro%C3%9Fen%21"), "Die Großen!");
        assert_eq!(form_decode("50%"), "50%");
        assert_eq!(
            form_field("do=create&name=Chor+2", "name").as_deref(),
            Some("Chor 2")
        );
        assert_eq!(ago(0, 30_000), "gerade eben");
        assert_eq!(ago(0, 5 * 60_000), "vor 5 Min.");
        assert_eq!(ago(0, 3 * 86_400_000), "vor 3 Tagen");
        assert_eq!(size(1_300_000), "1,2 MB");
        assert_eq!(size(2048), "2 KB");
    }
}
