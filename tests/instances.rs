//! More libraries on one port: `/i/<id>` with a folder and a token of its own,
//! added and removed while the server runs.

use std::io::{Read, Write};
use std::net::TcpStream;

use chordwright_server::{is_instance_id, Library, Options, Server};

fn request(port: u16, token: &str, method: &str, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nx-chordwright-user: Kim\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[test]
fn instances_have_their_own_folder_and_token() {
    let dir = std::env::temp_dir().join(format!("cw-instances-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = Server::start(Options {
        dir: dir.join("main"),
        token: "main".into(),
        port: 0,
        ..Options::default()
    })
    .unwrap();
    let port = server.ready().port;
    let hub = server.hub();
    let band = Library {
        dir: dir.join("band"),
        token: "band".into(),
        backup_every: 0.0,
        backup_keep: 3,
    };
    hub.add("band", band.clone()).unwrap();
    assert!(hub.add("band", band.clone()).is_err(), "the same id twice");
    assert!(
        hub.add(
            "other",
            Library {
                dir: dir.join("main"),
                ..band.clone()
            }
        )
        .is_err(),
        "the main folder twice"
    );
    assert!(hub.add("Bad/Id", band.clone()).is_err());
    assert_eq!(hub.ids(), ["band"]);

    let song = "{title: Lobe den Herren}\n[G]Lobe den Herren";
    let envelope = serde_json::json!({ "v": 1, "data": { "text": song } }).to_string();
    let put = request(
        port,
        "band",
        "PUT",
        "/i/band/api/library/record/doc.lobe",
        &envelope,
    );
    assert!(put.starts_with("HTTP/1.1 204"), "{put}");
    assert!(dir.join("band/library/songs/lobe.chordpro").exists());
    assert!(!dir.join("main/library/songs/lobe.chordpro").exists());

    // Each token opens its own library only.
    let wrong = request(port, "main", "GET", "/i/band/api/library", "");
    assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
    let main = request(port, "main", "GET", "/api/library/record/doc.lobe", "");
    assert!(main.starts_with("HTTP/1.1 404"), "{main}");
    let read = request(
        port,
        "band",
        "GET",
        "/i/band/api/library/record/doc.lobe?quiet=1",
        "",
    );
    assert!(
        read.starts_with("HTTP/1.1 200") && read.contains("Lobe"),
        "{read}"
    );
    let health = request(port, "", "GET", "/i/band/api/health", "");
    assert!(health.contains("band"), "{health}");

    let stats = hub.stats(Some("band")).unwrap();
    assert_eq!(stats.songs, 1);
    assert_eq!(stats.changes.len(), 1);
    assert_eq!(stats.changes[0].label, "Lobe den Herren");
    assert_eq!(stats.changes[0].by.as_deref(), Some("Kim"));
    assert_eq!(hub.stats(None).unwrap().songs, 0);
    assert!(hub.stats(Some("nope")).is_none());

    // Removed: the folder stays, the path is gone.
    assert!(hub.remove("band"));
    let gone = request(port, "band", "GET", "/i/band/api/library", "");
    assert!(gone.starts_with("HTTP/1.1 404"), "{gone}");
    assert!(dir.join("band/library/songs/lobe.chordpro").exists());
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn instance_ids() {
    assert!(is_instance_id("band-2"));
    assert!(!is_instance_id(""));
    assert!(!is_instance_id("-band"));
    assert!(!is_instance_id("Band"));
    assert!(!is_instance_id(&"a".repeat(33)));
}
