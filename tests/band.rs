//! The band's token: reads everything, plays along on the stage, writes
//! nothing — and ends when its time is up or it is taken back.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Band, Library, Options, Server};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-client: c1\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

fn status(answer: &str) -> &str {
    &answer[9..12]
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn the_band_reads_and_plays_along_but_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("cw-band-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.join("main"),
        token: "owner".into(),
        band: Some(Band {
            token: "band".into(),
            until: None,
        }),
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let hub = server.hub();

    let put = request(
        port,
        "owner",
        "PUT",
        "/api/user/record/songSettings",
        "{\"a\":1}",
    );
    assert_eq!(status(&put), "204", "{put}");

    // Reads.
    let read = request(port, "band", "GET", "/api/user/record/songSettings", "");
    assert_eq!(status(&read), "200", "{read}");
    assert!(read.contains("\\\"a\\\": 1"), "{read}");
    assert_eq!(
        status(&request(port, "band", "GET", "/api/library", "")),
        "200"
    );
    assert_eq!(
        status(&request(port, "band", "GET", "/api/changes", "")),
        "200"
    );

    // Writes nothing: no record, no removal, no backup.
    let write = request(port, "band", "PUT", "/api/user/record/songSettings", "{}");
    assert_eq!(status(&write), "403", "{write}");
    assert!(write.contains("read only"), "{write}");
    let remove = request(port, "band", "DELETE", "/api/user/record/songSettings", "");
    assert_eq!(status(&remove), "403", "{remove}");
    assert_eq!(
        status(&request(port, "band", "GET", "/api/backups", "")),
        "403"
    );
    assert_eq!(
        status(&request(port, "band", "POST", "/api/backups", "")),
        "403"
    );
    let still = request(port, "owner", "GET", "/api/user/record/songSettings", "");
    assert!(still.contains("\\\"a\\\": 1"), "{still}");

    // The stage is for everyone on it.
    let lead = request(
        port,
        "band",
        "POST",
        "/api/stage/main/lead",
        "{\"name\":\"Kim\"}",
    );
    assert_eq!(status(&lead), "200", "{lead}");
    let here = request(
        port,
        "band",
        "POST",
        "/api/stage/main/here",
        "{\"name\":\"Jo\"}",
    );
    assert_eq!(status(&here), "200", "{here}");

    // Health says who a token is.
    let health = request(port, "band", "GET", "/api/health", "");
    assert!(health.contains("\"access\":\"band\""), "{health}");
    assert!(request(port, "owner", "GET", "/api/health", "").contains("\"access\":\"owner\""));
    assert!(request(port, "nobody", "GET", "/api/health", "").contains("\"access\":\"none\""));
    assert!(!request(port, "", "GET", "/api/health", "").contains("\"access\""));

    // Its time is up: every route says so.
    let until = now() - 1;
    hub.set_band(
        None,
        Some(Band {
            token: "band".into(),
            until: Some(until),
        }),
    );
    let late = request(port, "band", "GET", "/api/library", "");
    assert_eq!(status(&late), "401", "{late}");
    assert!(late.contains("expired"), "{late}");
    assert!(request(port, "band", "GET", "/api/health", "").contains("\"access\":\"expired\""));

    // Extended, then taken back.
    let until = now() + 60_000;
    hub.set_band(
        None,
        Some(Band {
            token: "band".into(),
            until: Some(until),
        }),
    );
    let health = request(port, "band", "GET", "/api/health", "");
    assert!(health.contains(&format!("\"until\":{until}")), "{health}");
    assert_eq!(
        status(&request(port, "band", "GET", "/api/library", "")),
        "200"
    );
    hub.set_band(None, None);
    assert_eq!(hub.band(None), None);
    let gone = request(port, "band", "GET", "/api/library", "");
    assert_eq!(status(&gone), "401", "{gone}");
    assert!(gone.contains("unauthorised"), "{gone}");

    // One library's band does not open another.
    hub.add(
        "chor",
        Library {
            dir: dir.join("chor"),
            token: "chor".into(),
            backup_every: 0.0,
            backup_keep: 3,
        },
    )
    .unwrap();
    assert!(hub.set_band(
        Some("chor"),
        Some(Band {
            token: "singers".into(),
            until: None
        })
    ));
    assert!(!hub.set_band(Some("nope"), None));
    assert_eq!(
        status(&request(port, "singers", "GET", "/i/chor/api/library", "")),
        "200"
    );
    assert_eq!(
        status(&request(port, "singers", "GET", "/api/library", "")),
        "401"
    );
    let write = request(port, "singers", "PUT", "/i/chor/api/user/record/tags", "[]");
    assert_eq!(status(&write), "403", "{write}");

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
