//! A musician reports something to those who lead, a leader writes to a
//! musician or to all, and whoever got it says that they read it.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Can, Member, Options, Server};
use serde_json::{json, Value};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (String, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-user: Chef\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
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

fn member(id: &str, can: Can) -> Member {
    Member {
        id: id.into(),
        name: id.to_uppercase(),
        token: format!("{id}-token"),
        roles: Vec::new(),
        until: None,
        ai: None,
        voice: None,
        plays: true,
        can,
    }
}

#[test]
fn messages_reach_who_they_are_for_and_say_when_they_were_read() {
    let dir = std::env::temp_dir().join(format!("cw-messages-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![
            member("lea", Can::Read),
            member("ben", Can::Read),
            member("ali", Can::Sets),
        ],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let req = |token: &str, method: &str, path: &str, body: Value| {
        let body = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        request(port, token, method, path, &body)
    };
    let ids = |token: &str| -> Vec<String> {
        let (status, list) = req(token, "GET", "/api/messages", Value::Null);
        assert_eq!(status, "200");
        list["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };

    let (_, health) = req("lea-token", "GET", "/api/health", Value::Null);
    assert_eq!(health["messages"], true);

    // A musician reports a mistake: it goes to the leaders, under her name.
    let (status, bug) = req(
        "lea-token",
        "POST",
        "/api/messages",
        json!({ "kind": "bug", "text": "  Akkord in Takt 4 falsch\u{7} " }),
    );
    assert_eq!(status, "201");
    assert_eq!(bug["to"], "leaders");
    assert_eq!(bug["kind"], "bug");
    assert_eq!(bug["text"], "Akkord in Takt 4 falsch");
    assert_eq!(bug["from"], json!({ "id": "lea", "name": "LEA" }));
    assert_eq!(bug["read"], true);
    assert_eq!(bug["readBy"], json!([]));
    let bug_id = bug["id"].as_str().unwrap().to_string();

    // She writes to nobody else, and not nothing.
    for (body, expected) in [
        (json!({ "to": "ben", "text": "Hi" }), "403"),
        (json!({ "to": "all", "text": "Hi" }), "403"),
        (json!({ "text": "  " }), "400"),
        (json!({ "text": "x", "kind": "Not Good" }), "400"),
    ] {
        let (status, _) = req("lea-token", "POST", "/api/messages", body);
        assert_eq!(status, expected);
    }

    // The owner and who edits the sets see it, another musician does not.
    assert_eq!(ids("owner"), [bug_id.as_str()]);
    assert_eq!(ids("ali-token"), [bug_id.as_str()]);
    assert!(ids("ben-token").is_empty());
    let read = format!("/api/messages/{bug_id}/read");
    let (status, _) = req("ben-token", "POST", &read, Value::Null);
    assert_eq!(status, "404");

    // Reading it is booked once, with name and time, and she sees it.
    let (status, seen) = req("ali-token", "POST", &read, Value::Null);
    assert_eq!(status, "200");
    assert_eq!(seen["read"], true);
    let (_, again) = req("ali-token", "POST", &read, Value::Null);
    assert_eq!(again["readBy"].as_array().unwrap().len(), 1);
    let (_, list) = req("lea-token", "GET", "/api/messages", Value::Null);
    let by = &list["messages"][0]["readBy"];
    assert_eq!(by.as_array().unwrap().len(), 1);
    assert_eq!(by[0]["id"], "ali");
    assert_eq!(by[0]["name"], "ALI");
    assert!(by[0]["at"].is_string());
    let (_, list) = req("owner", "GET", "/api/messages", Value::Null);
    assert_eq!(list["messages"][0]["read"], false);

    // A leader tells one musician, and all of them.
    let (status, told) = req(
        "ali-token",
        "POST",
        "/api/messages",
        json!({ "to": "ben", "kind": "assigned", "text": "Du wurdest einem Termin zugeteilt" }),
    );
    assert_eq!(status, "201");
    let told_id = told["id"].as_str().unwrap().to_string();
    let (status, _) = req(
        "ali-token",
        "POST",
        "/api/messages",
        json!({ "to": "nobody", "text": "x" }),
    );
    assert_eq!(status, "400");
    let (status, news) = req(
        "owner",
        "POST",
        "/api/messages",
        json!({ "to": "all", "text": "Probe fällt aus" }),
    );
    assert_eq!(status, "201");
    assert_eq!(news["from"], json!({ "id": "owner", "name": "Chef" }));
    let news_id = news["id"].as_str().unwrap().to_string();

    // Newest first; a musician sees what is theirs and not who else read it.
    assert_eq!(ids("ben-token"), [news_id.as_str(), told_id.as_str()]);
    assert_eq!(ids("lea-token"), [news_id.as_str(), bug_id.as_str()]);
    let (_, list) = req("ben-token", "GET", "/api/messages", Value::Null);
    assert_eq!(list["messages"][1]["read"], false);
    assert!(list["messages"][1].get("readBy").is_none());
    req(
        "ben-token",
        "POST",
        &format!("/api/messages/{told_id}/read"),
        Value::Null,
    );
    let (_, list) = req("ali-token", "GET", "/api/messages", Value::Null);
    let told = list["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == told_id.as_str())
        .unwrap();
    assert_eq!(told["readBy"][0]["id"], "ben");

    // Only its sender or the owner removes one.
    let gone = format!("/api/messages/{told_id}");
    let (status, _) = req("ben-token", "DELETE", &gone, Value::Null);
    assert_eq!(status, "403");
    let (status, _) = req("ali-token", "DELETE", &gone, Value::Null);
    assert_eq!(status, "204");
    let (status, _) = req(
        "owner",
        "DELETE",
        &format!("/api/messages/{bug_id}"),
        Value::Null,
    );
    assert_eq!(status, "204");
    assert_eq!(ids("owner"), [news_id.as_str()]);

    // They lie next to the library, not in it.
    assert!(dir.join(".chordwright/messages.json").exists());
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
