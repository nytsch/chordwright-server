//! Eine eigene kleine Zertifizierungsstelle — für das Home-Assistant-Add-on
//! und für eine Bibliothek, die die Desktop-App im WLAN freigibt.
//!
//! Warum nicht einfach ein selbstsigniertes Zertifikat: Das muss man in jedem
//! Browser per Klick bestätigen, und eine App vom Home-Bildschirm hat keinen
//! Ort für diesen Klick. Eine CA installiert man einmal pro Gerät als
//! vertrauenswürdig, und danach ist jedes Zertifikat, das sie ausstellt,
//! überall gültig.
//!
//! - `create_ca`: die Stelle selbst. Zehn Jahre, darf nur signieren.
//! - `issue_server_cert`: das Zertifikat, mit dem der Server spricht. 825 Tage,
//!   SAN und serverAuth — darunter lehnen iOS und macOS ab.
//!
//! Die Dateien im Ordner sind dieselben wie beim Node-Server bis 1.7.0: Eine
//! CA, die der angelegt hat, bleibt gültig, und die Geräte, die sie
//! installiert haben, vertrauen weiter.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use serde_json::json;

use crate::util::{iso, now_ms};

pub const CA_VALIDITY_DAYS: i64 = 3650;
pub const SERVER_VALIDITY_DAYS: i64 = 825;
const DAY: i64 = 24 * 60 * 60 * 1000;
const CA_NAME: &str = "Chordwright lokale CA";

// --- DER, so viel davon, wie ein Zertifikat braucht --------------------------

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = body.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(body);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn set(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x31, &parts.concat())
}

fn explicit(n: u8, parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0xa0 | n, &parts.concat())
}

fn octets(buf: &[u8]) -> Vec<u8> {
    tlv(0x04, buf)
}

fn boolean(v: bool) -> Vec<u8> {
    tlv(0x01, &[if v { 0xff } else { 0 }])
}

fn bit_string(buf: &[u8]) -> Vec<u8> {
    tlv(0x03, &[&[0u8][..], buf].concat())
}

fn oid(dotted: &str) -> Vec<u8> {
    let n: Vec<u64> = dotted.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let mut bytes = vec![(40 * n[0] + n[1]) as u8];
    for &v in &n[2..] {
        let mut chunk = vec![(v & 0x7f) as u8];
        let mut rest = v >> 7;
        while rest > 0 {
            chunk.insert(0, (rest & 0x7f) as u8 | 0x80);
            rest >>= 7;
        }
        bytes.extend(chunk);
    }
    tlv(0x06, &bytes)
}

/// DER integers are signed; a leading 1-bit would make it negative.
fn integer(buf: &[u8]) -> Vec<u8> {
    if buf[0] & 0x80 != 0 {
        tlv(0x02, &[&[0u8][..], buf].concat())
    } else {
        tlv(0x02, buf)
    }
}

fn utc_time(ms: i64) -> Vec<u8> {
    let s = iso(ms); // 2026-09-23T17:00:00.000Z
    let t = format!(
        "{}{}{}{}{}{}Z",
        &s[2..4],
        &s[5..7],
        &s[8..10],
        &s[11..13],
        &s[14..16],
        &s[17..19]
    );
    tlv(0x17, t.as_bytes())
}

fn name(cn: &str) -> Vec<u8> {
    seq(&[set(&[seq(&[oid("2.5.4.3"), tlv(0x0c, cn.as_bytes())])])])
}

fn extension(id: &str, critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(boolean(true));
    }
    parts.push(octets(&value));
    seq(&parts)
}

fn ecdsa_with_sha256() -> Vec<u8> {
    seq(&[oid("1.2.840.10045.4.3.2")])
}

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = BASE64.encode(der);
    let lines: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// The DER inside the first `label` block of a PEM text.
