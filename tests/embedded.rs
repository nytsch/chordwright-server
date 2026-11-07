//! The server as the desktop app runs it: started and stopped from inside a
//! tokio runtime (Tauri's), shared in the network and then not, on the same
//! port again after a restart.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Options, Server};

fn request(port: u16, method: &str, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer t\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[tokio::test(flavor = "multi_thread")]
async fn start_stop_restart_inside_a_runtime() {
    let dir = std::env::temp_dir().join(format!("cw-embedded-{}", std::process::id()));
    let shared = Options {
        dir: dir.join("data"),
        token: "t".into(),
        host: "0.0.0.0".into(),
        port: 0,
        loopback_port: Some(0),
        own_ca: Some(dir.join("ca")),
        ..Options::default()
    };
    let server = Server::start(shared.clone()).unwrap();
    let ready = server.ready().clone();
    assert!(
        ready.tls
            && ready
                .lan
                .as_ref()
                .is_some_and(|l| l.ca && !l.urls.is_empty())
    );
    let local = ready.loopback_port.unwrap();
    let put = request(local, "PUT", "/api/user/record/settings", r#"{"a":1}"#);
    assert!(put.starts_with("HTTP/1.1 204"), "{put}");
    drop(server);

    // The same port again, right away: the old listener is gone with the server.
    let again = Server::start(Options {
        port: ready.port,
        ..shared
    })
    .unwrap();
    assert_eq!(again.ready().port, ready.port);
    let local = again.ready().loopback_port.unwrap();
    let read = request(local, "GET", "/api/user/record/settings", "");
    assert!(
        read.starts_with("HTTP/1.1 200") && read.contains(r#"\"a\": 1"#),
        "{read}"
    );
    let changes = request(local, "GET", "/api/changes", "");
    assert!(
        changes.contains(r#""action":"write""#),
        "the journal was written on stop: {changes}"
    );
    drop(again);

    // Not shared: loopback only, plain http.
    let plain = Server::start(Options {
        dir: dir.join("data"),
        token: "t".into(),
        port: 0,
        ..Options::default()
    })
    .unwrap();
    assert!(!plain.ready().tls && plain.ready().lan.is_none());
    assert!(request(plain.ready().port, "GET", "/api/health", "").starts_with("HTTP/1.1 200"));
    drop(plain);
    std::fs::remove_dir_all(dir).unwrap();
}

/// What `/api/changes` says about a write: what changed in it, and nothing for
/// a write that left the record as it was.
#[tokio::test(flavor = "multi_thread")]
async fn changes_say_what_changed() {
    let dir = std::env::temp_dir().join(format!("cw-detail-{}", std::process::id()));
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "t".into(),
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let put = |key: &str, body: &str| {
        let answer = request(port, "PUT", &format!("/api/user/record/{key}"), body);
        assert!(answer.starts_with("HTTP/1.1 204"), "{answer}");
    };
    put("tags", r#"{"v":1,"data":["Anbetung"]}"#);
    let changes = request(port, "GET", "/api/changes", "");
    assert!(
        changes.contains(r#""detail":[{"kind":"created"}]"#),
        "{changes}"
    );

    // Again with the same tags, as an app does after every song it saves.
    put("tags", r#"{"v":1,"data":["Anbetung"]}"#);
    put(
        "setlists",
        r#"{"v":1,"data":[{"id":"s","name":"Sonntag","entries":[]}]}"#,
    );
    put(
        "setlists",
        r#"{"v":1,"data":[{"id":"s","name":"Sonntag","entries":[{"songId":"lord"}]}]}"#,
    );
    let changes = request(port, "GET", "/api/changes", "");
    assert_eq!(changes.matches(r#""key":"tags""#).count(), 1, "{changes}");
    // Two writes to the sets in a row: one entry, described from the start.
    assert_eq!(
        changes.matches(r#""key":"setlists""#).count(),
        1,
        "{changes}"
    );
    assert!(changes.contains(r#"{"kind":"created"}"#), "{changes}");
    drop(server);
    std::fs::remove_dir_all(dir).unwrap();
}
