//! Small things the modules share: the clock as JavaScript writes it, the
//! JSON layout the files have always had, revisions, atomic writes.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Milliseconds since the epoch, like `Date.now()`.
pub fn now_ms() -> i64 {
    ms_of(SystemTime::now())
}

pub fn ms_of(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// `new Date(ms).toISOString()`: `2026-09-28T10:15:30.123Z`. Every timestamp
/// in the files has this form, and the journal compares them as strings.
pub fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

/// `Date.parse` for the form `iso` writes (fraction optional). None where
/// JavaScript would give NaN — for anything else, too, which is fine for
/// timestamps this server wrote itself.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };
    let (y, mo, d, h, mi, sec) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let rest = &s[19..];
    let (frac, zone) = match rest.strip_prefix('.') {
        Some(r) => {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            let ms = format!("{:0<3}", &r[..digits.min(3)]).parse::<i64>().ok()?;
            (ms, &r[digits..])
        }
        None => (0, rest),
    };
    if zone != "Z"
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || h > 23
        || mi > 59
        || sec > 59
    {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    Some(days * 86_400_000 + h * 3_600_000 + mi * 60_000 + sec * 1000 + frac)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `JSON.stringify(value, null, 2) + '\n'` — how every JSON file here looks.
pub fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into());
    text.push('\n');
    text
}

/// A number as JavaScript prints it: `24`, not `24.0`.
pub fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}

pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A record's revision: a hash of the bytes on disk. Not a timestamp —
/// clocks differ between devices, and a hash changes by itself when someone
/// edits the file over Samba or in vim, with nobody having to remember to bump
/// it.
pub fn revision_of(bytes: &[u8]) -> String {
    hex(&sha256(bytes))[..16].to_string()
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through a temporary file and rename it into place. A reader — the
/// app, the watcher, the next write's read-modify-write — sees the old file or
/// the new one, never half of one. The temp name starts with a dot and ends in
/// `.tmp`, so nothing mistakes it for a record.
pub fn write_atomic(path: &Path, body: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, body).and_then(|_| rename_over(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Windows refuses to rename onto a file someone else has open at that moment
/// — a reader of the same file, the virus scanner, the search indexer. It lets
/// go within milliseconds, so try again for a while instead of failing the
/// write. Elsewhere a rename onto an open file just works.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let mut waited = 0u64;
    let mut wait = 5u64;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if cfg!(windows) && busy(&e) && waited <= 5_000 => {
                std::thread::sleep(std::time::Duration::from_millis(wait));
                waited += wait;
                wait = (wait * 2).min(100);
            }
            Err(e) => return Err(e),
        }
    }
}

/// EPERM, EACCES, EBUSY in Node's words: access denied, a sharing or a lock
/// violation.
fn busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// `decodeURIComponent`, which throws on a broken escape or bytes that are not
/// UTF-8.
pub fn decode_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hexed = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hexed, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A query string as `URLSearchParams` reads it: `+` is a space, broken
/// escapes stay as they are.
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    let decode = |s: &str| {
        let s = s.replace('+', " ");
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                if let Some(b) = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect()
}

/// `String.prototype.trim`: Unicode white space and the byte order mark.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trip() {
        for ms in [
            0,
            1_759_000_000_123,
            951_782_400_000, /* 2000-02-29 */
            4_102_444_799_999,
        ] {
            assert_eq!(parse_iso(&iso(ms)), Some(ms), "{}", iso(ms));
        }
        assert_eq!(iso(1_759_224_930_123), "2025-09-30T09:35:30.123Z");
        assert_eq!(parse_iso("2026-09-30T10:00:00Z"), Some(1_790_762_400_000));
        assert_eq!(parse_iso("gestern"), None);
    }

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(number(24.0).to_string(), "24");
        assert_eq!(number(0.5).to_string(), "0.5");
    }

    #[test]
    fn components() {
        assert_eq!(decode_component("J%C3%BCrgen").as_deref(), Some("Jürgen"));
        assert_eq!(decode_component("%E0%A4%A"), None);
        assert_eq!(
            parse_query("a=1+2&b&c=%zz"),
            vec![
                ("a".into(), "1 2".into()),
                ("b".into(), String::new()),
                ("c".into(), "%zz".into()),
            ]
        );
    }
}
