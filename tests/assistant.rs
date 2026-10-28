//! The assistant through the server: the key stays here, the server decides
//! what a request may cost, and a musician's questions end at their limit.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use chordwright_server::{Assistant, Member, Options, Server};
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

/// Anthropic, as far as the server sees it: answers in turn, every request kept.
struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

fn fake(answers: Vec<(u16, Value)>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        let mut answers = answers.into_iter();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut head, mut length) = (String::new(), 0);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
                head.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            kept.lock()
                .unwrap()
                .push((head, serde_json::from_slice(&body).unwrap_or(Value::Null)));
            let (status, answer) = answers.next().unwrap_or((500, Value::Null));
            let text = answer.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            )
            .unwrap();
        }
    });
    Fake { port, seen }
}

fn reply(stop: &str, text: &str, input: u64, output: u64, searches: u64) -> (u16, Value) {
    (
        200,
        json!({
            "content": [{ "type": "text", "text": text }],
            "stop_reason": stop,
            "usage": { "input_tokens": input, "output_tokens": output, "server_tool_use": { "web_search_requests": searches } },
        }),
    )
}

fn member(id: &str, ai: Option<u32>) -> Member {
    Member {
        id: id.into(),
        name: id.to_uppercase(),
        token: format!("{id}-token"),
        roles: Vec::new(),
        until: None,
        can: Default::default(),
        ai,
    }
}

const ASK: &str = r#"{"kind":"form","system":"S","messages":[{"role":"user","content":"Form?"}],"model":"claude-haiku-5-5","max_tokens":999999}"#;

#[test]
fn questions_go_through_the_server_and_end_at_the_limit() {
    let dir = std::env::temp_dir().join(format!("cw-assistant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![member("niko", None), member("lea", Some(0))],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let hub = server.hub();

    // Without a key the assistant is off.
    let (status, health) = request(port, "niko-token", "GET", "/api/health", "");
    assert_eq!(
        (status.as_str(), &health["assistant"]),
        ("200", &json!(false))
    );
    assert_eq!(
        request(port, "niko-token", "POST", "/api/assistant", ASK).0,
        "503"
    );

    let anthropic = fake(vec![
        reply("pause_turn", "Ein", 100, 10, 2),
        reply("end_turn", "s", 300, 50, 1),
        reply("end_turn", "Zwei", 10, 5, 0),
        (401, json!({ "error": { "message": "invalid x-api-key" } })),
        reply("end_turn", "Chef", 1, 1, 0),
    ]);
    hub.set_assistant(
        None,
        Some(Assistant {
            key: "sk-ant-api03-test-key".into(),
            daily: 2,
            owner_daily: None,
            endpoint: Some(format!("http://127.0.0.1:{}", anthropic.port)),
        }),
    );

    // A paused turn is carried on by the server and comes back as one answer.
    let (status, first) = request(port, "niko-token", "POST", "/api/assistant", ASK);
    assert_eq!(status, "200", "{first}");
    assert_eq!(
        first["content"],
        json!([{ "type": "text", "text": "Ein" }, { "type": "text", "text": "s" }])
    );
    assert_eq!(
        first["usage"],
        json!({ "input": 400, "output": 60, "searches": 3 })
    );
    assert_eq!(first["quota"], json!({ "used": 1, "limit": 2 }));
    {
        let seen = anthropic.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let (head, body) = &seen[0];
        assert!(head.contains("x-api-key: sk-ant-api03-test-key"), "{head}");
        assert_eq!(
            body["model"], "claude-opus-5-5",
            "the server picks the model"
        );
        assert_eq!(body["max_tokens"], 16000, "and what it may cost");
        assert_eq!(
            seen[1].1["messages"][1]["role"], "assistant",
            "the paused turn goes on"
        );
    }

    let (status, second) = request(port, "niko-token", "POST", "/api/assistant", ASK);
    assert_eq!(
        (status.as_str(), &second["quota"]["used"]),
        ("200", &json!(2))
    );
    // The third one today is one too many — and Anthropic never hears of it.
    let (status, third) = request(port, "niko-token", "POST", "/api/assistant", ASK);
    assert_eq!((status.as_str(), &third["error"]), ("429", &json!("quota")));
    assert_eq!(anthropic.seen.lock().unwrap().len(), 3);
    let (_, health) = request(port, "niko-token", "GET", "/api/health", "");
    assert_eq!(health["assistantQuota"], json!({ "used": 2, "limit": 2 }));

    // Lea has none at all.
    assert_eq!(
        request(port, "lea-token", "POST", "/api/assistant", ASK).0,
        "403"
    );

    // A key Anthropic refuses: the owner learns why, and it costs nothing.
    let (status, refused) = request(port, "owner", "POST", "/api/assistant", ASK);
    assert_eq!(
        (status.as_str(), &refused["reason"]),
        ("502", &json!("key"))
    );
    assert_eq!(refused["quota"], json!({ "used": 0, "limit": null }));
    let (status, owner) = request(port, "owner", "POST", "/api/assistant", ASK);
    assert_eq!(
        (status.as_str(), &owner["quota"]["used"]),
        ("200", &json!(1))
    );

    // A history that does not end with the question is refused before it costs.
    let bad = r#"{"kind":"form","system":"S","messages":[{"role":"assistant","content":"x"}]}"#;
    assert_eq!(
        request(port, "owner", "POST", "/api/assistant", bad).0,
        "400"
    );

    // The month's figures, from the log.
    let tally = hub.assistant_tally(None).unwrap();
    let niko = tally.iter().find(|t| t.who == "niko").unwrap();
    assert_eq!(
        (niko.today, niko.month, niko.name.as_deref()),
        (2, 2, Some("NIKO"))
    );
    assert_eq!(niko.usage.input, 410);
    let owner = tally.iter().find(|t| t.who == "owner").unwrap();
    assert_eq!(
        owner.month, 1,
        "the refused one is in the log but did not count"
    );

    // After a restart today's counts are still there.
    drop(server);
    let server = Server::start(Options {
        dir: dir.clone(),
        token: "owner".into(),
        members: vec![member("niko", None)],
        port: 0,
        ..Options::default()
    })
    .unwrap();
    server.hub().set_assistant(
        None,
        Some(Assistant {
            key: "sk-ant-api03-test-key".into(),
            daily: 2,
            owner_daily: None,
            endpoint: Some(format!("http://127.0.0.1:{}", anthropic.port)),
        }),
    );
    let (status, _) = request(
        server.ready().port,
        "niko-token",
        "POST",
        "/api/assistant",
        ASK,
    );
    assert_eq!(status, "429");
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