pub fn pem_block(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let start = text.find(&begin)? + begin.len();
    let end = text[start..].find("-----END")? + start;
    let b64: String = text[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    BASE64.decode(b64).ok()
}

// --- Lesen: genau zwei Felder eines Zertifikats ------------------------------

/// One TLV at `pos`: (tag, start of content, end of content).
fn read_tlv(buf: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let tag = *buf.get(pos)?;
    let first = *buf.get(pos + 1)? as usize;
    let (len, head) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let len = buf
            .get(pos + 2..pos + 2 + n)?
            .iter()
            .fold(0usize, |acc, b| acc << 8 | *b as usize);
        (len, 2 + n)
    };
    let start = pos + head;
    let end = start.checked_add(len)?;
    (end <= buf.len()).then_some((tag, start, end))
}

struct CertInfo {
    /// The subject Name, as DER, byte for byte.
    subject: Vec<u8>,
    not_after: i64,
}

fn parse_time(tag: u8, bytes: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(bytes).ok()?.strip_suffix('Z')?;
    let full = match tag {
        0x17 if s.len() == 12 => {
            let yy: i64 = s[..2].parse().ok()?;
            format!("{}{}", if yy < 50 { 2000 + yy } else { 1900 + yy }, &s[2..])
        }
        0x18 if s.len() == 14 => s.to_string(),
        _ => return None,
    };
    crate::util::parse_iso(&format!(
        "{}-{}-{}T{}:{}:{}Z",
        &full[..4],
        &full[4..6],
        &full[6..8],
        &full[8..10],
        &full[10..12],
        &full[12..14]
    ))
}

fn cert_info(der: &[u8]) -> Option<CertInfo> {
    let (_, cert, _) = read_tlv(der, 0)?;
    let (_, tbs, tbs_end) = read_tlv(der, cert)?;
    let mut fields = Vec::new();
    let mut pos = tbs;
    while pos < tbs_end && fields.len() < 6 {
        let (tag, start, end) = read_tlv(der, pos)?;
        fields.push((tag, pos, start, end));
        pos = end;
    }
    // [0] version is optional; after it: serial, signature, issuer, validity, subject.
    let skip = usize::from(fields.first()?.0 == 0xa0);
    let &(_, _, validity, validity_end) = fields.get(skip + 3)?;
    let &(_, subject_at, _, subject_end) = fields.get(skip + 4)?;
    let (_, _, not_before_end) = read_tlv(der, validity)?;
    let (tag, start, end) = read_tlv(der, not_before_end)?;
    if end > validity_end {
        return None;
    }
    Some(CertInfo {
        subject: der[subject_at..subject_end].to_vec(),
        not_after: parse_time(tag, &der[start..end])?,
    })
}

/// The dNSName entries of the subjectAltName extension.
fn san_dns_names(der: &[u8]) -> Option<Vec<String>> {
    let (_, cert, _) = read_tlv(der, 0)?;
    let (_, tbs, tbs_end) = read_tlv(der, cert)?;
    let mut pos = tbs;
    while pos < tbs_end {
        let (tag, start, end) = read_tlv(der, pos)?;
        pos = end;
        if tag != 0xa3 {
            continue;
        }
        let (_, extensions, extensions_end) = read_tlv(der, start)?;
        let mut at = extensions;
        while at < extensions_end {
            let (_, ext, ext_end) = read_tlv(der, at)?;
            at = ext_end;
            let (_, id, id_end) = read_tlv(der, ext)?;
            if der[id..id_end] != [0x55, 0x1d, 0x11] {
                continue;
            }
            // critical (optional), then the value as an OCTET STRING
            let (mut tag, mut value, mut value_end) = read_tlv(der, id_end)?;
            if tag == 0x01 {
                (tag, value, value_end) = read_tlv(der, value_end)?;
            }
            if tag != 0x04 || value >= value_end {
                return None;
            }
            let (_, names, names_end) = read_tlv(der, value)?;
            let mut out = Vec::new();
            let mut n = names;
            while n < names_end {
                let (kind, s, e) = read_tlv(der, n)?;
                if kind == 0x82 {
                    out.push(String::from_utf8_lossy(&der[s..e]).into_owned());
                }
                n = e;
            }
            return Some(out);
        }
    }
    Some(Vec::new())
}

