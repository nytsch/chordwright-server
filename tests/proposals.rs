//! A musician who may not write proposes changes; the owner accepts them —
//! each on the revision it was made on — or turns them down.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Can, Member, Options, Server};
use serde_json::{json, Value};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (String, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-client: c1\r\nx-chordwright-user: Chef\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer[9..12].to_string();
    let json = answer
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    (status, json)
}

fn member(id: &str, token: &str, can: Can) -> Member {
    Member {
        id: id.into(),
        name: id.to_uppercase(),
        token: token.into(),
        roles: Vec::new(),
        until: None,
        ai: None,
        plays: true,
        can,
    }
}

fn song(text: &str) -> String {
    json!({ "v": 1, "data": { "text": text, "origin": "seed" } }).to_string()
}

#[test]
fn proposals_wait_for_the_owner_and_land_on_their_base() {
    let dir = std::env::temp_dir().join(format!("cw-proposals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![
            member("lea", "lea-token", Can::Suggest),
            member("ben", "ben-token", Can::Read),
            member("ali", "ali-token", Can::All),
        ],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let req = |token: &str, method: &str, path: &str, body: &str| {
        request(port, token, method, path, body)
    };

    // The library as it is: two songs and the sets.
    for (key, value) in [
        ("library/record/doc.a", song("{title: A}")),
        ("library/record/doc.b", song("{title: B}")),
        ("user/record/setlists", "[]".to_string()),
    ] {
        let (status, _) = req("owner", "PUT", &format!("/api/{key}"), &value);
        assert_eq!(status, "204");
    }

    // A suggester reads everything but writes nothing directly …
    let (status, health) = req("lea-token", "GET", "/api/health", "");
    assert_eq!(status, "200");
    assert_eq!(health["proposals"], true);
    assert_eq!(health["member"]["can"], "suggest");
    let (status, _) = req("lea-token", "PUT", "/api/library/record/doc.a", &song("x"));
    assert_eq!(status, "403");

    // … and mirrors the library with its revisions.
    let (_, mirror) = req("lea-token", "GET", "/api/library?revs=1", "");
    let rev_a = mirror["revs"]["doc.a"].as_str().unwrap().to_string();
    let rev_b = mirror["revs"]["doc.b"].as_str().unwrap().to_string();
    let (_, user) = req("lea-token", "GET", "/api/user?revs=1", "");
    let rev_sets = user["revs"]["setlists"].as_str().unwrap().to_string();

    // Someone who only reads may not propose.
    let (status, _) = req(
        "ben-token",
        "POST",
        "/api/proposals",
        &json!({ "changes": [{ "db": "library", "key": "doc.a", "base": rev_a, "value": song("y") }] }).to_string(),
    );
    assert_eq!(status, "403");

    // What she changed in her copy goes back as one proposal. The unchanged
    // song is left out of it, another musician's answers are refused.
    let changes = json!([
        { "db": "library", "key": "doc.a", "base": rev_a, "value": song("{title: A}\n[G]neu") },
        { "db": "library", "key": "doc.b", "base": rev_b, "value": song("{title: B}") },
        { "db": "library", "key": "doc.c", "base": null, "value": song("{title: C}") },
        { "db": "user", "key": "setlists", "base": rev_sets, "value": "[{\"id\":\"s1\"}]" },
    ]);
    let (status, _) = req(
        "lea-token",
        "POST",
        "/api/proposals",
        &json!({ "changes": [{ "db": "user", "key": "availability.ali", "value": "{}" }] })
            .to_string(),
    );
    assert_eq!(status, "403");
    let (status, nothing) = req(
        "lea-token",
        "POST",
        "/api/proposals",
        &json!({ "changes": [{ "db": "library", "key": "doc.b", "base": rev_b, "value": song("{title: B}") }] }).to_string(),
    );
    assert_eq!(status, "400", "{nothing}");
    let (status, made) = req(
        "lea-token",
        "POST",
        "/api/proposals",
        &json!({ "title": "Neue Akkorde", "changes": changes }).to_string(),
    );
    assert_eq!(status, "201", "{made}");
    let id = made["id"].as_str().unwrap().to_string();
    assert_eq!(made["by"], json!({ "id": "lea", "name": "LEA" }));
    assert_eq!(made["open"], 3);
    assert_eq!(made["status"], "open");

    // Nothing of it is in the library yet.
    let (_, a) = req("owner", "GET", "/api/library/record/doc.a", "");
    assert_eq!(a["rev"], rev_a.as_str());
    let (status, _) = req("owner", "GET", "/api/library/record/doc.c", "");
    assert_eq!(status, "404");

    // She sees her own, the owner sees all, another musician none of them.
    let (_, own) = req("lea-token", "GET", "/api/proposals", "");
    assert_eq!(own["proposals"].as_array().unwrap().len(), 1);
    let (_, all) = req("owner", "GET", "/api/proposals", "");
    assert_eq!(all["proposals"][0]["id"], id.as_str());
    let (_, other) = req("ali-token", "GET", "/api/proposals", "");
    assert_eq!(other["proposals"], json!([]));
    let (status, _) = req("ali-token", "GET", &format!("/api/proposals/{id}"), "");
    assert_eq!(status, "404");

    // Only the owner accepts.
    let accept = format!("/api/proposals/{id}/accept");
    let (status, _) = req("lea-token", "POST", &accept, "{}");
    assert_eq!(status, "403");

    // Meanwhile the owner changed song A themselves: that one is a conflict.
    let (status, _) = req(
        "owner",
        "PUT",
        "/api/library/record/doc.a",
        &song("{title: A}\n[D]anders"),
    );
    assert_eq!(status, "204");
    let (_, detail) = req("owner", "GET", &format!("/api/proposals/{id}"), "");
    let stale: Vec<&str> = detail["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["stale"] == true)
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    assert_eq!(stale, ["doc.a"]);
    assert_eq!(detail["changes"][1]["label"], "C");

    let (status, done) = req("owner", "POST", &accept, "{}");
    assert_eq!(status, "200", "{done}");
    assert_eq!(
        done["applied"],
        json!(["library/doc.c", "user/setlists"]),
        "{done}"
    );
    assert_eq!(done["conflicts"][0]["key"], "doc.a");
    assert!(done["conflicts"][0]["value"]
        .as_str()
        .unwrap()
        .contains("anders"));
    assert_eq!(done["proposal"]["status"], "open");
    assert_eq!(done["proposal"]["open"], 1);
    let (_, c) = req("owner", "GET", "/api/library/record/doc.c", "");
    assert_eq!(c["change"]["by"], "LEA");
    let (_, sets) = req("owner", "GET", "/api/user/record/setlists", "");
    assert!(sets["value"].as_str().unwrap().contains("s1"));

    // Taking hers anyway closes it.
    let (status, done) = req("owner", "POST", &accept, "{\"force\":true}");
    assert_eq!(status, "200", "{done}");
    assert_eq!(done["applied"], json!(["library/doc.a"]));
    assert_eq!(done["proposal"]["status"], "accepted");
    assert_eq!(done["proposal"]["decidedBy"], "Chef");
    let (_, a) = req("owner", "GET", "/api/library/record/doc.a", "");
    assert!(a["value"].as_str().unwrap().contains("[G]neu"));
    // In the journal like any other write: under her name, saying what changed.
    let (_, changes) = req("owner", "GET", "/api/changes?limit=5", "");
    let entry = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "doc.a")
        .unwrap()
        .clone();
    assert_eq!(entry["by"], "LEA", "{entry}");
    assert!(entry["detail"].is_array(), "{entry}");
    let (status, _) = req("owner", "POST", &accept, "{}");
    assert_eq!(status, "409");

    // A second one: withdrawn by her, a third turned down by the owner.
    let one = |title: &str| {
        json!({ "title": title, "changes": [{ "db": "library", "key": "doc.d", "value": song("{title: D}") }] })
            .to_string()
    };
    let (_, second) = req("lea-token", "POST", "/api/proposals", &one("zwei"));
    let (_, third) = req("lea-token", "POST", "/api/proposals", &one("drei"));
    let second = second["id"].as_str().unwrap().to_string();
    let third = third["id"].as_str().unwrap().to_string();
    let (status, _) = req(
        "lea-token",
        "DELETE",
        &format!("/api/proposals/{second}"),
        "",
    );
    assert_eq!(status, "204");
    let (status, _) = req("owner", "DELETE", &format!("/api/proposals/{third}"), "");
    assert_eq!(status, "204");
    let (_, list) = req("lea-token", "GET", "/api/proposals", "");
    let states: Vec<(&str, &str)> = list["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["title"].as_str().unwrap(), p["status"].as_str().unwrap()))
        .collect();
    assert!(states.contains(&("zwei", "withdrawn")), "{states:?}");
    assert!(states.contains(&("drei", "rejected")), "{states:?}");
    assert!(states.contains(&("Neue Akkorde", "accepted")), "{states:?}");
    let (status, _) = req("owner", "GET", "/api/library/record/doc.d", "");
    assert_eq!(status, "404");

    // A decided one, deleted again, is gone.
    let (status, _) = req("owner", "DELETE", &format!("/api/proposals/{third}"), "");
    assert_eq!(status, "204");
    let (status, _) = req("owner", "GET", &format!("/api/proposals/{third}"), "");
    assert_eq!(status, "404");

    // Kept outside the library: no backup carries them.
    assert!(dir.join(".chordwright/proposals").is_dir());
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_admin_takes_proposals_but_leaves_removing_songs_to_the_owner() {
    let dir = std::env::temp_dir().join(format!("cw-proposals-admin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![
            member("lea", "lea-token", Can::Suggest),
            member("max", "max-token", Can::Admin),
            member("ali", "ali-token", Can::All),
        ],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let req = |token: &str, method: &str, path: &str, body: &str| {
        request(port, token, method, path, body)
    };
    let index = |ids: &[&str]| {
        let songs: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "id": id, "chordProId": id }))
            .collect();
        json!({ "v": 1, "data": songs }).to_string()
    };
    for (key, value) in [
        ("doc.a", song("{title: A}")),
        ("doc.b", song("{title: B}")),
        ("index", index(&["a", "b"])),
    ] {
        let (status, _) = req(
            "owner",
            "PUT",
            &format!("/api/library/record/{key}"),
            &value,
        );
        assert_eq!(status, "204");
    }
    let (_, health) = req("max-token", "GET", "/api/health", "");
    assert_eq!(health["member"]["can"], "admin");
    // An admin writes like `all`, but deletes no song directly either.
    let (status, _) = req("max-token", "DELETE", "/api/library/record/doc.b", "");
    assert_eq!(status, "403");

    let (_, mirror) = req("lea-token", "GET", "/api/library?revs=1", "");
    let rev = |key: &str| mirror["revs"][key].as_str().unwrap().to_string();
    let (status, made) = req(
        "lea-token",
        "POST",
        "/api/proposals",
        &json!({ "title": "A neu, B weg", "changes": [
            { "db": "library", "key": "doc.a", "base": rev("doc.a"), "value": song("{title: A}\n[D]neu") },
            { "db": "library", "key": "doc.b", "base": rev("doc.b"), "value": null },
            { "db": "library", "key": "index", "base": rev("index"), "value": index(&["a"]) },
        ] })
        .to_string(),
    );
    assert_eq!(status, "201", "{made}");
    let id = made["id"].as_str().unwrap().to_string();

    // The admin sees everyone's; one with `all` only their own.
    let (_, seen) = req("max-token", "GET", "/api/proposals", "");
    assert_eq!(seen["proposals"][0]["id"], id.as_str());
    let (_, seen) = req("ali-token", "GET", "/api/proposals", "");
    assert_eq!(seen["proposals"], json!([]));
    let accept = format!("/api/proposals/{id}/accept");
    let (status, _) = req("ali-token", "POST", &accept, "{}");
    assert_eq!(status, "404");

    // The admin takes the new text; removing B (file and index) stays open.
    let (status, done) = req("max-token", "POST", &accept, "{\"force\":true}");
    assert_eq!(status, "200", "{done}");
    assert_eq!(done["applied"], json!(["library/doc.a"]));
    assert_eq!(done["held"], json!(["library/doc.b", "library/index"]));
    assert_eq!(done["proposal"]["status"], "open");
    let (_, b) = req("owner", "GET", "/api/library/record/doc.b", "");
    assert!(b["value"].is_string(), "B is still there");

    // The owner takes the rest.
    let (status, done) = req("owner", "POST", &accept, "{}");
    assert_eq!(status, "200", "{done}");
    assert_eq!(done["applied"], json!(["library/doc.b", "library/index"]));
    assert_eq!(done["proposal"]["status"], "accepted");
    let (status, _) = req("owner", "GET", "/api/library/record/doc.b", "");
    assert_eq!(status, "404");

    // An admin turns one down too.
    let (_, other) = req(
        "lea-token",
        "POST",
        "/api/proposals",
        &json!({ "changes": [{ "db": "library", "key": "doc.c", "value": song("{title: C}") }] })
            .to_string(),
    );
    let other = other["id"].as_str().unwrap();
    let (status, _) = req(
        "max-token",
        "DELETE",
        &format!("/api/proposals/{other}"),
        "",
    );
    assert_eq!(status, "204");
    let (_, list) = req("lea-token", "GET", "/api/proposals", "");
    assert!(list["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == other && p["status"] == "rejected"));
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
