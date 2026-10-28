//! A musician's token: reads everything, plays along on the stage, writes
//! only their own answers — and ends when its time is up or they are taken
//! off the list.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Can, Library, Member, Options, Server};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-client: c1\r\nx-chordwright-user: Somebody\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
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

fn member(id: &str, name: &str, token: &str, roles: &[&str], until: Option<i64>) -> Member {
    Member {
        id: id.into(),
        name: name.into(),
        token: token.into(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        until,
        ai: None,
        can: Default::default(),
    }
}

#[test]
fn musicians_read_play_along_and_write_only_their_answers() {
    let dir = std::env::temp_dir().join(format!("cw-members-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.join("main"),
        token: "owner".into(),
        members: vec![
            member("niko", "Niko", "niko-token", &["A", "S"], None),
            member("lea", "Lea", "lea-token", &["P"], None),
        ],
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
    let read = request(
        port,
        "niko-token",
        "GET",
        "/api/user/record/songSettings",
        "",
    );
    assert_eq!(status(&read), "200", "{read}");
    assert_eq!(
        status(&request(port, "niko-token", "GET", "/api/library", "")),
        "200"
    );
    assert_eq!(
        status(&request(port, "niko-token", "GET", "/api/changes", "")),
        "200"
    );

    // Their own answers, under their own name.
    let own = request(
        port,
        "niko-token",
        "PUT",
        "/api/user/record/availability.niko",
        "{\"2026-11-08\":\"yes\"}",
    );
    assert_eq!(status(&own), "204", "{own}");
    let change = request(
        port,
        "owner",
        "GET",
        "/api/changes?db=user&key=availability.niko",
        "",
    );
    assert!(change.contains("\"by\":\"Niko\""), "{change}");
    let gone = request(
        port,
        "niko-token",
        "DELETE",
        "/api/user/record/availability.niko",
        "",
    );
    assert_eq!(status(&gone), "204", "{gone}");

    // Nothing else: not someone else's answers, no setting, no backup.
    for (method, path) in [
        ("PUT", "/api/user/record/availability.lea"),
        ("PUT", "/api/user/record/songSettings"),
        ("DELETE", "/api/user/record/songSettings"),
        ("PUT", "/api/library/record/availability.niko"),
        ("GET", "/api/backups"),
        ("POST", "/api/backups"),
    ] {
        let refused = request(port, "niko-token", method, path, "{}");
        assert_eq!(status(&refused), "403", "{method} {path}: {refused}");
        assert!(refused.contains("read only"), "{refused}");
    }
    let still = request(port, "owner", "GET", "/api/user/record/songSettings", "");
    assert!(still.contains("\\\"a\\\": 1"), "{still}");

    // The stage is for everyone on it.
    let lead = request(
        port,
        "lea-token",
        "POST",
        "/api/stage/main/lead",
        "{\"name\":\"Lea\"}",
    );
    assert_eq!(status(&lead), "200", "{lead}");

    // Who plays, without anyone's token; and who is asking.
    let band = request(port, "lea-token", "GET", "/api/band", "");
    assert_eq!(status(&band), "200", "{band}");
    assert!(band.contains("\"me\":\"lea\""), "{band}");
    assert!(
        band.contains(
            "{\"id\":\"niko\",\"name\":\"Niko\",\"roles\":[\"A\",\"S\"],\"can\":\"read\"}"
        ),
        "{band}"
    );
    assert!(
        band.contains("{\"id\":\"S\",\"name\":\"Singer\"}"),
        "{band}"
    );
    assert!(!band.contains("token"), "{band}");
    assert!(request(port, "owner", "GET", "/api/band", "").contains("\"me\":null"));

    // Health says who a token is.
    let health = request(port, "niko-token", "GET", "/api/health", "");
    assert!(health.contains("\"access\":\"member\""), "{health}");
    assert!(health.contains("\"member\":{\"id\":\"niko\""), "{health}");
    assert!(request(port, "owner", "GET", "/api/health", "").contains("\"access\":\"owner\""));
    assert!(request(port, "nobody", "GET", "/api/health", "").contains("\"access\":\"none\""));
    assert!(!request(port, "", "GET", "/api/health", "").contains("\"access\""));

    // Niko's time is up; Lea plays on.
    hub.set_members(
        None,
        vec![
            member("niko", "Niko", "niko-token", &["A"], Some(now() - 1)),
            member("lea", "Lea", "lea-token", &["P"], Some(now() + 60_000)),
        ],
    );
    let late = request(port, "niko-token", "GET", "/api/library", "");
    assert_eq!(status(&late), "401", "{late}");
    assert!(late.contains("expired"), "{late}");
    assert!(
        request(port, "niko-token", "GET", "/api/health", "").contains("\"access\":\"expired\"")
    );
    assert_eq!(
        status(&request(port, "lea-token", "GET", "/api/library", "")),
        "200"
    );

    // Taken off the list.
    hub.set_members(None, vec![]);
    assert_eq!(hub.members(None), Some(vec![]));
    let out = request(port, "lea-token", "GET", "/api/library", "");
    assert_eq!(status(&out), "401", "{out}");
    assert!(out.contains("unauthorised"), "{out}");

    // One library's musicians do not open another.
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
    assert!(hub.set_members(
        Some("chor"),
        vec![member("jo", "Jo", "jo-token", &["S"], None)]
    ));
    assert!(!hub.set_members(Some("nope"), vec![]));
    assert_eq!(
        status(&request(port, "jo-token", "GET", "/i/chor/api/library", "")),
        "200"
    );
    assert_eq!(
        status(&request(port, "jo-token", "GET", "/api/library", "")),
        "401"
    );
    let own = request(
        port,
        "jo-token",
        "PUT",
        "/i/chor/api/user/record/availability.jo",
        "{}",
    );
    assert_eq!(status(&own), "204", "{own}");

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_a_musician_may_change_beyond_their_answers() {
    let dir = std::env::temp_dir().join(format!("cw-members-can-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let with = |m: Member, can: Can| Member { can, ..m };
    let server = Server::start(Options {
        dir: dir.join("main"),
        token: "owner".into(),
        members: vec![
            with(member("sam", "Sam", "sets-token", &["D"], None), Can::Sets),
            with(member("ali", "Ali", "all-token", &["P"], None), Can::All),
        ],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let song = |text: &str| format!("{{\"v\":1,\"data\":{{\"text\":\"{text}\"}}}}");
    let index = |ids: &[&str]| {
        let songs: Vec<String> = ids
            .iter()
            .map(|id| format!("{{\"id\":\"{id}\",\"chordProId\":\"{id}\"}}"))
            .collect();
        format!("{{\"v\":1,\"data\":[{}]}}", songs.join(","))
    };
    let ok = |token: &str, method: &str, path: &str, body: &str| {
        let answer = request(port, token, method, path, body);
        assert_eq!(status(&answer), "204", "{token} {method} {path}: {answer}");
    };
    let refused = |token: &str, method: &str, path: &str, body: &str| {
        let answer = request(port, token, method, path, body);
        assert_eq!(status(&answer), "403", "{token} {method} {path}: {answer}");
    };
    ok(
        "owner",
        "PUT",
        "/api/library/record/doc.a",
        &song("{title: A}"),
    );
    ok(
        "owner",
        "PUT",
        "/api/library/record/doc.b",
        &song("{title: B}"),
    );
    ok(
        "owner",
        "PUT",
        "/api/library/record/index",
        &index(&["a", "b"]),
    );

    // Sets: the sets and who plays in them, nothing of the songs.
    ok("sets-token", "PUT", "/api/user/record/setlists", "[]");
    ok("sets-token", "PUT", "/api/user/record/lineups", "{}");
    refused("sets-token", "PUT", "/api/user/record/songSettings", "{}");
    refused("sets-token", "PUT", "/api/library/record/doc.a", &song("x"));
    refused(
        "sets-token",
        "PUT",
        "/api/user/record/availability.ali",
        "{}",
    );

    // All: songs and arrangements too — but no song goes away, and no backup.
    ok(
        "all-token",
        "PUT",
        "/api/library/record/doc.a",
        &song("{title: A2}"),
    );
    ok(
        "all-token",
        "PUT",
        "/api/library/record/doc.c",
        &song("{title: C}"),
    );
    ok(
        "all-token",
        "PUT",
        "/api/library/record/index",
        &index(&["a", "b", "c"]),
    );
    ok("all-token", "PUT", "/api/user/record/songSettings", "{}");
    refused(
        "all-token",
        "PUT",
        "/api/library/record/index",
        &index(&["a", "c"]),
    );
    refused("all-token", "DELETE", "/api/library/record/doc.b", "");
    refused(
        "all-token",
        "PUT",
        "/api/user/record/availability.sam",
        "{}",
    );
    refused("all-token", "GET", "/api/backups", "");
    let index_now = request(port, "owner", "GET", "/api/library/record/index", "");
    assert!(index_now.contains("\\\"b\\\""), "{index_now}");

    // The owner still deletes.
    ok(
        "owner",
        "PUT",
        "/api/library/record/index",
        &index(&["a", "c"]),
    );
    ok("owner", "DELETE", "/api/library/record/doc.b", "");

    // What the app learns about itself.
    let health = request(port, "sets-token", "GET", "/api/health", "");
    assert!(health.contains("\"can\":\"sets\""), "{health}");
    let band = request(port, "owner", "GET", "/api/band", "");
    assert!(band.contains("\"can\":\"all\""), "{band}");
    assert!(
        band.contains("\"can\":\"read\"") || band.contains("\"can\":\"sets\""),
        "{band}"
    );

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
