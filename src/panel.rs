//! Die Web-UI des Add-ons (Home Assistant: „Web-UI öffnen", per Ingress):
//! alle Bibliotheken auf einen Blick, mit Songs, Sets, verbundenen Geräten,
//! letzter Änderung und Sicherungen; zu der gewählten die QR-Codes zum
//! Verbinden (bei eigener CA einer fürs Zertifikat davor), die letzten
//! Änderungen, offene Bühnen, Vorschläge der Musiker zum Übernehmen — und für
//! die weiteren Bibliotheken (instances.rs)
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

use chordwright_server::{
    Can, ChangeState, Hub, Member, ProposalStatus, Stats, Voice, ROLES, USAGE_KINDS,
};
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

/// Die Musiker in der Liste: wer mehr darf, steht oben (Admin, Songs und Sets, dann
/// Sets, dann wer vorschlagen darf, dann nur lesen), innerhalb gleicher
/// Rechte nach Namen, ohne auf Groß- und Kleinschreibung zu achten.
pub fn order_members(members: &mut [Member]) {
    let rank = |c: Can| match c {
        Can::Admin => 0,
        Can::All => 1,
        Can::Sets => 2,
        Can::Suggest => 3,
        Can::Read => 4,
    };
    members.sort_by(|a, b| {
        rank(a.can)
            .cmp(&rank(b.can))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
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

/// Ein QR-Code zum Weitergeben: dazu Knöpfe, ihn als Bild zu kopieren, zu
/// speichern oder (am Handy) zu teilen, und den Link zu kopieren — das
/// Skript `SHARE` zeichnet ihn dafür als PNG, mit `caption` darunter. Nur für
/// die Zugänge der Musiker: der Token der Bibliothek darf alles.
fn qr_share(link: &str, label: &str, caption: &str, file: &str) -> String {
    format!(
        "<div class=\"share\" data-link=\"{}\" data-caption=\"{}\" data-file=\"{}\">{}\
         <div class=\"tools\" hidden>\
         <button type=\"button\" class=\"quiet\" data-do=\"image\" title=\"Als Bild in die Zwischenablage, zum Einfügen in eine Nachricht\">Bild kopieren</button>\
         <button type=\"button\" class=\"quiet\" data-do=\"link\" title=\"Wer den Link am Handy antippt, ist gleich verbunden\">Link kopieren</button>\
         <button type=\"button\" class=\"quiet\" data-do=\"save\">Speichern</button>\
         <button type=\"button\" class=\"quiet\" data-do=\"share\" hidden>Teilen</button>\
         </div></div>",
        esc(link),
        esc(caption),
        esc(file),
        qr_svg(link, label)
    )
}

/// Ein Name als Dateiname, nur in ASCII: Chromium verwirft einen Namen mit
/// Umlauten im `download`-Attribut und speichert „download“.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'ä' => "ae".into(),
            'ö' => "oe".into(),
            'ü' => "ue".into(),
            'Ä' => "Ae".into(),
            'Ö' => "Oe".into(),
            'Ü' => "Ue".into(),
            'ß' => "ss".into(),
            'A'..='Z' | 'a'..='z' | '0'..='9' | ' ' | '-' | '.' | '(' | ')' => c.to_string(),
            _ => "_".into(),
        })
        .collect()
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

/// Höchstens so viele Fragen am Tag lassen sich einstellen.
const AI_MAX: u32 = 100;

/// „840“, „12,4 k“, „1,3 Mio.“
fn tokens(n: u64) -> String {
    let one = |v: f64| format!("{v:.1}").replace('.', ",");
    match n {
        0..=999 => n.to_string(),
        1000..=999_999 => format!("{} k", one(n as f64 / 1000.0)),
        _ => format!("{} Mio.", one(n as f64 / 1_000_000.0)),
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

/// Wofür eine Anfrage war (usage.rs `KINDS`), in derselben Reihenfolge und
/// Farbe überall.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "read" => "Lesen",
        "write" => "Schreiben",
        "live" => "Live-Verbindung",
        "stage" => "Bühne",
        "assistant" => "KI",
        "backups" => "Sicherungen",
        "check" => "Verbindungstest",
        "denied" => "Abgewiesen",
        _ => "Sonstiges",
    }
}

fn kind_title(kind: &str) -> &'static str {
    match kind {
        "read" => "Songs, Sets und Einstellungen abgerufen",
        "write" => "Etwas gespeichert oder gelöscht",
        "live" => "Eine App hat sich für Änderungen angemeldet",
        "stage" => "Gemeinsame Bühne: leiten und folgen",
        "assistant" => "Fragen an die KI im Ablaufplaner",
        "backups" => "Sicherungen angesehen, angelegt oder zurückgeholt",
        "check" => "Erreichbarkeit geprüft",
        "denied" => "Ohne gültigen Token, oder ein abgelaufener Zugang",
        _ => "",
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        "owner" => "Token der Bibliothek",
        "guest" => "Browser mit Code",
        "member" => "Musiker",
        "denied" => "ohne gültigen Zugang",
        _ => "",
    }
}

/// Die Wahl des Zeitraums für „Nutzung“, in Tagen.
const USAGE_DAYS: [(u32, &str); 3] = [(1, "24 Std."), (7, "7 Tage"), (30, "30 Tage")];
/// So viele Personen bekommen eine eigene Farbe, der Rest ist „Weitere“.
const USAGE_SLOTS: usize = 5;

/// „12 ms“, „3,4 s“, „2 Min.“ — Mikrosekunden zum Lesen.
fn busy(us: u64) -> String {
    let ms = us as f64 / 1000.0;
    if ms < 1000.0 {
        format!("{} ms", ms.round())
    } else if ms < 60_000.0 {
        format!("{:.1} s", ms / 1000.0).replace('.', ",")
    } else {
        format!("{} Min.", (ms / 60_000.0).round())
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0 %".into();
    }
    let p = part as f64 * 100.0 / whole as f64;
    if p > 0.0 && p < 1.0 {
        "< 1 %".into()
    } else {
        format!("{} %", p.round())
    }
}

/// Ein Zeitpunkt: „vor 5 Min.“, im Browser zu Datum und Uhrzeit des Geräts
/// (das Add-on weiß die Zeitzone nicht).
fn moment(at: i64, now: i64) -> String {
    format!("<time data-t=\"{at}\">{}</time>", ago(at, now))
}

/// Ein Ring aus Anteilen: (Wert, Farbe, Name). Jeder Teil trägt seinen
/// Namen als Tooltip.
fn donut(parts: &[(u64, String, String)], label: &str) -> String {
    let total: u64 = parts.iter().map(|p| p.0).sum();
    let mut rings = String::new();
    if total == 0 {
        rings.push_str("<circle class=\"track\" cx=\"21\" cy=\"21\" r=\"15.9155\"/>");
    } else {
        let shown: Vec<_> = parts.iter().filter(|p| p.0 > 0).collect();
        // Ein Spalt in der Farbe der Fläche zwischen den Teilen.
        let gap = if shown.len() > 1 { 0.8 } else { 0.0 };
        let mut offset = 0.0;
        for (n, color, name) in shown {
            let share = *n as f64 * 100.0 / total as f64;
            let len = (share - gap).max(0.4);
            rings.push_str(&format!(
                "<circle cx=\"21\" cy=\"21\" r=\"15.9155\" stroke=\"var({color})\" stroke-dasharray=\"{len:.3} {:.3}\" stroke-dashoffset=\"{:.3}\">\
                 <title>{}: {} ({})</title></circle>",
                100.0 - len,
                25.0 - offset,
                esc(name),
                n,
                percent(*n, total)
            ));
            offset += share;
        }
    }
    format!(
        "<svg class=\"donut\" viewBox=\"0 0 42 42\" role=\"img\" aria-label=\"{}\">{rings}\
         <text x=\"21\" y=\"21\" class=\"donut-n\">{total}</text><text x=\"21\" y=\"27\" class=\"donut-k\">Anfragen</text></svg>",
        esc(label)
    )
}

/// Die Legende zum Ring: Farbe, Name, Zahl, Anteil.
fn legend(parts: &[(u64, String, String)], titles: &[String]) -> String {
    let total: u64 = parts.iter().map(|p| p.0).sum();
    let rows: String = parts
        .iter()
        .zip(titles.iter().chain(std::iter::repeat(&String::new())))
        .map(|((n, color, name), title)| {
            let title = if title.is_empty() {
                String::new()
            } else {
                format!(" title=\"{}\"", esc(title))
            };
            format!(
                "<li{title}><span class=\"swatch\" style=\"background:var({color})\"></span>\
                 <span class=\"name\">{}</span><span class=\"num\">{n}</span><span class=\"num muted\">{}</span></li>",
                esc(name),
                percent(*n, total)
            )
        })
        .collect();
    format!("<ul class=\"legend\">{rows}</ul>")
}

