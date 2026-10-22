//! A browser that is let in with a short code instead of the token.
//!
//! The program that started the server shows the code (the desktop app next
//! to the address); a browser on another computer types it in and gets a
//! token of its own back (`POST /api/pair`). That token is not the server's
//! token but a guest token made from it and the code — so the server's token
//! never leaves the program, and a guest token keeps working when the server
//! restarts (another wifi, a new certificate) as long as it is started with
//! the same code. Pairing off, another code or another server token: every
//! guest is out.
//!
//! Six digits are few, so wrong codes cost time: after a handful the server
//! stops listening to codes for a while.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

const GUEST_PREFIX: &str = "g1";
const TRIES: u32 = 5;
const LOCKED_FOR: Duration = Duration::from_secs(30);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() & 1 == 1 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn key(token: &str, code: &str) -> hmac::Key {
    hmac::Key::new(hmac::HMAC_SHA256, format!("{token}\n{code}").as_bytes())
}

/// Six random digits, for the program that starts the server.
pub fn new_code() -> String {
    let mut bytes = [0u8; 4];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("system randomness");
    format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000)
}

/// A token for one browser, made from the server's and the code.
fn guest_token(token: &str, code: &str) -> String {
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .expect("system randomness");
    let nonce = hex(&nonce);
    let tag = hmac::sign(&key(token, code), format!("guest:{nonce}").as_bytes());
    format!("{GUEST_PREFIX}.{nonce}.{}", hex(tag.as_ref()))
}

fn is_guest_token(token: &str, code: &str, candidate: &str) -> bool {
    let mut parts = candidate.split('.');
    let (Some(GUEST_PREFIX), Some(nonce), Some(tag), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some(tag) = unhex(tag) else {
        return false;
    };
    !token.is_empty()
        && !code.is_empty()
        && hmac::verify(&key(token, code), format!("guest:{nonce}").as_bytes(), &tag).is_ok()
}

pub enum Attempt {
    Paired(String),
    Wrong,
    /// Too many wrong codes; try again after this long.
    Locked(Duration),
}

pub struct Pairing {
    code: String,
    /// Wrong codes in a row, and since when the door is shut.
    wrong: Mutex<(u32, Option<Instant>)>,
}

impl Pairing {
    pub fn new(code: String) -> Pairing {
        Pairing {
            code,
            wrong: Mutex::new((0, None)),
        }
    }

    /// Whether `candidate` is a guest token this pairing handed out.
    pub fn admits(&self, token: &str, candidate: &str) -> bool {
        is_guest_token(token, &self.code, candidate)
    }

    pub fn attempt(&self, token: &str, code: &str) -> Attempt {
        let mut wrong = self.wrong.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(since) = wrong.1 {
            let waited = since.elapsed();
            if waited < LOCKED_FOR {
                return Attempt::Locked(LOCKED_FOR - waited);
            }
            *wrong = (0, None);
        }
        let given: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if !self.code.is_empty() && given == self.code {
            *wrong = (0, None);
            return Attempt::Paired(guest_token(token, &self.code));
        }
        wrong.0 += 1;
        if wrong.0 >= TRIES {
            wrong.1 = Some(Instant::now());
        }
        Attempt::Wrong
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_token_is_made_from_the_server_token() {
        let guest = guest_token("secret", "123456");
        assert!(is_guest_token("secret", "123456", &guest));
        assert!(!is_guest_token("other", "123456", &guest));
        assert!(!is_guest_token("secret", "654321", &guest));
        assert!(!is_guest_token("", "123456", &guest));
        assert!(!is_guest_token("secret", "123456", "secret"));
        let last = if guest.ends_with('0') { '1' } else { '0' };
        let forged = format!("{}{last}", &guest[..guest.len() - 1]);
        assert!(!is_guest_token("secret", "123456", &forged));
    }

    #[test]
    fn wrong_codes_shut_the_door() {
        let pairing = Pairing::new("123456".into());
        assert!(matches!(
            pairing.attempt("t", "123 456"),
            Attempt::Paired(_)
        ));
        for _ in 0..TRIES {
            assert!(matches!(pairing.attempt("t", "000000"), Attempt::Wrong));
        }
        assert!(matches!(pairing.attempt("t", "123456"), Attempt::Locked(_)));
    }

    #[test]
    fn codes_have_six_digits() {
        let code = new_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }
}