/// Die Namen in einem Zertifikat (Let's Encrypt, DuckDNS) — ohne Platzhalter
/// wie `*.example.org`. Nichts, wenn es nicht lesbar ist.
pub fn certificate_names(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| pem_block(&t, "CERTIFICATE"))
        .and_then(|der| san_dns_names(&der))
        .unwrap_or_default()
        .into_iter()
        .filter(|n| !n.contains('*'))
        .collect()
}

fn read_cert(path: &Path) -> Option<CertInfo> {
    cert_info(&pem_block(
        &std::fs::read_to_string(path).ok()?,
        "CERTIFICATE",
    )?)
}

// --- Schlüssel und Zertifikate -----------------------------------------------

/// SubjectPublicKeyInfo of a P-256 key: id-ecPublicKey, prime256v1, the point.
fn spki(key: &EcdsaKeyPair) -> Vec<u8> {
    seq(&[
        seq(&[oid("1.2.840.10045.2.1"), oid("1.2.840.10045.3.1.7")]),
        bit_string(key.public_key().as_ref()),
    ])
}

/// Key identifier: SHA-1 over the SubjectPublicKeyInfo, as it always was.
fn key_id(key: &EcdsaKeyPair) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &spki(key))
        .as_ref()
        .to_vec()
}

fn generate_key(rng: &SystemRandom) -> Result<(EcdsaKeyPair, Vec<u8>), String> {
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, rng)
        .map_err(|e| e.to_string())?;
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), rng)
        .map_err(|e| e.to_string())?;
    Ok((key, pkcs8.as_ref().to_vec()))
}

fn load_key(path: &Path, rng: &SystemRandom) -> Result<EcdsaKeyPair, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let der = pem_block(&text, "PRIVATE KEY")
        .ok_or_else(|| format!("{}: no PKCS#8 key", path.display()))?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &der, rng)
        .map_err(|e| format!("{}: {e}", path.display()))
}

struct Unsigned<'a> {
    subject: Vec<u8>,
    issuer: Vec<u8>,
    public: &'a EcdsaKeyPair,
    signer: &'a EcdsaKeyPair,
    not_before: i64,
    not_after: i64,
    extensions: Vec<Vec<u8>>,
}

fn certificate(c: Unsigned, rng: &SystemRandom) -> Result<String, String> {
    let mut serial = [0u8; 16];
    rng.fill(&mut serial).map_err(|e| e.to_string())?;
    // Positiv (oberstes Bit aus) und ohne führende Null: DER verlangt die
    // kürzeste Form, und OpenSSL 3 lehnt Auffüllung als „illegal padding" ab.
    serial[0] &= 0x7f;
    if serial[0] == 0 {
        serial[0] = 1;
    }
    let tbs = seq(&[
        explicit(0, &[integer(&[2])]), // v3
        integer(&serial),
        ecdsa_with_sha256(),
        c.issuer,
        seq(&[utc_time(c.not_before), utc_time(c.not_after)]),
        c.subject,
        spki(c.public),
        explicit(3, &[seq(&c.extensions)]),
    ]);
    let signature = c.signer.sign(rng, &tbs).map_err(|e| e.to_string())?;
    Ok(pem(
        "CERTIFICATE",
        &seq(&[tbs, ecdsa_with_sha256(), bit_string(signature.as_ref())]),
    ))
}

/// Die Stelle selbst: (Zertifikat, Schlüssel), beides als PEM.
fn create_ca(rng: &SystemRandom) -> Result<(String, String), String> {
    let (key, pkcs8) = generate_key(rng)?;
    let now = now_ms();
    let cert = certificate(
        Unsigned {
            subject: name(CA_NAME),
            issuer: name(CA_NAME),
            public: &key,
            signer: &key,
            not_before: now - DAY,
            not_after: now + CA_VALIDITY_DAYS * DAY,
            extensions: vec![
                extension("2.5.29.19", true, seq(&[boolean(true), integer(&[0])])), // CA, pathLen 0
                extension("2.5.29.15", true, tlv(0x03, &[0x01, 0x06])), // keyCertSign, cRLSign
                extension("2.5.29.14", false, octets(&key_id(&key))),
            ],
        },
        rng,
    )?;
    Ok((cert, pem("PRIVATE KEY", &pkcs8)))
}