/// Säulen über die Zeit, älteste links. `label(i)`: wann Säule `i` war.
fn bars(values: &[u64], label: impl Fn(usize) -> String, axis: [&str; 3]) -> String {
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let cols: String = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let h = if *v == 0 { 0.0 } else { (*v as f64 * 100.0 / max as f64).max(2.0) };
            format!(
                "<span class=\"bar\" title=\"{}: {}\"><span style=\"height:{h:.1}%\"></span></span>",
                esc(&label(i)),
                plural(*v, "Anfrage", "Anfragen")
            )
        })
        .collect();
    format!(
        "<div class=\"bars\" role=\"img\" aria-label=\"Anfragen über die Zeit, höchstens {max}\">\
         <span class=\"bars-max muted\">{max}</span>{cols}</div>\
         <div class=\"bars-axis muted\"><span>{}</span><span>{}</span><span>{}</span></div>",
        axis[0], axis[1], axis[2]
    )
}

/// Was der Server-Prozess gerade braucht, und die Last des ganzen Rechners —
/// unter Linux (Home Assistant) aus /proc, sonst nichts.
struct Machine {
    /// Rechenzeit seit dem Start, ms.
    cpu_ms: u64,
    rss: u64,
    load: Option<f64>,
}

fn machine() -> Option<Machine> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Nach dem Namen in Klammern: Feld 3 ist das erste, utime/stime 14/15.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks = |i: usize| fields.get(i).and_then(|f| f.parse::<u64>().ok());
    // 100 Ticks je Sekunde: so unter Linux überall, wo Home Assistant läuft.
    let cpu_ms = (ticks(11)? + ticks(12)?) * 10;
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)?;
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|l| l.split_whitespace().next()?.parse::<f64>().ok());
    Some(Machine { cpu_ms, rss, load })
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
        "ai-key" => "Schlüssel gespeichert. Die KI im Ablaufplaner läuft jetzt über diesen Server.",
        "ai-limits" => "Limits gespeichert.",
        "ai-off" => "Schlüssel entfernt. Die KI ist aus.",
        "accepted" => "Übernommen. Die Änderungen stehen jetzt in der Bibliothek.",
        "partly" => "Teilweise übernommen. Was sich inzwischen geändert hat, ist offen geblieben.",
        "rejected" => "Abgelehnt. In der Bibliothek hat sich nichts geändert.",
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
    #[allow(clippy::too_many_arguments)]
    pub fn page(
        &self,
        id: &str,
        person: &str,
        pick: usize,
        message: Option<&str>,
        error: Option<&str>,
        // Wohin die Seite nach dem Laden springt: `band`, `vorschlaege`, `ki`, `nutzung` oder leer.
        to: &str,
        // Der Zeitraum unter „Nutzung“, in Tagen.
        days: u32,
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
                    let waiting = if st.proposals > 0 {
                        format!(
                            " · <span class=\"waiting\" title=\"Warten darauf, dass du sie annimmst\">{}</span>",
                            plural(st.proposals as u64, "Vorschlag", "Vorschläge")
                        )
                    } else {
                        String::new()
                    };
                    (
                        format!(
                            "{} · {} · <span title=\"Gerade verbundene Apps\">{}</span>{waiting}",
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
        let ki = self.ai_section(sel);
        let suggestions = self.proposals_section(sel, now);
        let usage = self.usage_section(sel, days, now);

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
        // Zur Band hinunter — mit einem Skript statt `#band`: unter Ingress
        // liegt die Seite in einem Iframe auf derselben Adresse wie Home
        // Assistant, und ein Sprung zum Anker scrollt auch dessen Seite mit;
        // der Iframe rutscht aus dem Bild, es bleibt schwarz. `scrollTo`
        // bewegt nur dieses Fenster.
        let scroll = match to {
            "band" | "vorschlaege" | "ki" | "nutzung" => format!(
                "<script>addEventListener('load',()=>{{const b=document.getElementById('{to}');if(b)scrollTo(0,b.getBoundingClientRect().top+scrollY-16)}})</script>\n"
            ),
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
             <h3 id=\"band\">Band</h3>\n{band}\n<h3 id=\"vorschlaege\">Vorschläge</h3>\n{suggestions}\n<h3 id=\"ki\">KI</h3>\n{ki}\n<h3>Zahlen</h3>\n{figures}\n<h3 id=\"nutzung\">Nutzung</h3>\n{usage}\n<h3>Verwalten</h3>\n{manage}\n</article>\n</main>\n{LOCAL_TIME}{SHARE}{scroll}</body>\n</html>\n",
            esc(&shown.name)
        )
    }

    /// Wer die Bibliothek wann genutzt hat und was das den Rechner kostet:
    /// Zahlen, wer (Ring), wofür (Ring), Verlauf, je Person, die Besuche.
    fn usage_section(&self, sel: &str, days: u32, now: i64) -> String {
        let Some(r) = self.hub.usage((!sel.is_empty()).then_some(sel), days) else {
            return String::new();
        };
        let id_q = if sel.is_empty() {
            String::new()
        } else {
            format!("i={}&amp;", esc(sel))
        };
        let chips: String = USAGE_DAYS
            .iter()
            .map(|(d, label)| {
                let current = if *d == r.days {
                    " aria-current=\"true\""
                } else {
                    ""
                };
                format!("<a href=\"?{id_q}u={d}&amp;to=nutzung\"{current}>{label}</a>")
            })
            .collect();
        let chips = format!("<nav class=\"chips\" aria-label=\"Zeitraum\">{chips}</nav>");

        let mut tiles = vec![
            (
                "Anfragen".to_string(),
                r.total.requests.to_string(),
                "An diese Bibliothek, im gewählten Zeitraum".to_string(),
            ),
            (
                "Personen".into(),
                r.people
                    .iter()
                    .filter(|p| p.role != "denied")
                    .count()
                    .to_string(),
                "Musiker, Apps mit Namen und Browser mit Code".into(),
            ),
            (
                "Rechenzeit".into(),
                busy(r.total.busy_us),
                "So lange war der Server mit diesen Anfragen beschäftigt".into(),
            ),
            (
                "Übertragen".into(),
                size(r.total.bytes),
                "Gesendet und empfangen, ohne Live-Verbindungen".into(),
            ),
        ];
        if let Some(m) = machine() {
            let up = (now - self.hub.started()).max(1) as f64;
            let cpu = m.cpu_ms as f64 * 100.0 / up;
            tiles.push((
                "CPU Ø".into(),
                format!("{:.1} %", cpu).replace('.', ","),
                "Anteil eines Kerns, den das ganze Add-on seit dem Start im Schnitt braucht".into(),
            ));
            tiles.push((
                "Arbeitsspeicher".into(),
                size(m.rss),
                "Was das Add-on gerade belegt".into(),
            ));
            if let Some(load) = m.load {
                let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
                tiles.push((
                    "Last des Rechners".into(),
                    format!("{load:.2}").replace('.', ","),
                    format!("Home Assistant samt allem, letzte Minute; {cores} Kerne — unter {cores} hat er Luft"),
                ));
            }
        }
        let tiles: String = tiles
            .iter()
            .map(|(k, v, title)| {
                format!(
                    "<div class=\"tile\" title=\"{}\"><span class=\"big\">{}</span><span class=\"muted\">{k}</span></div>",
                    esc(title),
                    esc(v)
                )
            })
            .collect();

        if r.total.requests == 0 {
            return format!(
                "{chips}<div class=\"tiles\">{tiles}</div>\n<p class=\"muted\">In diesem Zeitraum hat niemand auf die Bibliothek zugegriffen.</p>"
            );
        }

        // Wer: die ersten fünf mit eigener Farbe, der Rest zusammen.
        let mut who: Vec<(u64, String, String)> = r
            .people
            .iter()
            .take(USAGE_SLOTS)
            .enumerate()
            .map(|(i, p)| (p.tally.requests, format!("--c{}", i + 1), p.name.clone()))
            .collect();
        let rest: u64 = r
            .people
            .iter()
            .skip(USAGE_SLOTS)
            .map(|p| p.tally.requests)
            .sum();
        if rest > 0 {
            who.push((
                rest,
                "--muted".into(),
                format!("{} weitere", r.people.len() - USAGE_SLOTS),
            ));
        }
        let who_titles: Vec<String> = r
            .people
            .iter()
            .take(USAGE_SLOTS)
            .map(|p| role_label(&p.role).to_string())
            .collect();
        // Wofür: jede Art behält ihre Farbe, auch wenn eine fehlt.
        let what: Vec<(u64, String, String)> = USAGE_KINDS
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                let n = r.total.kinds.get(*k).copied().unwrap_or(0);
                (n > 0).then(|| (n, format!("--c{}", i + 1), kind_label(k).to_string()))
            })
            .collect();
        let what_titles: Vec<String> = USAGE_KINDS
            .iter()
            .filter(|k| r.total.kinds.get(**k).copied().unwrap_or(0) > 0)
            .map(|k| kind_title(k).to_string())
            .collect();
        let pies = format!(
            "<div class=\"pies\">\
             <figure><figcaption>Wer</figcaption><div class=\"pie\">{}{}</div></figure>\
             <figure><figcaption>Wofür</figcaption><div class=\"pie\">{}{}</div></figure></div>",
            donut(&who, "Anfragen je Person"),
            legend(&who, &who_titles),
            donut(&what, "Anfragen je Art"),
            legend(&what, &what_titles)
        );

        let timeline = if r.days == 1 {
            bars(
                &r.hourly,
                |i| match 23 - i {
                    0 => "diese Stunde".into(),
                    1 => "vor 1 Std.".into(),
                    n => format!("vor {n} Std."),
                },
                ["vor 24 Std.", "vor 12 Std.", "jetzt"],
            )
        } else {
            let n = r.daily.len();
            let mid = format!("vor {} Tagen", n / 2);
            let first = format!("vor {n} Tagen");
            bars(
                &r.daily,
                |i| match n - 1 - i {
                    0 => "letzte 24 Std.".into(),
                    1 => "vor 1–2 Tagen".into(),
                    d => format!("vor {d}–{} Tagen", d + 1),
                },
                [&first, &mid, "jetzt"],
            )
        };

        let people: String = r
            .people
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let color = if i < USAGE_SLOTS {
                    format!("--c{}", i + 1)
                } else {
                    "--muted".into()
                };
                let kinds: String = USAGE_KINDS
                    .iter()
                    .filter_map(|k| {
                        let n = p.tally.kinds.get(*k)?;
                        Some(format!("{} {n}", kind_label(k)))
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                format!(
                    "<tr><td><span class=\"swatch\" style=\"background:var({color})\"></span><b>{}</b><br><span class=\"muted small\">{}</span></td>\
                     <td class=\"num\" title=\"{}\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                    esc(&p.name),
                    role_label(&p.role),
                    esc(&kinds),
                    p.tally.requests,
                    busy(p.tally.busy_us),
                    size(p.tally.bytes),
                    moment(p.last, now)
                )
            })
            .collect();
        let table = format!(
            "<div class=\"scroll\"><table class=\"usage\"><thead><tr><th>Wer</th><th class=\"num\">Anfragen</th>\
             <th class=\"num\" title=\"So lange war der Server mit den Anfragen beschäftigt\">Rechenzeit</th>\
             <th class=\"num\">Übertragen</th><th>Zuletzt</th></tr></thead><tbody>{people}</tbody></table></div>"
        );

        let visits: String = r
            .visits
            .iter()
            .take(30)
            .map(|v| {
                let kinds: Vec<&str> = USAGE_KINDS
                    .iter()
                    .filter(|k| v.kinds.contains_key(**k))
                    .map(|k| kind_label(k))
                    .collect();
                let span = if v.to - v.from < 60_000 {
                    moment(v.from, now)
                } else {
                    format!(
                        "{} – <time data-t=\"{}\" data-only=\"time\">{} Min. lang</time>",
                        moment(v.from, now),
                        v.to,
                        (v.to - v.from) / 60_000
                    )
                };
                format!(
                    "<li><span class=\"what\">{}</span> <span class=\"muted\">{} · {} · {span}</span></li>",
                    esc(&v.name),
                    kinds.join(", "),
                    plural(v.requests, "Anfrage", "Anfragen")
                )
            })
            .collect();

        format!(
            "{chips}<div class=\"tiles\">{tiles}</div>\n{pies}\n\
             <h4>Verlauf</h4>\n{timeline}\n<h4>Je Person</h4>\n{table}\n\
             <h4>Besuche</h4>\n<ul class=\"rows\">{visits}</ul>"
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
        // Ob er mitspielt: nach Terminen gefragt und in Sets eingeteilt wird.
        // Ohne liest er nur mit (und schlägt, wenn er darf, Songs vor).
        let plays_box = |on: bool| -> String {
            format!(
                "<label class=\"plays\" title=\"Wird nach Terminen gefragt und in Sets eingeteilt. Ohne Haken liest er nur mit.\"><input type=\"checkbox\" name=\"plays\" value=\"1\"{}>spielt mit</label>",
                if on { " checked" } else { "" }
            )
        };
        // Die Stimme eines Sängers: danach fragt die App, wer die Hauptstimme
        // singt, und schlägt eine Tonart vor.
        let voice_pick = |on: Option<Voice>| -> String {
            let none = if on.is_none() { " selected" } else { "" };
            let options: String = [Voice::Low, Voice::High]
                .iter()
                .map(|v| {
                    let sel = if on == Some(*v) { " selected" } else { "" };
                    format!("<option value=\"{}\"{sel}>{}</option>", v.id(), v.label())
                })
                .collect();
            format!(
                "<select name=\"voice\" aria-label=\"Stimme\" title=\"Für Sänger: die App fragt, wer die Hauptstimme singt, und schlägt eine Tonart vor\"><option value=\"\"{none}>Stimme –</option>{options}</select>"
            )
        };
        let state = |m: &Member| match m.until {
            _ if m.expired(now) => "abgelaufen".to_string(),
            None => "bis auf Widerruf".to_string(),
            Some(until) => left(until, now),
        };
        let mut members = self.registry.members(sel);
        order_members(&mut members);
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
                let can = if m.plays {
                    can
                } else {
                    format!("{can} · nicht in der Planung")
                };
                let can = match m.ai {
                    Some(n) => format!("{can} · KI {n}/Tag"),
                    None => can,
                };
                format!(
                    "<li><a class=\"who\" href=\"?i={id}&amp;p={}&amp;to=band\"{current}>{}</a> {letters} <span class=\"muted\">· {}{can}</span></li>",
                    esc(&m.id),
                    esc(&m.name),
                    state(m)
                )
            })
            .collect();
        let find = if members.len() > 8 {
            "<input type=\"search\" class=\"find\" placeholder=\"Suchen\" aria-label=\"Musiker suchen\" autocomplete=\"off\">"
        } else {
            ""
        };
        let list = if members.is_empty() {
            "<p class=\"muted\">Jeder Musiker bekommt einen eigenen Zugang: Songs, Sets und Abläufe lesen, der Bühne folgen und eintragen, wann er kann. Ob er auch Sets oder Songs bearbeiten darf, stellst du je Musiker ein.</p>".to_string()
        } else {
            format!("{find}<ul class=\"rows crew\">{rows}</ul>")
        };
        let add = format!(
            "<form class=\"row\" method=\"post\" action=\"./\">{csrf}<input type=\"hidden\" name=\"do\" value=\"member-add\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
             <input name=\"name\" required maxlength=\"60\" placeholder=\"Name\" aria-label=\"Name\" autocomplete=\"off\">\
             <span class=\"roles\" role=\"group\" aria-label=\"Rollen\">{}</span>\
             {}{}{}<select name=\"days\" aria-label=\"Wie lange\">{}</select><button>Aufnehmen</button></form>",
            boxes(&[]),
            rights(Can::Read),
            plays_box(true),
            voice_pick(None),
            choices(30)
        );
        let Some(m) = members.iter().find(|m| m.id == person) else {
            let hint = if members.is_empty() {
                ""
            } else {
                "<p class=\"muted\">Einen Namen wählen: dann stehen hier sein QR-Code zum Weitergeben, Rollen und wie lange er dabei ist.</p>"
            };
            return format!("<div class=\"band\">\n<div>{list}\n{add}</div>\n<div class=\"pick\">{hint}</div>\n</div>");
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
                // Ohne Tunnel und mit eigener CA braucht auch der Musiker
                // erst das Zertifikat, sonst lässt sein Gerät die App nicht
                // an den Server. Über den Tunnel hat Cloudflare ein echtes.
                let two = self.panel.own_ca && self.panel.public_url.is_none();
                let ca_step = if two {
                    let root = server.strip_suffix(&shown.path).unwrap_or(server);
                    let ca = format!("{root}/ca.crt");
                    format!(
                        "<section>\n  <h3><span class=\"n\">1</span>Zertifikat <small>einmal pro Gerät</small></h3>\n  {}\n  \
                         <p>Zuerst: mit der Kamera scannen und öffnen. Am iPhone danach unter <b>Einstellungen → Allgemein → Info →\n  \
                         Zertifikatsvertrauenseinstellungen</b> „Chordwright lokale CA“ einschalten.</p>\n</section>\n",
                        qr_share(
                            &ca,
                            "QR-Code: Zertifikat laden",
                            "Chordwright: 1. Zertifikat",
                            "Chordwright - 1 Zertifikat.png"
                        )
                    )
                } else {
                    String::new()
                };
                format!(
                    "{ca_step}<section>\n  <h3>{}Verbinden <small>öffnet Chordwright mit dem Zugang von {}</small></h3>\n  {}\n  {reach}<p>Mit der Kamera scannen, oder in Chordwright unter <b>Einstellungen → Datenquelle → Server hinzufügen</b> „QR-Code scannen“.</p>\n  \
                     <dl>\n    <dt>Adresse</dt><dd class=\"value\">{}</dd>\n    <dt>Token</dt><dd class=\"value\">{}</dd>\n  </dl>\n</section>\n",
                    if two { "<span class=\"n\">2</span>" } else { "" },
                    esc(&m.name),
                    qr_share(
                        &link,
                        &format!("QR-Code: Chordwright für {} verbinden", m.name),
                        &if two {
                            format!("Chordwright: 2. Verbinden für {}", m.name)
                        } else {
                            format!("Chordwright für {}", m.name)
                        },
                        &format!(
                            "Chordwright - {}{}.png",
                            if two { "2 " } else { "" },
                            file_name(&m.name)
                        )
                    ),
                    esc(server),
                    esc(&m.token)
                )
            }
        };
        let mid = esc(&m.id);
        let hidden = format!(
            "{csrf}<input type=\"hidden\" name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"p\" value=\"{mid}\">"
        );
        let ai_input = format!(
            "<input name=\"ai\" type=\"number\" min=\"0\" max=\"{AI_MAX}\" value=\"{}\" placeholder=\"KI/Tag\" aria-label=\"KI-Fragen am Tag\" title=\"Fragen an die KI im Ablaufplaner je Tag. Leer: wie für alle, 0: keine\" class=\"short\">",
            m.ai.map(|n| n.to_string()).unwrap_or_default()
        );
        let manage = format!(
            "<section>\n  <p><b>{}</b> <span class=\"muted\">· {}</span></p>\n  \
             <form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-edit\">\
             <input name=\"name\" required maxlength=\"60\" value=\"{}\" aria-label=\"Name\">\
             <span class=\"roles\" role=\"group\" aria-label=\"Rollen\">{}</span>{}{}{}{}<button class=\"quiet\">Speichern</button></form>\n  \
             <form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-extend\">\
             <select name=\"days\" aria-label=\"Ab jetzt für\">{}</select><button class=\"quiet\" title=\"Der Token bleibt; verbundene Geräte bleiben verbunden\">Verlängern</button></form>\n  \
             <form class=\"row\" method=\"post\" action=\"./\" onsubmit=\"return confirm('Von der Liste nehmen? Mit diesem Zugang kommt niemand mehr hinein.')\">{hidden}<input type=\"hidden\" name=\"do\" value=\"member-remove\">\
             <button class=\"quiet danger\" title=\"Die Antworten bleiben im Ordner\">Von der Liste nehmen</button></form>\n</section>",
            esc(&m.name),
            state(m),
            esc(&m.name),
            boxes(&m.roles),
            rights(m.can),
            plays_box(m.plays),
            voice_pick(m.voice),
            ai_input,
            choices(if m.until.is_none() { 0 } else { 30 })
        );
        format!("<div class=\"band\">\n<div>{list}\n{add}</div>\n<div class=\"pick\">\n{code}{manage}\n</div>\n</div>")
    }

    /// Die KI im Ablaufplaner: der Schlüssel, wie oft am Tag, wer wie viel
    /// gefragt hat. Ohne Schlüssel nur das Feld dafür.
    fn ai_section(&self, sel: &str) -> String {
        let csrf = self.csrf();
        let id = esc(sel);
        let hidden = format!("{csrf}<input type=\"hidden\" name=\"id\" value=\"{id}\">");
        let key_form = |label: &str| {
            format!(
                "<form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"ai-key\">\
                 <input name=\"key\" type=\"password\" required autocomplete=\"off\" placeholder=\"sk-ant-…\" aria-label=\"Anthropic-Schlüssel\" \
                 title=\"Bleibt auf diesem Server: geht nie an die App und nie in eine Sicherung\"><button>{label}</button></form>"
            )
        };
        let Some(a) = self.registry.assistant(sel) else {
            return format!(
                "<p class=\"muted\">Mit einem Anthropic-Schlüssel fragt der Ablaufplaner die KI über diesen Server, mit einem Tageslimit je Musiker. Am besten ein eigener Schlüssel je Bibliothek, aus einem Workspace mit Ausgabenlimit in der Anthropic Console.</p>\n{}",
                key_form("Schlüssel speichern")
            );
        };
        let limits = format!(
            "<form class=\"row\" method=\"post\" action=\"./\">{hidden}<input type=\"hidden\" name=\"do\" value=\"ai-limits\">\
             <label>Je Musiker am Tag <input name=\"daily\" type=\"number\" min=\"0\" max=\"{AI_MAX}\" value=\"{}\" class=\"short\" title=\"Für jeden ohne eigene Zahl in der Band\"></label>\
             <label>Du am Tag <input name=\"owner\" type=\"number\" min=\"0\" max=\"{AI_MAX}\" value=\"{}\" placeholder=\"∞\" class=\"short\" title=\"Leer: ohne Grenze\"></label>\
             <button class=\"quiet\">Speichern</button></form>",
            a.daily,
            a.owner_daily.map(|n| n.to_string()).unwrap_or_default()
        );
        let members = self.registry.members(sel);
        let limit_of = |who: &str| -> String {
            if who == "owner" {
                return a.owner_daily.map_or("∞".into(), |n| n.to_string());
            }
            let m = members.iter().find(|m| m.id == who);
            m.and_then(|m| m.ai).unwrap_or(a.daily).to_string()
        };
        let tally = self
            .hub
            .assistant_tally((!sel.is_empty()).then_some(sel))
            .unwrap_or_default();
        let rows: String = tally
            .iter()
            .map(|t| {
                let name = match (t.who.as_str(), &t.name) {
                    ("owner", _) => "Du".to_string(),
                    (_, Some(n)) => n.clone(),
                    (who, None) => who.to_string(),
                };
                format!(
                    "<li>{} <span class=\"muted\">· heute {} von {} · Monat {} · {} Tokens · {}</span></li>",
                    esc(&name),
                    t.today,
                    limit_of(&t.who),
                    plural(t.month as u64, "Frage", "Fragen"),
                    tokens(t.usage.input + t.usage.output),
                    plural(t.usage.searches, "Suche", "Suchen")
                )
            })
            .collect();
        let figures = if tally.is_empty() {
            "<p class=\"muted\">Diesen Monat noch keine Fragen.</p>".to_string()
        } else {
            format!("<ul class=\"rows\">{rows}</ul>")
        };
        format!(
            "<p><span class=\"value\">{}</span> <span class=\"muted\">· Modell {}</span></p>\n{}\n{limits}\n{figures}\n\
             <form class=\"row\" method=\"post\" action=\"./\" onsubmit=\"return confirm('Schlüssel entfernen? Die KI im Ablaufplaner ist dann aus.')\">{hidden}<input type=\"hidden\" name=\"do\" value=\"ai-remove\">\
             <button class=\"quiet danger\">Schlüssel entfernen</button></form>",
            esc(&a.masked()),
            esc(chordwright_server::ASSISTANT_MODEL),
            key_form("Schlüssel ersetzen")
        )
    }

    /// Was Musiker vorschlagen: offen mit Vergleich, Übernehmen und Ablehnen;
    /// darunter die zuletzt entschiedenen.
    fn proposals_section(&self, sel: &str, now: i64) -> String {
        let lib = (!sel.is_empty()).then_some(sel);
        let Some(all) = self.hub.proposals(lib) else {
            return String::new();
        };
        let csrf = self.csrf();
        let id = esc(sel);
        let (open, decided): (Vec<_>, Vec<_>) =
            all.iter().partition(|p| p.status == ProposalStatus::Open);
        let mut out = String::new();
        if open.is_empty() {
            out.push_str("<p class=\"muted\">Keine offenen Vorschläge. Wer unter Band <b>Änderungen vorschlagen</b> (oder mehr) darf, kann aus seiner Kopie der Bibliothek Änderungen schicken. Sie warten hier, bis du sie übernimmst oder ablehnst.</p>");
        }
        for (n, p) in open.iter().enumerate() {
            let Some((p, changes)) = self.hub.proposal(lib, &p.id) else {
                continue;
            };
            let pid = esc(&p.id);
            let waiting: Vec<_> = changes
                .iter()
                .filter(|c| c.change.state == ChangeState::Open)
                .collect();
            let stale = waiting.iter().filter(|c| c.stale).count();
            let who =
                p.by.name
                    .as_deref()
                    .map(|n| format!("von {}", esc(n)))
                    .unwrap_or_else(|| "ohne Namen".into());
            let stale_note = if stale > 0 {
                format!(" · <span class=\"stale\">{} seitdem geändert</span>", stale)
            } else {
                String::new()
            };
            let rows: String = changes
                .iter()
                .map(|c| {
                    let kind = match (&c.before, &c.after) {
                        (None, Some(_)) => "neu",
                        (Some(_), None) => "löschen",
                        (None, None) => "schon weg",
                        _ => "geändert",
                    };
                    if c.change.state == ChangeState::Applied {
                        return format!(
                            "<div class=\"change done\"><b>{}</b> <span class=\"muted\">· {kind} · übernommen</span></div>",
                            esc(&c.label)
                        );
                    }
                    let flag = if c.stale {
                        " <span class=\"stale\" title=\"Seit dem Vorschlag hat sich das in der Bibliothek geändert. Der Vergleich zeigt den jetzigen Stand gegen den Vorschlag. Übernehmen lässt es aus, Trotzdem übernehmen überschreibt den jetzigen Stand.\">seitdem geändert</span>"
                    } else {
                        ""
                    };
                    let diff = diff_lines(
                        c.before.as_deref().unwrap_or(""),
                        c.after.as_deref().unwrap_or(""),
                    );
                    format!(
                        "<div class=\"change\"><label><input type=\"checkbox\" name=\"key\" value=\"{}/{}\" checked> <b>{}</b> <span class=\"muted\">· {kind}</span>{flag}</label>{diff}</div>",
                        esc(&c.change.db),
                        esc(&c.change.key),
                        esc(&c.label)
                    )
                })
                .collect();
            let note = p
                .note
                .as_deref()
                .map(|n| format!("<p class=\"note\">{}</p>", esc(n)))
                .unwrap_or_default();
            let force = if stale > 0 {
                "<button class=\"quiet\" name=\"force\" value=\"1\" onclick=\"return confirm('Auch übernehmen, was sich seitdem geändert hat? Der jetzige Stand dieser Songs wird überschrieben.')\" title=\"Auch was sich seitdem geändert hat: der jetzige Stand wird überschrieben\">Trotzdem übernehmen</button>"
            } else {
                ""
            };
            out.push_str(&format!(
                "<details class=\"prop\"{}><summary><b>{}</b> <span class=\"muted\">{who} · {} · {}</span>{stale_note}</summary>\n{note}\
                 <form method=\"post\" action=\"./\">{csrf}<input type=\"hidden\" name=\"do\" value=\"proposal-accept\"><input type=\"hidden\" name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"pid\" value=\"{pid}\">\
                 {rows}<div class=\"row\"><button>Ausgewählte übernehmen</button>{force}</div></form>\
                 <form method=\"post\" action=\"./\" onsubmit=\"return confirm('Ablehnen? Was davon noch offen ist, kommt nicht in die Bibliothek.')\">{csrf}<input type=\"hidden\" name=\"do\" value=\"proposal-reject\"><input type=\"hidden\" name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"pid\" value=\"{pid}\">\
                 <button class=\"quiet danger\">Ablehnen</button></form></details>\n",
                if n == 0 { " open" } else { "" },
                esc(&p.title),
                ago(p.created_ms(), now),
                plural(waiting.len() as u64, "Änderung offen", "Änderungen offen"),
            ));
        }
        if !decided.is_empty() {
            let rows: String = decided
                .iter()
                .take(5)
                .map(|p| {
                    let status = match p.status {
                        ProposalStatus::Accepted => "übernommen",
                        ProposalStatus::Rejected => "abgelehnt",
                        ProposalStatus::Withdrawn => "zurückgezogen",
                        ProposalStatus::Open => "offen",
                    };
                    format!(
                        "<li><span class=\"what\">{}</span> <span class=\"muted\">{} · {status} {}</span></li>",
                        esc(&p.title),
                        p.by.name.as_deref().map(esc).unwrap_or_default(),
                        ago(p.decided_ms().unwrap_or_else(|| p.created_ms()), now)
                    )
                })
                .collect();
            out.push_str(&format!(
                "<h4>Zuletzt entschieden</h4>\n<ul class=\"rows\">{rows}</ul>"
            ));
        }
        out
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
        // Ein nicht angehakter Kasten schickt nichts: dann spielt er nicht mit.
        let plays = || field("plays") == "1";
        let days = || field("days").trim().parse::<u32>().unwrap_or(30).min(365);
        // Leer: keine eigene Zahl (für einen Musiker: wie für alle; für dich: ohne Grenze).
        let count = |name: &str| {
            field(name)
                .trim()
                .parse::<u32>()
                .ok()
                .map(|n| n.min(AI_MAX))
        };
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
                        plays(),
                        days(),
                        now_ms(),
                    )
                    .map_err(fail)?;
                self.registry
                    .set_member_voice(&id, &m.id, Voice::parse(&field("voice")))
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={}&m=member&to=band", m.id))
            }
            "member-edit" => {
                self.registry
                    .edit_member(
                        &id,
                        &person,
                        &field("name"),
                        &form_fields(body, "role"),
                        Can::parse(&field("can")),
                        plays(),
                        count("ai"),
                    )
                    .map_err(fail)?;
                self.registry
                    .set_member_voice(&id, &person, Voice::parse(&field("voice")))
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={person}&m=saved&to=band"))
            }
            "member-extend" => {
                self.registry
                    .extend_member(&id, &person, days(), now_ms())
                    .map_err(fail)?;
                Ok(format!("?i={id}&p={person}&m=extended&to=band"))
            }
            "member-remove" => {
                self.registry.remove_member(&id, &person).map_err(fail)?;
                Ok(format!("?i={id}&m=unmember&to=band"))
            }
            "ai-key" => {
                self.registry
                    .set_assistant_key(&id, &field("key"))
                    .map_err(fail)?;
                Ok(format!("?i={id}&m=ai-key&to=ki"))
            }
            "ai-limits" => {
                self.registry
                    .set_assistant_limits(&id, count("daily").unwrap_or(2), count("owner"))
                    .map_err(fail)?;
                Ok(format!("?i={id}&m=ai-limits&to=ki"))
            }
            "ai-remove" => {
                self.registry.remove_assistant(&id).map_err(fail)?;
                Ok(format!("?i={id}&m=ai-off&to=ki"))
            }
            "proposal-accept" => {
                let keys = form_fields(body, "key");
                if keys.is_empty() {
                    return Err(fail("Keine Änderung ausgewählt.".into()));
                }
                let done = self
                    .hub
                    .accept_proposal(
                        (!id.is_empty()).then_some(id.as_str()),
                        &field("pid"),
                        Some(&keys),
                        field("force") == "1",
                        None,
                    )
                    .map_err(|e| fail(proposal_error(&e)))?;
                let m = if done.conflicts > 0 {
                    "partly"
                } else {
                    "accepted"
                };
                Ok(format!("?i={id}&m={m}&to=vorschlaege"))
            }
            "proposal-reject" => {
                self.hub
                    .reject_proposal((!id.is_empty()).then_some(id.as_str()), &field("pid"), None)
                    .map_err(|e| fail(proposal_error(&e)))?;
                Ok(format!("?i={id}&m=rejected&to=vorschlaege"))
            }
            "remove" => {
                self.registry.remove(&id).map_err(fail)?;
                Ok("?m=removed".into())
            }
            _ => Err((id, "Unbekannte Aktion.".into())),
        }
    }
}

