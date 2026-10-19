//! The app for a browser on another computer: the files under `/` on a plain
//! http port, a code instead of the token, and a guest token that works only
//! while pairing is on.

use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{Files, Options, Server, Site};

struct Two;

impl Files for Two {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        match path {
            "index.html" => Some(Cow::Borrowed(b"<!doctype html><title>lite</title>")),
            "assets/app-1.js" => Some(Cow::Borrowed(b"console.log(1)")),
            _ => None,
        }
    }
}

fn request(port: u16, method: &str, path: &str, auth: Option<&str>, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = auth
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\n{auth}connection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

fn token_of(answer: &str) -> String {
    let body = answer.split("\r\n\r\n").nth(1).unwrap();
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    value["token"].as_str().unwrap().to_string()
}

#[test]
fn a_browser_pairs_with_a_code() {
    let dir = std::env::temp_dir().join(format!("cw-site-{}", std::process::id()));
    let options = Options {
        dir: dir.join("data"),
        token: "secret".into(),
        host: "0.0.0.0".into(),
        port: 0,
        plain_port: Some(0),
        site: Some(Site::new(Two)),
        pairing: Some("424242".into()),
        ..Options::default()
    };
    let server = Server::start(options.clone()).unwrap();
    let ready = server.ready().clone();
    let plain = ready.plain_port.unwrap();
    let lan = ready.lan.as_ref().unwrap();
    assert!(lan
        .site
        .iter()
        .all(|u| u.starts_with("http://") && u.ends_with(&format!(":{plain}"))));

    // The files, without any token: the page has to load before it can ask.
    let page = request(plain, "GET", "/", None, "");
    assert!(
        page.starts_with("HTTP/1.1 200") && page.contains("<title>lite</title>"),
        "{page}"
    );
    assert!(page.contains("content-type: text/html"), "{page}");
    let asset = request(plain, "GET", "/assets/app-1.js", None, "");
    assert!(
        asset.contains("immutable") && asset.contains("text/javascript"),
        "{asset}"
    );
    let deep = request(plain, "GET", "/library/x", None, "");
    assert!(deep.contains("<title>lite</title>"), "{deep}");
    assert!(request(plain, "GET", "/missing.js", None, "").starts_with("HTTP/1.1 404"));
    assert!(request(plain, "GET", "/../Cargo.toml", None, "").starts_with("HTTP/1.1 404"));

    // The library is not.
    assert!(request(plain, "GET", "/api/library", None, "").starts_with("HTTP/1.1 401"));
    let wrong = request(plain, "POST", "/api/pair", None, r#"{"code":"000000"}"#);
    assert!(wrong.starts_with("HTTP/1.1 403"), "{wrong}");
    let paired = request(plain, "POST", "/api/pair", None, r#"{"code":"424242"}"#);
    assert!(paired.starts_with("HTTP/1.1 200"), "{paired}");
    let guest = token_of(&paired);
    assert_ne!(guest, "secret");
    let read = request(plain, "GET", "/api/library", Some(&guest), "");
    assert!(read.starts_with("HTTP/1.1 200"), "{read}");
    let health = request(plain, "GET", "/api/health", None, "");
    assert!(health.contains(r#""pairing":true"#), "{health}");
    drop(server);

    // Restarted with the same code: still in. Another code, or none: out.
    let again = Server::start(options.clone()).unwrap();
    let port = again.ready().plain_port.unwrap();
    assert!(request(port, "GET", "/api/library", Some(&guest), "").starts_with("HTTP/1.1 200"));
    drop(again);
    let other = Server::start(Options {
        pairing: Some("111111".into()),
        ..options.clone()
    })
    .unwrap();
    let port = other.ready().plain_port.unwrap();
    assert!(request(port, "GET", "/api/library", Some(&guest), "").starts_with("HTTP/1.1 401"));
    drop(other);
    let closed = Server::start(Options {
        pairing: None,
        ..options
    })
    .unwrap();
    let port = closed.ready().plain_port.unwrap();
    assert!(request(port, "GET", "/api/library", Some(&guest), "").starts_with("HTTP/1.1 401"));
    assert!(
        request(port, "POST", "/api/pair", None, r#"{"code":"424242"}"#)
            .starts_with("HTTP/1.1 404")
    );
    assert!(request(port, "GET", "/api/library", Some("secret"), "").starts_with("HTTP/1.1 200"));
    drop(closed);
    let _ = std::fs::remove_dir_all(dir);
}
