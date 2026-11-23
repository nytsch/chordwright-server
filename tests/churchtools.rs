//! ChurchTools through the server: the token stays here, only named reading
//! routes go through, and a musician sees only their own dates.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use chordwright_server::{Can, ChurchTools, Member, Options, Server};
use serde_json::{json, Value};

fn request(port: u16, token: &str, path: &str) -> (String, String, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let at = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&answer[..at]).to_string();
    (head[9..12].to_string(), head, answer[at + 4..].to_vec())
}

fn get(port: u16, token: &str, path: &str) -> (String, Value) {
    let (status, _, body) = request(port, token, path);
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// ChurchTools, as far as the server sees it: an answer by path, every request kept.
struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

fn fake(answer: fn(&str) -> (u16, &'static str, String)) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            let path = head.split(' ').nth(1).unwrap_or("").to_string();
            kept.lock().unwrap().push(head);
            let (status, kind, body) = answer(&path);
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    Fake { port, seen }
}

fn event(id: u64, name: &str, people: &[u64]) -> Value {
    let services: Vec<Value> = people
        .iter()
        .map(|p| json!({ "id": p * 10, "personId": p, "name": "Band", "agreed": true }))
        .collect();
    json!({ "id": id, "name": name, "startDate": "2026-12-06T09:30:00Z", "eventServices": services })
}

fn churchtools(path: &str) -> (u16, &'static str, String) {
    let json = "application/json";
    let path_only = path.split('?').next().unwrap();
    match path_only {
        "/api/whoami" => (
            200,
            json,
            json!({ "data": { "id": 5, "firstName": "Chordwright", "lastName": "Leser" } })
                .to_string(),
        ),
        "/api/events" => (
            200,
            json,
            json!({
                "data": [event(1, "Gottesdienst", &[11, 12]), event(2, "Lobpreisabend", &[11])],
                "meta": { "pagination": { "total": 2 } },
            })
            .to_string(),
        ),
        "/api/events/1" => (200, json, json!({ "data": event(1, "Gottesdienst", &[12]) }).to_string()),
        "/api/events/1/agenda" => (
            200,
            json,
            json!({ "data": { "items": [{ "type": "song", "song": { "songId": 77, "arrangementId": 3, "title": "Lord I Need You", "key": "A" } }] } })
                .to_string(),
        ),
        "/api/events/2" => (200, json, json!({ "data": event(2, "Lobpreisabend", &[]) }).to_string()),
        "/api/events/2/agenda" => (404, json, json!({ "message": "no agenda" }).to_string()),
        "/api/songs" => (
            200,
            json,
            json!({ "data": [{ "id": 77, "name": "Lord I Need You", "arrangements": [{ "id": 3, "key": "A", "files": [] }] }] })
                .to_string(),
        ),
        "/api/songs/77" => (200, json, json!({ "data": { "id": 77, "name": "Lord I Need You" } }).to_string()),
        "/" if path.contains("filedownload") => (
            200,
            "text/plain; charset=utf-8",
            "{title: Lord I Need You}\n[A]Lord".into(),
        ),
        "/api/songs/66" => (401, json, json!({ "message": "Session expired" }).to_string()),
        _ => (404, json, "{}".into()),
    }
}

fn member(id: &str, can: Can, person: Option<u64>) -> Member {
    Member {
        id: id.into(),
        name: id.to_uppercase(),
        token: format!("{id}-token"),
        roles: Vec::new(),
        until: None,
        can,
        plays: true,
        ai: None,
        arrangements: vec![],
        churchtools: person,
    }
}

#[test]
fn dates_services_and_songs_come_through_the_server() {
    let dir = std::env::temp_dir().join(format!("cw-churchtools-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![
            member("lea", Can::Sets, Some(11)),
            member("max", Can::Read, Some(12)),
            member("ole", Can::Read, None),
        ],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;

    // Without ChurchTools it is off.
    let (_, health) = get(port, "owner", "/api/health");
    assert_eq!(health["churchtools"], json!(false));
    assert_eq!(get(port, "owner", "/api/churchtools").0, "503");

    let ct = fake(churchtools);
    let token = "geheimer-login-token-0815";
    let mut config = ChurchTools::new(&format!("http://127.0.0.1:{}/", ct.port), token).unwrap();
    config.user = Some("Chordwright Leser".into());
    assert!(server.hub().set_churchtools(None, Some(config)));
    let (_, health) = get(port, "max-token", "/api/health");
    assert_eq!(health["churchtools"], json!(true));
    assert_eq!(health["member"]["churchtools"], json!(12));

    // Whom the token belongs to, asked with `Login <token>`.
    let (status, me) = get(port, "owner", "/api/churchtools");
    assert_eq!(status, "200");
    assert_eq!(me["user"]["firstName"], json!("Chordwright"));
    let first = ct.seen.lock().unwrap()[0].clone();
    assert!(first.starts_with("GET /api/whoami HTTP/1.1"), "{first}");
    assert!(
        first
            .to_lowercase()
            .contains(&format!("authorization: login {token}")),
        "{first}"
    );

    // The dates, each service with the musician who is that person.
    let (status, events) = get(
        port,
        "lea-token",
        "/api/churchtools/events?from=2026-12-01&to=2026-12-31&limit=50",
    );
    assert_eq!(status, "200");
    assert_eq!(events["data"].as_array().unwrap().len(), 2);
    let services = &events["data"][0]["eventServices"];
    assert_eq!(services[0]["member"], json!("lea"));
    assert_eq!(services[1]["member"], json!("max"));
    assert_eq!(events["meta"]["pagination"]["total"], json!(2));
    let asked = ct.seen.lock().unwrap().last().unwrap().clone();
    assert!(
        asked.starts_with(
            "GET /api/events?include%5B%5D=eventServices&from=2026-12-01&to=2026-12-31&limit=50 "
        ),
        "{asked}"
    );
    // Only the dates a person serves at.
    let (_, only) = get(port, "owner", "/api/churchtools/events?person=12");
    let names: Vec<&str> = only["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Gottesdienst"]);
    assert_eq!(
        get(port, "owner", "/api/churchtools/events?from=morgen").0,
        "400"
    );

    // One date: who serves, and the songs of its agenda; without one, null.
    let (_, one) = get(port, "owner", "/api/churchtools/events/1");
    assert_eq!(one["event"]["eventServices"][0]["member"], json!("max"));
    assert_eq!(one["agenda"]["items"][0]["song"]["songId"], json!(77));
    let (status, two) = get(port, "owner", "/api/churchtools/events/2");
    assert_eq!((status.as_str(), &two["agenda"]), ("200", &Value::Null));
    assert_eq!(get(port, "owner", "/api/churchtools/events/abc").0, "404");
    assert_eq!(get(port, "owner", "/api/churchtools/events/9").0, "404");

    // Songs, searched by name; one song; a file of an arrangement as it is.
    let (_, songs) = get(port, "owner", "/api/churchtools/songs?q=Lord%20I&page=1");
    assert_eq!(songs["data"][0]["id"], json!(77));
    let asked = ct.seen.lock().unwrap().last().unwrap().clone();
    assert!(
        asked.starts_with("GET /api/songs?page=1&name=Lord%20I "),
        "{asked}"
    );
    assert_eq!(
        get(port, "owner", "/api/churchtools/songs/77").1["data"]["name"],
        json!("Lord I Need You")
    );
    let file = format!("http://127.0.0.1:{}/?q=public/filedownload&id=4", ct.port);
    let (status, head, body) = request(
        port,
        "owner",
        &format!(
            "/api/churchtools/file?url={}",
            file.replace('&', "%26").replace('?', "%3F")
        ),
    );
    assert_eq!(status, "200");
    assert!(head.contains("text/plain"));
    assert_eq!(body, b"{title: Lord I Need You}\n[A]Lord");
    // The token goes to this ChurchTools and nowhere else.
    let before = ct.seen.lock().unwrap().len();
    for elsewhere in [
        "http://127.0.0.1:1/x".to_string(),
        format!("http://127.0.0.1:{}0/x", ct.port),
        "https://example.org/".to_string(),
    ] {
        let (status, _) = get(
            port,
            "owner",
            &format!("/api/churchtools/file?url={elsewhere}"),
        );
        assert_eq!(status, "400", "{elsewhere}");
    }
    assert_eq!(ct.seen.lock().unwrap().len(), before);

    // A token ChurchTools no longer takes.
    let (status, no) = get(port, "owner", "/api/churchtools/songs/66");
    assert_eq!(status, "502");
    assert_eq!(no["reason"], json!("token"));

    // A musician who only reads: their own dates, nothing else.
    assert_eq!(get(port, "max-token", "/api/churchtools/events").0, "403");
    assert_eq!(get(port, "max-token", "/api/churchtools/songs").0, "403");
    let (status, mine) = get(port, "max-token", "/api/churchtools/mine?from=2026-12-01");
    assert_eq!(status, "200");
    assert_eq!(mine["data"].as_array().unwrap().len(), 1);
    assert_eq!(mine["data"][0]["name"], json!("Gottesdienst"));
    assert_eq!(get(port, "ole-token", "/api/churchtools/mine").0, "404");
    // Leaders find theirs the same way.
    let (_, leas) = get(port, "lea-token", "/api/churchtools/mine");
    assert_eq!(leas["data"].as_array().unwrap().len(), 2);
    assert_eq!(get(port, "owner", "/api/churchtools/mine").0, "404");

    // Reading only, and the token is in no answer.
    let (status, _, _) = {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "POST /api/churchtools/events HTTP/1.1\r\nhost: x\r\nauthorization: Bearer owner\r\nconnection: close\r\ncontent-length: 0\r\n\r\n").unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        (answer[9..12].to_string(), (), ())
    };
    assert_eq!(status, "405");
    for path in [
        "/api/health",
        "/api/band",
        "/api/churchtools",
        "/api/churchtools/events",
    ] {
        let (_, _, body) = request(port, "owner", path);
        assert!(!String::from_utf8_lossy(&body).contains(token), "{path}");
    }

    // Off again.
    assert!(server.hub().set_churchtools(None, None));
    assert_eq!(get(port, "owner", "/api/churchtools/events").0, "503");
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
