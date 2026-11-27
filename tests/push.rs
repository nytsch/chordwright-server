//! Push through the server: a phone signs up, a change to the sets or a new
//! message goes to the relay sealed for it — and only it can read what.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chordwright_server::{open_push, Member, Options, Server};
use ring::hmac;
use serde_json::{json, Value};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (String, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer[9..12].to_string();
    let body = answer.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

const SECRET: &str = "relay-secret";

/// The relay, as the server sees it: signs up, takes sends, keeps them.
struct Relay {
    port: u16,
    sent: Arc<Mutex<Vec<(String, String)>>>,
    gone: Arc<Mutex<bool>>,
}

fn relay() -> Relay {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let gone = Arc::new(Mutex::new(false));
    let (kept, gone2) = (sent.clone(), gone.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut first, mut length, mut signature) = (String::new(), 0, String::new());
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(n) = lower.strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
                if let Some(n) = lower.strip_prefix("x-chordwright-signature:") {
                    signature = n.trim().to_string();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let answer = if first.starts_with("POST /v1/server") {
                json!({ "serverId": "srv-1", "secret": SECRET })
            } else {
                let n = serde_json::from_str::<Value>(&body).unwrap()["items"]
                    .as_array()
                    .unwrap()
                    .len();
                kept.lock().unwrap().push((signature, body));
                let r = if *gone2.lock().unwrap() { "gone" } else { "ok" };
                json!({ "results": vec![r; n] })
            };
            let text = answer.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            )
            .unwrap();
        }
    });
    Relay { port, sent, gone }
}

fn member(id: &str) -> Member {
    Member {
        id: id.into(),
        name: id.to_uppercase(),
        token: format!("{id}-token"),
        roles: vec!["B".into()],
        until: None,
        can: Default::default(),
        plays: true,
        arrangements: vec![],
        ai: None,
        churchtools: None,
    }
}

const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const DEVICE: &str = "phone-lea-1";

/// The next send, opened as the phone would.
fn next(relay: &Relay, seen: &mut usize) -> Vec<Value> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some((signature, body)) = relay.sent.lock().unwrap().get(*seen).cloned() {
            *seen += 1;
            let key = hmac::Key::new(hmac::HMAC_SHA256, SECRET.as_bytes());
            let expected: String = hmac::sign(&key, body.as_bytes())
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(signature, expected, "signed with our secret");
            assert!(
                !body.contains("Sonntag") && !body.contains("Kommst"),
                "nothing in the clear"
            );
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["serverId"], "srv-1");
            return body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    assert_eq!(i["handle"], "handle-lea");
                    open_push(KEY, DEVICE, i["enc"].as_str().unwrap()).unwrap()
                })
                .collect();
        }
        assert!(Instant::now() < until, "nothing was sent");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn changes_go_sealed_to_the_phones_concerned() {
    let dir = std::env::temp_dir().join(format!("cw-push-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let relay = relay();
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![member("lea"), member("tom")],
        port: 0,
        push_relay: Some(format!("http://127.0.0.1:{}", relay.port)),
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    server.hub().set_push_settle(Duration::from_millis(200));

    // The first health signs up at the relay; then it says where to bind.
    let until = Instant::now() + Duration::from_secs(5);
    let health = loop {
        let (_, health) = request(port, "lea-token", "GET", "/api/health", "");
        if !health["push"].is_null() {
            break health;
        }
        assert!(Instant::now() < until, "never signed up");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(health["push"]["serverId"], "srv-1");

    let sign_up = format!(r#"{{"handle":"handle-lea","key":"{KEY}"}}"#);
    assert_eq!(
        request(port, "lea-token", "PUT", "/api/push/bad", &sign_up).0,
        "400"
    );
    assert_eq!(
        request(
            port,
            "lea-token",
            "PUT",
            "/api/push/phone-lea-1",
            r#"{"handle":"h","key":"short"}"#
        )
        .0,
        "400"
    );
    let (status, body) = request(port, "lea-token", "PUT", "/api/push/phone-lea-1", &sign_up);
    assert_eq!(
        (status.as_str(), &body["kinds"]),
        ("200", &json!(["assigned", "changed", "message", "day"]))
    );
    // The device file is the server's own, not the library's.
    assert!(dir.join(".chordwright/push.json").exists());

    let mut seen = 0;
    // A new set asks Lea, who has not answered for the day.
    let sets = r#"{"v":1,"data":[{"id":"s1","name":"Sonntag","date":"","eventDate":"2099-12-06","tags":[],"entries":[]}]}"#;
    assert_eq!(
        request(port, "owner", "PUT", "/api/user/record/setlists", sets).0,
        "204"
    );
    assert_eq!(
        next(&relay, &mut seen),
        vec![json!({ "v": 1, "kind": "day", "day": "2099-12-06", "names": ["Sonntag"] })]
    );

    // She is put in.
    let lineups = r#"{"v":1,"data":{"s1":{"B":["lea"]}}}"#;
    assert_eq!(
        request(port, "owner", "PUT", "/api/user/record/lineups", lineups).0,
        "204"
    );
    let got = next(&relay, &mut seen);
    assert_eq!(got[0]["kind"], "assigned");
    assert_eq!(got[0]["roles"], json!(["B"]));

    // Three edits in a row: one notification, after they settle.
    for n in 1..=3 {
        let sets = format!(
            r#"{{"v":1,"data":[{{"id":"s1","name":"Sonntag","date":"","eventDate":"2099-12-06","tags":[],"entries":[],"updatedAt":"{n}"}}]}}"#
        );
        assert_eq!(
            request(port, "owner", "PUT", "/api/user/record/setlists", &sets).0,
            "204"
        );
    }
    assert_eq!(next(&relay, &mut seen)[0]["kind"], "changed");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(relay.sent.lock().unwrap().len(), seen, "gathered into one");

    // A message to her.
    let (status, _) = request(
        port,
        "owner",
        "POST",
        "/api/messages",
        r#"{"text":"Kommst du?","to":"lea"}"#,
    );
    assert_eq!(status, "201");
    let got = next(&relay, &mut seen);
    assert_eq!(
        (
            got[0]["kind"].as_str(),
            got[0]["text"].as_str(),
            got[0]["from"].as_str()
        ),
        (Some("message"), Some("Kommst du?"), None)
    );

    // Only what she asked for.
    let only = format!(r#"{{"handle":"handle-lea","key":"{KEY}","kinds":["message"]}}"#);
    request(port, "lea-token", "PUT", "/api/push/phone-lea-1", &only);
    request(
        port,
        "owner",
        "PUT",
        "/api/user/record/lineups",
        r#"{"v":1,"data":{"s1":{"B":["lea"],"S":["lea"]}}}"#,
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(relay.sent.lock().unwrap().len(), seen);

    // A phone the relay says is gone is forgotten.
    *relay.gone.lock().unwrap() = true;
    request(
        port,
        "owner",
        "POST",
        "/api/messages",
        r#"{"text":"Noch da?","to":"lea"}"#,
    );
    next(&relay, &mut seen);
    std::thread::sleep(Duration::from_millis(200));
    let devices: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(".chordwright/push.json")).unwrap())
            .unwrap();
    assert_eq!(devices, json!([]));

    // Signing off: only one's own device.
    request(port, "lea-token", "PUT", "/api/push/phone-lea-1", &sign_up);
    assert_eq!(
        request(port, "tom-token", "DELETE", "/api/push/phone-lea-1", "").0,
        "404"
    );
    assert_eq!(
        request(port, "lea-token", "DELETE", "/api/push/phone-lea-1", "").0,
        "204"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