/// Was der Server beim Vorschlag sagt, für einen Menschen.
fn proposal_error(e: &str) -> String {
    match e {
        "no such proposal" => "Diesen Vorschlag gibt es nicht mehr.".into(),
        "already decided" => "Über diesen Vorschlag ist schon entschieden.".into(),
        other => other.to_string(),
    }
}

/// Mehr Zeilen vergleicht die Seite nicht: das wäre kein Song mehr.
const DIFF_MAX: usize = 3000;
/// Gleiche Zeilen um jede Änderung herum; der Rest wird zusammengefaltet.
const DIFF_CONTEXT: usize = 2;

/// Was bleibt, geht und kommt, Zeile für Zeile (längste gemeinsame
/// Teilfolge), als `<pre>`.
fn diff_lines(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    if a.len() > DIFF_MAX || b.len() > DIFF_MAX {
        return format!(
            "<p class=\"muted small\">Zu lang zum Vergleichen: {} statt {} Zeilen.</p>",
            b.len(),
            a.len()
        );
    }
    // lcs[i][j]: wie lang die gemeinsame Teilfolge von a[i..] und b[j..] ist.
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Bei Gleichstand erst was geht, dann was kommt.
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    if ops.iter().all(|(op, _)| *op == ' ') {
        return "<p class=\"muted small\">Nur Leerzeichen oder Zeilenenden anders.</p>".into();
    }
    let near: Vec<bool> = (0..ops.len())
        .map(|k| {
            let from = k.saturating_sub(DIFF_CONTEXT);
            let to = (k + DIFF_CONTEXT + 1).min(ops.len());
            ops[from..to].iter().any(|(op, _)| *op != ' ')
        })
        .collect();
    let mut out = String::new();
    let mut skipped = 0;
    for (k, (op, line)) in ops.iter().enumerate() {
        if !near[k] {
            skipped += 1;
            continue;
        }
        if skipped > 0 {
            out.push_str(&format!(
                "<span class=\"gap\">⋯ {}</span>",
                plural(skipped, "Zeile gleich", "Zeilen gleich")
            ));
            skipped = 0;
        }
        let class = match op {
            '+' => "add",
            '-' => "del",
            _ => "ctx",
        };
        out.push_str(&format!(
            "<span class=\"{class}\">{op} {}</span>",
            esc(line)
        ));
    }
    if skipped > 0 {
        out.push_str(&format!(
            "<span class=\"gap\">⋯ {}</span>",
            plural(skipped, "Zeile gleich", "Zeilen gleich")
        ));
    }
    format!("<pre class=\"diff\">{out}</pre>")
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
  input.short { width: 5.5em; }
  form { margin: 0; }
  .row { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 10px; }
  .row input { flex: 1 1 200px; min-width: 0; }
  .roles { display: inline-flex; flex-wrap: wrap; gap: 6px; }
  .roles label { display: inline-flex; align-items: center; gap: 4px; min-height: 36px; padding: 0 10px; border: 1px solid var(--line); border-radius: 999px; font-weight: 600; cursor: pointer; }
  .roles input { margin: 0; flex: none; min-height: 0; padding: 0; }
  .plays { display: inline-flex; align-items: center; gap: 6px; min-height: 36px; padding: 0 10px; border: 1px solid var(--line); border-radius: 999px; cursor: pointer; }
  .plays input { margin: 0; flex: none; min-height: 0; padding: 0; }
  .letter { display: inline-grid; place-items: center; width: 22px; height: 22px; border: 1px solid var(--line); border-radius: 6px; font-size: 12px; font-weight: 600; }
  .who { font-weight: 600; }
  .band { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 24px; align-items: start; }
  .band .row > input { flex-basis: 140px; }
  .crew { max-height: min(60vh, 520px); overflow-y: auto; margin: 0 0 12px; position: relative; overscroll-behavior: contain; }
  .crew li { padding-right: 4px; }
  .crew li:has([aria-current]) { background: color-mix(in srgb, var(--accent) 12%, transparent); border-radius: 6px; padding-left: 6px; }
  .find { width: 100%; margin: 0 0 8px; }
  .pick { position: sticky; top: 16px; min-width: 0; }
  .pick section + section { margin-top: 20px; }
  .tools { display: flex; flex-wrap: wrap; gap: 6px; margin: 10px 0 0; }
  .tools button { padding: 6px 12px; font-size: 13px; min-height: 32px; }
  @media (max-width: 640px) { .band { grid-template-columns: 1fr; } .crew { max-height: 45vh; } .pick { position: static; } }
  .who[aria-current] { text-decoration: underline; }
  input, select { font: inherit; padding: 8px 12px; min-height: 36px; border: 1px solid var(--line); border-radius: 8px; background: var(--bg); color: var(--fg); }
  button { font: inherit; padding: 8px 16px; min-height: 36px; border: 1px solid var(--accent); border-radius: 999px; background: var(--accent); color: var(--bg); cursor: pointer; }
  button.quiet { background: transparent; color: var(--fg); border-color: var(--line); }
  button.danger { color: var(--danger); }
  .banner { padding: 10px 14px; border-radius: 12px; border: 1px solid var(--accent); }
  .banner.error { border-color: var(--danger); color: var(--danger); }
  :root { --c1: #2a78d6; --c2: #eb6834; --c3: #1baf7a; --c4: #eda100; --c5: #e87ba4; --c6: #008300; --c7: #6250d6; --c8: #e34948; }
  @media (prefers-color-scheme: dark) { :root { --c1: #3987e5; --c2: #d95926; --c3: #199e70; --c4: #c98500; --c5: #d55181; --c6: #008300; --c7: #9085e9; --c8: #e66767; } }
  h4 { font-size: 14px; margin: 20px 0 8px; color: var(--muted); font-weight: 600; }
  .small { font-size: 12px; }
  .pies { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 12px; margin-top: 12px; }
  figure { margin: 0; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--card); min-width: 0; }
  figcaption { font-weight: 600; margin-bottom: 8px; }
  .pie { display: flex; align-items: center; gap: 16px; }
  .donut { flex: none; width: 120px; height: 120px; }
  .donut circle { fill: none; stroke-width: 6; }
  .donut .track { stroke: var(--line); }
  .donut-n { text-anchor: middle; font-size: 8px; font-weight: 600; fill: var(--fg); }
  .donut-k { text-anchor: middle; font-size: 3.6px; fill: var(--muted); }
  .legend { list-style: none; margin: 0; padding: 0; flex: 1; min-width: 0; font-size: 13px; }
  .legend li { display: flex; align-items: center; gap: 8px; padding: 3px 0; }
  .legend .name { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .swatch { display: inline-block; flex: none; width: 10px; height: 10px; border-radius: 3px; margin-right: 6px; vertical-align: baseline; }
  .legend .swatch { margin: 0; }
  .num { font-variant-numeric: tabular-nums; text-align: right; }
  .bars { position: relative; display: flex; align-items: flex-end; gap: 2px; height: 120px; padding: 16px 0 0; border-bottom: 1px solid var(--line); }
  .bar { flex: 1; height: 100%; display: flex; align-items: flex-end; min-width: 0; }
  .bar > span { width: 100%; background: var(--accent); border-radius: 3px 3px 0 0; }
  .bar:hover > span { opacity: .75; }
  .bars-max { position: absolute; top: 0; left: 0; font-size: 11px; }
  .bars-axis { display: flex; justify-content: space-between; font-size: 12px; margin-top: 4px; }
  .scroll { overflow-x: auto; }
  table.usage { width: 100%; border-collapse: collapse; font-size: 14px; }
  table.usage th { text-align: left; font-weight: 600; color: var(--muted); font-size: 12px; padding: 6px 8px; border-bottom: 1px solid var(--line); }
  table.usage th.num { text-align: right; }
  table.usage td { padding: 6px 8px; border-bottom: 1px solid var(--line); vertical-align: top; white-space: nowrap; }
  table.usage td:first-child { white-space: normal; padding-left: 0; }
  .waiting { color: var(--accent); font-weight: 600; }
  .prop { border: 1px solid var(--line); border-radius: 12px; background: var(--card); padding: 10px 14px; margin: 0 0 10px; }
  .prop summary { cursor: pointer; }
  .prop[open] summary { margin-bottom: 8px; }
  .change { padding: 8px 0; border-top: 1px solid var(--line); }
  .change label { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; cursor: pointer; }
  .change input { margin: 0; min-height: 0; }
  .change.done { opacity: .7; }
  .stale { color: var(--danger); font-size: 13px; }
  pre.diff { margin: 6px 0 0; padding: 8px 10px; max-height: 320px; overflow: auto; border-radius: 8px; background: var(--bg); font: 12px/1.5 ui-monospace, monospace; }
  pre.diff span { display: block; white-space: pre-wrap; overflow-wrap: anywhere; }
  pre.diff .add { background: color-mix(in srgb, var(--c3) 18%, transparent); }
  pre.diff .del { background: color-mix(in srgb, var(--c8) 18%, transparent); }
  pre.diff .gap { color: var(--muted); }
</style>
</head>
<body>
<main>
"#;

/// Zeitpunkte in der Zeitzone des Geräts, das die Seite ansieht; ohne
/// Skript bleibt „vor 5 Min.“ stehen.
const LOCAL_TIME: &str = "<script>for(const t of document.querySelectorAll('time[data-t]')){const d=new Date(+t.dataset.t);t.title=t.textContent;t.dateTime=d.toISOString();t.textContent=t.dataset.only==='time'?d.toLocaleTimeString('de-DE',{hour:'2-digit',minute:'2-digit'}):d.toLocaleString('de-DE',{weekday:'short',day:'numeric',month:'numeric',hour:'2-digit',minute:'2-digit'})}</script>\n";

/// Zum Weitergeben der QR-Codes (`qr_share`): jeder wird ein PNG mit dem
/// Namen darunter, das sich auch per Rechtsklick oder langem Tippen kopieren
/// lässt; dazu die Knöpfe. Und die Suche in der Band, und der gewählte
/// Musiker bleibt in der Liste im Bild.
const SHARE: &str = r#"<script>(()=>{
const flash=(b,t)=>{b.dataset.label??=b.textContent;b.textContent=t;clearTimeout(b.t);b.t=setTimeout(()=>b.textContent=b.dataset.label,1800)};
const draw=(svg,text)=>new Promise(done=>{const n=svg.viewBox.baseVal.width,s=12,cap=text?48:0,c=document.createElement('canvas');c.width=n*s;c.height=n*s+cap;const g=c.getContext('2d');g.fillStyle='#fff';g.fillRect(0,0,c.width,c.height);g.fillStyle='#000';for(const m of svg.querySelector('path').getAttribute('d').matchAll(/M(\d+) (\d+)/g))g.fillRect(m[1]*s,m[2]*s,s,s);if(text){let f=26;do g.font=`600 ${f}px system-ui,sans-serif`;while(g.measureText(text).width>c.width-32&&--f>10);g.textAlign='center';g.fillText(text,c.width/2,n*s+18)}c.toBlob(done,'image/png')});
const copyText=async t=>{try{await navigator.clipboard.writeText(t);return true}catch{const a=document.createElement('textarea');a.value=t;a.style.position='fixed';a.style.opacity='0';document.body.append(a);a.select();const ok=document.execCommand('copy');a.remove();return ok}};
for(const box of document.querySelectorAll('.share')){
  const svg=box.querySelector('svg.qr');if(!svg)continue;
  const {link,caption,file}=box.dataset,png=draw(svg,caption);
  png.then(b=>{if(!b)return;const img=new Image();img.className='qr';img.alt=svg.getAttribute('aria-label');img.src=URL.createObjectURL(b);svg.replaceWith(img)});
  const save=b=>{const a=document.createElement('a');a.download=file;a.href=URL.createObjectURL(b);document.body.append(a);a.click();a.remove();setTimeout(()=>URL.revokeObjectURL(a.href),1e4)};
  const tools=box.querySelector('.tools'),btn=d=>tools.querySelector(`[data-do=${d}]`);
  if(!(window.ClipboardItem&&navigator.clipboard?.write))btn('image').hidden=true;
  png.then(b=>{if(b&&navigator.canShare?.({files:[new File([b],file,{type:'image/png'})]}))btn('share').hidden=false});
  tools.hidden=false;
  tools.addEventListener('click',async e=>{const b=e.target.closest('button');if(!b)return;
    try{switch(b.dataset.do){
      case'image':await navigator.clipboard.write([new ClipboardItem({'image/png':png})]);flash(b,'Kopiert ✓');break;
      case'link':flash(b,await copyText(link)?'Kopiert ✓':'Ging nicht');break;
      case'save':save(await png);break;
      case'share':await navigator.share({files:[new File([await png],file,{type:'image/png'})],title:caption,text:link});break}}
    catch(err){if(err?.name==='AbortError')return;if(b.dataset.do==='image'){save(await png);flash(b,'Gespeichert')}else flash(b,'Ging nicht')}})}
const find=document.querySelector('.find'),crew=document.querySelector('.crew');
if(find&&crew)find.addEventListener('input',()=>{const q=find.value.trim().toLowerCase();for(const li of crew.children)li.hidden=!!q&&!li.textContent.toLowerCase().includes(q)});
const cur=crew?.querySelector('[aria-current]')?.closest('li');if(cur)crew.scrollTop=cur.offsetTop-crew.clientHeight/2;
// Am Handy steht der Gewählte unter der Liste: nach dem Sprung zur Band gleich zu ihm.
const pick=document.querySelector('.pick section');if(pick&&matchMedia('(max-width: 640px)').matches)addEventListener('load',()=>setTimeout(()=>scrollTo(0,pick.getBoundingClientRect().top+scrollY-16)));
})()</script>
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
                &ui.page(&id, "", 0, None, Some(&e), "", 7),
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
        &ui.page(
            &id,
            &param("p").unwrap_or_default(),
            pick,
            message,
            None,
            param("to").as_deref().unwrap_or_default(),
            param("u").and_then(|u| u.parse().ok()).unwrap_or(7),
        ),
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
    fn the_assistant_section() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-panel-ai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let server = Server::start(Options {
            dir: share.join("chordwright"),
            token: "owner".into(),
            port: 0,
            ..Options::default()
        })
        .unwrap();
        let registry = Arc::new(Registry::open(
            &data,
            &share,
            "chordwright",
            0.0,
            3,
            server.hub(),
        ));
        let ui = Ui {
            panel: Panel {
                addresses: Vec::new(),
                scheme: "https".into(),
                port: "4174".into(),
                token: "owner".into(),
                own_ca: false,
                folder: "chordwright".into(),
                samba: false,
                public_url: None,
            },
            registry: registry.clone(),
            hub: server.hub(),
            secret: "s".into(),
        };
        assert!(ui.ai_section("").contains("Schlüssel speichern"));
        let form = |what: &str| format!("csrf=s&id=&do={what}");
        assert!(ui.act(&format!("{}&key=nope", form("ai-key"))).is_err());
        assert_eq!(
            ui.act(&format!(
                "{}&key=sk-ant-api03-panel-test-9876",
                form("ai-key")
            ))
            .unwrap(),
            "?i=&m=ai-key&to=ki"
        );
        ui.act(&format!("{}&daily=3&owner=", form("ai-limits")))
            .unwrap();
        let a = registry.assistant("").unwrap();
        assert_eq!((a.daily, a.owner_daily), (3, None));
        let page = ui.ai_section("");
        assert!(
            page.contains("sk-ant-…9876") && !page.contains("panel-test"),
            "only the end of the key shows"
        );
        assert!(page.contains("Diesen Monat noch keine Fragen"));
        ui.act(&form("ai-remove")).unwrap();
        assert!(registry.assistant("").is_none());
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_proposals_section() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-panel-prop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let server = Server::start(Options {
            dir: share.join("chordwright"),
            token: "owner".into(),
            port: 0,
            ..Options::default()
        })
        .unwrap();
        let port = server.ready().port;
        let registry = Arc::new(Registry::open(
            &data,
            &share,
            "chordwright",
            0.0,
            3,
            server.hub(),
        ));
        let ui = Ui {
            panel: Panel {
                addresses: Vec::new(),
                scheme: "https".into(),
                port: "4174".into(),
                token: "owner".into(),
                own_ca: false,
                folder: "chordwright".into(),
                samba: false,
                public_url: None,
            },
            registry: registry.clone(),
            hub: server.hub(),
            secret: "s".into(),
        };
        let http = |token: &str, method: &str, path: &str, body: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            let body = answer.split_once("\r\n\r\n").map(|(_, b)| b.to_string());
            serde_json::from_str::<serde_json::Value>(&body.unwrap_or_default()).unwrap_or_default()
        };
        let song = |text: &str| serde_json::json!({ "v": 1, "data": { "text": text } }).to_string();
        http(
            "owner",
            "PUT",
            "/api/library/record/doc.a",
            &song("{title: Alpha}\n[C]eins\nzwei\ndrei"),
        );
        http(
            "owner",
            "PUT",
            "/api/library/record/doc.b",
            &song("{title: Beta}\nalt"),
        );
        assert!(ui
            .proposals_section("", now_ms())
            .contains("Keine offenen Vorschläge"));

        let lea = registry
            .add_member("", "Lea", &[], Can::Suggest, true, 0, now_ms())
            .unwrap();
        let revs = http(&lea.token, "GET", "/api/library?revs=1", "")["revs"].clone();
        let made = http(
            &lea.token,
            "POST",
            "/api/proposals",
            &serde_json::json!({
                "title": "Akkorde <neu>",
                "note": "Bitte prüfen",
                "changes": [
                    { "db": "library", "key": "doc.a", "base": revs["doc.a"], "value": song("{title: Alpha}\n[G]eins\nzwei\ndrei") },
                    { "db": "library", "key": "doc.b", "base": revs["doc.b"], "value": song("{title: Beta}\nneu") },
                    { "db": "library", "key": "doc.c", "base": null, "value": song("{title: Gamma}") },
                ]
            })
            .to_string(),
        );
        let pid = made["id"].as_str().unwrap().to_string();
        // Meanwhile Beta changes in the library.
        http(
            "owner",
            "PUT",
            "/api/library/record/doc.b",
            &song("{title: Beta}\nanders"),
        );

        let page = ui.page("", "", 0, None, None, "vorschlaege", 7);
        assert!(page.contains("1 Vorschlag"), "the card counts it");
        if let Ok(out) = std::env::var("CW_PANEL_DUMP_PROPOSALS") {
            std::fs::write(out, &page).unwrap();
        }
        let section = ui.proposals_section("", now_ms());
        assert!(section.contains("Akkorde &lt;neu&gt;") && section.contains("von Lea"));
        assert!(section.contains("Bitte prüfen"));
        assert!(
            section.contains("<span class=\"del\">- [C]eins</span>"),
            "{section}"
        );
        assert!(section.contains("<span class=\"add\">+ [G]eins</span>"));
        assert!(section.contains("<b>Gamma</b> <span class=\"muted\">· neu"));
        assert!(section.contains("1 seitdem geändert") && section.contains("Trotzdem übernehmen"));

        let form = |what: &str, keys: &[&str]| {
            let keys: String = keys
                .iter()
                .map(|k| format!("&key={}", form_encode(k)))
                .collect();
            format!("csrf=s&id=&do={what}&pid={pid}{keys}")
        };
        assert!(
            ui.act(&form("proposal-accept", &[])).is_err(),
            "nothing picked"
        );
        assert_eq!(
            ui.act(&form(
                "proposal-accept",
                &["library/doc.a", "library/doc.b", "library/doc.c"]
            ))
            .unwrap(),
            "?i=&m=partly&to=vorschlaege"
        );
        let a = http("owner", "GET", "/api/library/record/doc.a", "");
        assert!(a["value"].as_str().unwrap().contains("[G]eins"));
        assert_eq!(a["change"]["by"], "Lea");
        let section = ui.proposals_section("", now_ms());
        assert!(section.contains("übernommen") && section.contains("1 Änderung offen"));

        // Beta, which changed meanwhile, is taken anyway.
        assert_eq!(
            ui.act(&format!(
                "{}&force=1",
                form("proposal-accept", &["library/doc.b"])
            ))
            .unwrap(),
            "?i=&m=accepted&to=vorschlaege"
        );
        let b = http("owner", "GET", "/api/library/record/doc.b", "");
        assert!(b["value"].as_str().unwrap().contains("neu"));
        let section = ui.proposals_section("", now_ms());
        assert!(
            section.contains("Keine offenen Vorschläge") && section.contains("Zuletzt entschieden")
        );
        assert!(ui
            .act(&form("proposal-accept", &["library/doc.b"]))
            .is_err());

        // Another one, turned down.
        let made = http(
            &lea.token,
            "POST",
            "/api/proposals",
            &serde_json::json!({ "title": "Weg damit", "changes": [{ "db": "library", "key": "doc.c", "base": null, "value": null }] }).to_string(),
        );
        let other = made["id"].as_str().unwrap();
        ui.act(&format!("csrf=s&id=&do=proposal-reject&pid={other}"))
            .unwrap();
        assert!(http("owner", "GET", "/api/library/record/doc.c", "")["value"].is_string());
        assert!(ui.proposals_section("", now_ms()).contains("abgelehnt"));
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn someone_who_reads_along() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-panel-plays-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let server = Server::start(Options {
            dir: share.join("chordwright"),
            token: "owner".into(),
            port: 0,
            ..Options::default()
        })
        .unwrap();
        let registry = Arc::new(Registry::open(
            &data,
            &share,
            "chordwright",
            0.0,
            3,
            server.hub(),
        ));
        let ui = Ui {
            panel: Panel {
                addresses: Vec::new(),
                scheme: "https".into(),
                port: "4174".into(),
                token: "owner".into(),
                own_ca: false,
                folder: "chordwright".into(),
                samba: false,
                public_url: None,
            },
            registry: registry.clone(),
            hub: server.hub(),
            secret: "s".into(),
        };
        // The box is ticked when adding someone; unticked, they only read along.
        let page = ui.page("", "", 0, None, None, "band", 7);
        assert!(page.contains("name=\"plays\" value=\"1\" checked"));
        ui.act("csrf=s&id=&do=member-add&name=Ute&can=suggest&days=0")
            .unwrap();
        ui.act("csrf=s&id=&do=member-add&name=Niko&can=read&plays=1&days=0")
            .unwrap();
        let members = registry.members("");
        let ute = members.iter().find(|m| m.name == "Ute").unwrap();
        assert!(!ute.plays && ute.can == Can::Suggest);
        assert!(members.iter().find(|m| m.name == "Niko").unwrap().plays);
        assert_eq!(
            server.hub().members(None).unwrap().len(),
            2,
            "the server knows at once"
        );
        let page = ui.page("", &ute.id, 0, None, None, "band", 7);
        assert!(page.contains("nicht in der Planung"));
        assert!(
            page.contains("name=\"plays\" value=\"1\">spielt mit"),
            "unticked when editing her"
        );
        // Edited back to playing.
        ui.act(&format!(
            "csrf=s&id=&do=member-edit&p={}&name=Ute&can=suggest&plays=1&voice=high",
            ute.id
        ))
        .unwrap();
        assert!(registry.members("").iter().all(|m| m.plays));
        // A singer's voice: given when editing, kept as the app reads it.
        let members = registry.members("");
        assert_eq!(
            members.iter().find(|m| m.name == "Ute").unwrap().voice,
            Some(Voice::High)
        );
        assert_eq!(
            members.iter().find(|m| m.name == "Niko").unwrap().voice,
            None
        );
        let page = ui.page("", &ute.id, 0, None, None, "band", 7);
        assert!(page.contains("<option value=\"high\" selected>Frauenstimme"));
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diffs() {
        let d = diff_lines("a\nb\nc\nd\ne\nf\ng\nh", "a\nb\nc\nd\nX\nf\ng\nh");
        assert!(d.contains("⋯ 2 Zeilen gleich"), "{d}");
        assert!(d.contains("<span class=\"del\">- e</span><span class=\"add\">+ X</span>"));
        assert!(diff_lines("", "neu").contains("+ neu"));
        assert!(diff_lines("a\n", "a").contains("Nur Leerzeichen"));
        assert!(diff_lines("<b>", "x").contains("&lt;b&gt;"));
    }

    #[test]
    fn the_band_side_by_side() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-panel-band-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let server = Server::start(Options {
            dir: share.join("chordwright"),
            token: "owner".into(),
            port: 0,
            ..Options::default()
        })
        .unwrap();
        let registry = Arc::new(Registry::open(
            &data,
            &share,
            "chordwright",
            0.0,
            3,
            server.hub(),
        ));
        let ui = Ui {
            panel: Panel {
                addresses: vec!["192.168.1.5".into()],
                scheme: "https".into(),
                port: "4174".into(),
                token: "owner".into(),
                own_ca: false,
                folder: "chordwright".into(),
                samba: false,
                public_url: None,
            },
            registry: registry.clone(),
            hub: server.hub(),
            secret: "s".into(),
        };
        let mut picked = String::new();
        for i in 0..12 {
            let m = registry
                .add_member(
                    "",
                    &format!("Musiker/{i}"),
                    &["P".into()],
                    Can::Read,
                    true,
                    0,
                    now_ms(),
                )
                .unwrap();
            if i == 7 {
                picked = m.id;
            }
        }
        let page = ui.page("", "", 0, None, None, "band", 30);
        assert!(page.contains("class=\"band\"") && page.contains("class=\"find\""));
        assert!(page.contains("Einen Namen wählen"));
        assert!(
            !page.contains("data-do=\"image\""),
            "the library's own code is not for passing on"
        );
        let page = ui.page("", &picked, 0, None, None, "band", 30);
        assert!(
            page.contains("data-do=\"image\"")
                && page.contains("data-file=\"Chordwright - Musiker_7.png\"")
        );
        assert!(page.contains("data-caption=\"Chordwright für Musiker/7\""));
        assert!(
            !page.contains("1 Zertifikat"),
            "without an own CA one code is enough"
        );
        let ui = Ui {
            panel: Panel {
                own_ca: true,
                ..ui.panel
            },
            ..ui
        };
        let page = ui.page("", &picked, 0, None, None, "band", 30);
        if let Ok(out) = std::env::var("CW_PANEL_DUMP") {
            std::fs::write(out, &page).unwrap();
        }
        assert!(page.contains("data-link=\"https://192.168.1.5:4174/ca.crt\""));
        assert!(page.contains("data-file=\"Chordwright - 2 Musiker_7.png\""));
        assert_eq!(file_name("Sönke (Aushilfe)"), "Soenke (Aushilfe)");
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_usage_section() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-panel-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let server = Server::start(Options {
            dir: share.join("chordwright"),
            token: "owner".into(),
            port: 0,
            ..Options::default()
        })
        .unwrap();
        let port = server.ready().port;
        let ask = |token: &str, method: &str, path: &str, extra: &str, body: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-client: app1\r\n{extra}connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        let registry = Arc::new(Registry::open(
            &data,
            &share,
            "chordwright",
            0.0,
            3,
            server.hub(),
        ));
        let ui = Ui {
            panel: Panel {
                addresses: Vec::new(),
                scheme: "https".into(),
                port: "4174".into(),
                token: "owner".into(),
                own_ca: false,
                folder: "chordwright".into(),
                samba: false,
                public_url: None,
            },
            registry,
            hub: server.hub(),
            secret: "s".into(),
        };
        assert!(ui.usage_section("", 7, now_ms()).contains("hat niemand"));

        // A read before the app gave its name, a write with it, a read after.
        ask("owner", "GET", "/api/library", "", "");
        ask(
            "owner",
            "PUT",
            "/api/user/record/x",
            "x-chordwright-user: J%C3%BCrgen\r\n",
            "\"1\"",
        );
        ask("owner", "GET", "/api/library", "", "");
        ask("wrong", "GET", "/api/library", "", "");
        let page = ui.usage_section("", 7, now_ms());
        assert!(page.contains("<b>Jürgen</b>"), "{page}");
        assert!(page.contains("Ohne Namen") && page.contains("Abgewiesen"));
        assert!(page.contains("Schreiben") && page.contains("class=\"donut\""));
        assert!(page.contains("aria-current=\"true\">7 Tage"));
        let report = server.hub().usage(None, 1).unwrap();
        assert_eq!(report.total.requests, 4);
        assert_eq!(report.people[0].name, "Jürgen");
        assert_eq!(report.people[0].tally.requests, 2);
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

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

    #[test]
    fn members_by_rights_then_name() {
        let m = |name: &str, can: Can| Member {
            id: name.to_lowercase(),
            name: name.into(),
            token: String::new(),
            roles: vec![],
            until: None,
            can,
            ai: None,
            voice: None,
            plays: true,
        };
        let mut list = vec![
            m("zoe", Can::Read),
            m("Anna", Can::Read),
            m("Bert", Can::Sets),
            m("Xaver", Can::All),
            m("carla", Can::All),
            m("Alex", Can::Sets),
        ];
        order_members(&mut list);
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["carla", "Xaver", "Alex", "Bert", "Anna", "zoe"]);
    }
}
