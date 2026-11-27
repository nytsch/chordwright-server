//! A server for the stage alone — a laptop on stage, no internet: it hands
//! out the shared stage and nothing of a library, and tells the QR code the
//! pin of its certificate authority.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{ca_pin, Options, Server};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-client: c1\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head[9..12].parse().unwrap();
    (status, raw[split + 4..].to_vec())
}

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[test]
fn only_the_stage_and_a_pin() {
    let dir = std::env::temp_dir().join(format!("cw-stage-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.join("stage"),
        token: "t".into(),
        host: "0.0.0.0".into(),
        port: 0,
        loopback_port: Some(0),
        own_ca: Some(dir.join("ca")),
        stage_only: true,
        ..Options::default()
    })
    .unwrap();
    let ready = server.ready().clone();
    let port = ready.loopback_port.unwrap();

    // The pin is the SHA-256 of what /ca.crt hands out.
    let (status, der) = request(port, "", "GET", "/ca.crt", "");
    assert_eq!(status, 200);
    let pin = ready
        .lan
        .as_ref()
        .and_then(|l| l.pin.clone())
        .expect("a pin");
    assert_eq!(pin, ca_pin(&der));

    let (status, health) = request(port, "t", "GET", "/api/health", "");
    let health = text(&health);
    assert_eq!(status, 200);
    assert!(health.contains(r#""stageOnly":true"#), "{health}");
    assert!(health.contains(r#""databases":[]"#), "{health}");
    assert!(!health.contains(r#""dir""#), "{health}");

    // The stage works.
    assert_eq!(request(port, "t", "GET", "/api/time", "").0, 200);
    let (status, lead) = request(
        port,
        "t",
        "POST",
        "/api/stage/Buehne/lead",
        r#"{"name":"Anna"}"#,
    );
    assert_eq!(status, 200, "{}", text(&lead));
    let (status, state) = request(
        port,
        "t",
        "PUT",
        "/api/stage/Buehne",
        r#"{"state":{"song":"s1"}}"#,
    );
    assert_eq!(status, 200, "{}", text(&state));
    let (status, rooms) = request(port, "t", "GET", "/api/stage", "");
    assert!(
        status == 200 && text(&rooms).contains("Buehne"),
        "{}",
        text(&rooms)
    );

    // Nothing of a library, whatever the token may.
    for (method, path, body) in [
        ("GET", "/api/user", ""),
        ("PUT", "/api/user/record/settings", r#"{"a":1}"#),
        ("GET", "/api/changes", ""),
        ("GET", "/api/backups", ""),
        ("POST", "/api/backups", ""),
        ("GET", "/api/band", ""),
        ("GET", "/api/messages", ""),
        ("GET", "/api/proposals", ""),
    ] {
        let (status, answer) = request(port, "t", method, path, body);
        assert_eq!(status, 404, "{method} {path}: {}", text(&answer));
        assert!(text(&answer).contains("stage only"));
    }
    // And without the token not even the stage.
    assert_eq!(request(port, "x", "GET", "/api/stage", "").0, 401);

    drop(server);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_library_server_still_serves_its_library() {
    let dir = std::env::temp_dir().join(format!("cw-stage-not-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.join("data"),
        token: "t".into(),
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    assert!(
        server.ready().lan.is_none(),
        "loopback only: no pin to show"
    );
    let (status, health) = request(port, "t", "GET", "/api/health", "");
    assert!(status == 200 && !text(&health).contains("stageOnly"));
    assert_eq!(
        request(port, "t", "PUT", "/api/user/record/settings", r#"{"a":1}"#).0,
        204
    );
    drop(server);
    std::fs::remove_dir_all(dir).unwrap();
}