/// Das Zertifikat, mit dem der Server spricht, ausgestellt von der CA.
fn issue_server_cert(
    ca_key: &EcdsaKeyPair,
    ca_subject: Vec<u8>,
    dns_names: &[String],
    ips: &[IpAddr],
    rng: &SystemRandom,
) -> Result<(String, String), String> {
    let (key, pkcs8) = generate_key(rng)?;
    let now = now_ms();
    let mut alt_names: Vec<Vec<u8>> = dns_names.iter().map(|d| tlv(0x82, d.as_bytes())).collect();
    alt_names.extend(ips.iter().map(|ip| match ip {
        IpAddr::V4(v4) => tlv(0x87, &v4.octets()),
        IpAddr::V6(v6) => tlv(0x87, &v6.octets()),
    }));
    let cert = certificate(
        Unsigned {
            subject: name("chordwright"),
            // Der Aussteller muss Byte für Byte dem Namen der CA entsprechen.
            issuer: ca_subject,
            public: &key,
            signer: ca_key,
            // Uhren gehen nicht überall gleich.
            not_before: now - 60 * 60 * 1000,
            not_after: now + SERVER_VALIDITY_DAYS * DAY,
            extensions: vec![
                extension("2.5.29.19", true, seq(&[])), // kein CA
                extension("2.5.29.15", true, tlv(0x03, &[0x07, 0x80])), // digitalSignature
                extension("2.5.29.37", false, seq(&[oid("1.3.6.1.5.5.7.3.1")])), // serverAuth
                extension("2.5.29.17", false, seq(&alt_names)),
                extension("2.5.29.14", false, octets(&key_id(&key))),
                extension("2.5.29.35", false, seq(&[tlv(0x80, &key_id(ca_key))])), // authorityKeyIdentifier
            ],
        },
        rng,
    )?;
    Ok((cert, pem("PRIVATE KEY", &pkcs8)))
}

/// Gültig noch länger als `days` Tage? Nicht lesbar zählt als nein.
fn valid_for(path: &Path, days: i64) -> bool {
    read_cert(path).is_some_and(|c| c.not_after - now_ms() > days * DAY)
}

fn is_ip(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    (parts.len() == 4
        && parts
            .iter()
            .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())))
        || s.contains(':')
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}

pub struct Own {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
    /// What the certificate names: host names, then IP addresses.
    pub names: Vec<String>,
    pub ips: Vec<String>,
}

