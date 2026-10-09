//! chordwright serve, as a program:
//!
//!   chordwright-server --dir ./data --port 4174
//!   chordwright-server --addon            the Home Assistant add-on (addon.rs)
//!
//! The desktop app does not start this; it runs the library in its own
//! process (the library crate).

mod addon;

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;

use chordwright_server::{Options, Server};

struct Args {
    options: Options,
    addon: bool,
    ready_json: bool,
    exit_with_stdin: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let mut values = std::collections::HashMap::new();
    let mut flags = std::collections::HashSet::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if matches!(
            arg.as_str(),
            "--insecure" | "--ready-json" | "--exit-with-stdin" | "--addon"
        ) {
            flags.insert(arg[2..].to_string());
        } else if let Some(name) = arg.strip_prefix("--") {
            i += 1;
            values.insert(name.to_string(), argv.get(i).cloned().unwrap_or_default());
        }
        i += 1;
    }
    let get = |name: &str| values.get(name).cloned().filter(|v| !v.is_empty());
    let defaults = Options::default();
    let every = get("backup-every")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0);
    let keep = get("backup-keep")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(f64::floor)
        .filter(|v| *v != 0.0 && v.is_finite());
    Args {
        options: Options {
            dir: get("dir").map(PathBuf::from).unwrap_or(defaults.dir),
            host: get("host").unwrap_or(defaults.host),
            port: get("port")
                .and_then(|p| p.trim().parse::<u16>().ok())
                .unwrap_or(defaults.port),
            token: get("token").unwrap_or_default(),
            insecure: flags.contains("insecure"),
            cert: get("cert").map(PathBuf::from),
            key: get("key").map(PathBuf::from),
            ca_file: get("ca-file").map(PathBuf::from),
            own_ca: get("own-ca").map(PathBuf::from),
            hostnames: get("hostnames")
                .map(|h| {
                    h.split(',')
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            loopback_port: get("loopback-port").map(|p| p.trim().parse::<u16>().unwrap_or(0)),
            backup_every: every.unwrap_or(defaults.backup_every),
            backup_keep: keep
                .map(|k| k.max(1.0) as usize)
                .unwrap_or(defaults.backup_keep),
        },
        addon: flags.contains("addon"),
        ready_json: flags.contains("ready-json"),
        exit_with_stdin: flags.contains("exit-with-stdin"),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args = parse_args(&argv);
    if args.addon {
        args.options = addon::options().unwrap_or_else(|err| {
            eprintln!("{err}");
            std::process::exit(1);
        });
    }
    let o = args.options.clone();
    let server = match Server::start(args.options) {
        Ok(server) => server,
        Err(err) => {
            // A port taken by another program, a folder that cannot be
            // written: say so and go, rather than sit there half started.
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    let ready = server.ready();
    if args.ready_json {
        // For a program that started this one: one line it can parse. With a
        // loopback listener, `url` is that one — the starter's own way in.
        let mut line = serde_json::json!({
            "ready": true,
            "url": ready.local_url(),
            "port": ready.loopback_port.unwrap_or(ready.port),
            "dir": ready.dir.to_string_lossy(),
        });
        if let Some(lan) = &ready.lan {
            line["lan"] = serde_json::json!(lan);
        }
        println!("{line}");
    } else if !args.addon {
        println!("chordwright serve");
        println!("  data   {}", ready.dir.display());
        println!("  url    {}", ready.url);
        for url in ready.lan.iter().flat_map(|l| &l.urls) {
            println!("         {url}");
        }
        if let Some(port) = ready.loopback_port {
            println!("  local  http://127.0.0.1:{port}");
        }
        println!(
            "  auth   {}",
            if o.token.is_empty() {
                "none (loopback only)"
            } else {
                "token required"
            }
        );
        if o.backup_every > 0.0 {
            println!(
                "  backup every {} h when changed, newest {} kept",
                o.backup_every, o.backup_keep
            );
        } else {
            println!("  backup on request only");
        }
    }

    // Home Assistant and docker stop with SIGTERM: a wanted end, not a crash —
    // what the journal has not written yet is written first, then exit 0.
    let (stop, stopped) = mpsc::channel::<()>();
    let on_signal = stop.clone();
    std::thread::spawn(move || {
        wait_for_signal();
        let _ = on_signal.send(());
    });
    // A server started by another program goes when that program goes — also
    // when it crashes and never gets to stop it. Its end of our stdin closes
    // either way.
    if args.exit_with_stdin {
        std::thread::spawn(move || {
            let mut sink = [0u8; 1024];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            let _ = stop.send(());
        });
    }
    let _ = stopped.recv();
    drop(server);
    std::process::exit(0);
}

fn wait_for_signal() {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return std::thread::park();
    };
    runtime.block_on(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
                return;
            }
        }
        let _ = tokio::signal::ctrl_c().await;
    });
}