/// Die eigene CA in `dir` und ein Zertifikat von ihr für `addresses` (Namen
/// und IPs gemischt) — angelegt, wo es fehlt, erneuert, wo es bald abläuft
/// oder nicht mehr zu den Adressen passt.
///
/// Die CA bleibt dieselbe, solange sie gilt: Sie ist es, die auf den Geräten
/// installiert ist. Nur eine, die in 60 Tagen abläuft, wird ersetzt. Das
/// Server-Zertifikat dagegen wird neu ausgestellt, sobald sich eine Adresse
/// ändert (neue IP vom Router) — das merkt kein Gerät.
pub fn ensure_own_certificate(dir: &Path, addresses: &[String]) -> Result<Own, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(fail)?;
    let mut all: Vec<String> = Vec::new();
    for a in addresses.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        if !all.iter().any(|b| b == a) {
            all.push(a.to_string());
        }
    }
    let names: Vec<String> = all.iter().filter(|a| !is_ip(a)).cloned().collect();
    let ips: Vec<String> = all.iter().filter(|a| is_ip(a)).cloned().collect();
    let rng = SystemRandom::new();

    let ca_cert = dir.join("ca-cert.pem");
    let ca_key = dir.join("ca-key.pem");
    let mut ca_renewed = false;
    if !ca_key.exists() || !valid_for(&ca_cert, 60) {
        let (cert, key) = create_ca(&rng)?;
        write_private(&ca_key, &key).map_err(fail)?;
        std::fs::write(&ca_cert, cert).map_err(fail)?;
        ca_renewed = true;
    }

    let own = Own {
        cert: dir.join("server-cert.pem"),
        key: dir.join("server-key.pem"),
        ca: ca_cert.clone(),
        names: names.clone(),
        ips: ips.clone(),
    };
    let meta = dir.join("server-cert.json");
    let wanted = json!({ "names": names, "ips": ips }).to_string();
    let current = std::fs::read_to_string(&meta)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| v.to_string());
    if ca_renewed
        || !own.key.exists()
        || !valid_for(&own.cert, 30)
        || current.as_deref() != Some(wanted.as_str())
    {
        let signer = load_key(&ca_key, &rng)?;
        let subject = read_cert(&ca_cert)
            .map(|c| c.subject)
            .unwrap_or_else(|| name(CA_NAME));
        let parsed: Vec<IpAddr> = ips.iter().filter_map(|ip| ip.parse().ok()).collect();
        let (cert, key) = issue_server_cert(&signer, subject, &names, &parsed, &rng)?;
        write_private(&own.key, &key).map_err(fail)?;
        std::fs::write(&own.cert, cert).map_err(fail)?;
        std::fs::write(&meta, format!("{wanted}\n")).map_err(fail)?;
    }
    Ok(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A random serial with a leading 0x00 byte is not minimal DER; OpenSSL 3
    /// rejected about one certificate in two hundred of the Node server's for
    /// it. A thousand serials would almost surely have hit one.
    #[test]
    fn serials_are_minimal_positive_integers() {
        let rng = SystemRandom::new();
        let (key, _) = generate_key(&rng).unwrap();
        for _ in 0..1000 {
            let pem = certificate(
                Unsigned {
                    subject: name("x"),
                    issuer: name("x"),
                    public: &key,
                    signer: &key,
                    not_before: 0,
                    not_after: DAY,
                    extensions: vec![],
                },
                &rng,
            )
            .unwrap();
            let der = pem_block(&pem, "CERTIFICATE").unwrap();
            let (_, tbs, _) = read_tlv(&der, read_tlv(&der, 0).unwrap().1).unwrap();
            let (_, _, version_end) = read_tlv(&der, tbs).unwrap();
            let (tag, start, end) = read_tlv(&der, version_end).unwrap();
            let serial = &der[start..end];
            assert_eq!(tag, 0x02);
            assert!(serial[0] & 0x80 == 0, "positive");
            assert!(
                !(serial[0] == 0 && serial.len() > 1 && serial[1] & 0x80 == 0),
                "minimal"
            );
            assert!(cert_info(&der).is_some());
        }
    }

    #[test]
    fn a_ca_and_a_certificate_it_issues() {
        let dir =
            std::env::temp_dir().join(format!("cw-certs-{}-{}", std::process::id(), now_ms()));
        let own = ensure_own_certificate(
            &dir,
            &["localhost".into(), "127.0.0.1".into(), "::1".into()],
        )
        .unwrap();
        let ca = read_cert(&own.ca).unwrap();
        let server = read_cert(&own.cert).unwrap();
        assert_eq!(ca.subject, name(CA_NAME));
        assert_eq!(server.subject, name("chordwright"));
        assert_eq!(certificate_names(&own.cert), ["localhost"]);
        assert!(server.not_after - now_ms() > 800 * DAY);
        let before = std::fs::read(&own.cert).unwrap();
        ensure_own_certificate(
            &dir,
            &["localhost".into(), "127.0.0.1".into(), "::1".into()],
        )
        .unwrap();
        assert_eq!(
            before,
            std::fs::read(&own.cert).unwrap(),
            "same addresses, same certificate"
        );
        ensure_own_certificate(&dir, &["localhost".into()]).unwrap();
        assert_ne!(
            before,
            std::fs::read(&own.cert).unwrap(),
            "another address, a new one"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
