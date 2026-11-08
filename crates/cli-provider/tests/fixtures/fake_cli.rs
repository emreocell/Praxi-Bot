use std::io::{Read, Write};
use std::time::Duration;

fn probe_descendant() {
    let mut stream = std::net::TcpStream::connect(std::env::var("PROBE_TREE_ADDRESS").unwrap())
        .expect("connect tree observer");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    std::fs::write(
        std::env::var("PROBE_TREE_READY").unwrap(),
        std::process::id().to_string(),
    )
    .unwrap();
    stream.write_all(b"ready").unwrap();
    let mut command = [0];
    while stream.read_exact(&mut command).is_ok() {
        match command[0] {
            b'p' => stream.write_all(b"p").unwrap(),
            b'q' => std::process::exit(0),
            _ => panic!("unknown tree observer command"),
        }
    }
}

#[allow(clippy::zombie_processes)] // Exiting before the descendant is the regression fixture.
fn probe_tree(args: &[String]) -> bool {
    if std::env::var("PROBE_TREE_DESCENDANT").is_ok() {
        probe_descendant();
        return true;
    }
    let Ok(mode) = std::env::var("PROBE_TREE_MODE") else {
        return false;
    };
    if args != ["--version"] {
        return false;
    }
    let ready = std::env::var("PROBE_TREE_READY").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PROBE_TREE_DESCENDANT", "1")
        .env(
            "PROBE_TREE_ADDRESS",
            std::env::var("PROBE_TREE_ADDRESS").unwrap(),
        )
        .env("PROBE_TREE_READY", &ready)
        .stdin(std::process::Stdio::null());
    for key in ["SYSTEMROOT", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if mode == "closed" {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    } else {
        command
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
    }
    let mut child = command.spawn().expect("spawn probe descendant");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !std::path::Path::new(&ready).exists() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("probe descendant did not become ready");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if mode == "hang" {
        let _ = child.wait();
        return true;
    }
    // Deliberately exit without waiting. The descendant retains the inherited
    // output handles, just like a shell wrapper that backgrounds a helper.
    false
}

fn leaked_sentinel() -> bool {
    std::env::vars().any(|(key, value)| {
        let upper = key.to_ascii_uppercase();
        let sensitive = upper.contains("KEY")
            || upper.contains("TOKEN")
            || upper.contains("SECRET")
            || upper.contains("CREDENTIAL");
        sensitive && value.contains("SENTINEL_")
    })
}

#[allow(clippy::too_many_lines)] // Explicit fake CLI command transcript.
fn main() {
    if leaked_sentinel() {
        println!("LEAKED");
        return;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    if probe_tree(&args) {
        return;
    }

    if std::env::var("FAKE_TREE_GRANDCHILD").is_ok() {
        let marker = std::env::var("FAKE_TREE_MARKER").unwrap_or_default();
        let _ = std::fs::write(&marker, std::process::id().to_string());
        std::thread::sleep(Duration::from_secs(60));
        return;
    }

    if std::env::var("FAKE_SPAWN_TREE").is_ok() {
        let marker = std::env::var("FAKE_TREE_MARKER").unwrap_or_default();
        let exe = std::env::current_exe().expect("current exe");
        let mut child = std::process::Command::new(exe);
        child
            .env_clear()
            .env("FAKE_TREE_GRANDCHILD", "1")
            .env("FAKE_TREE_MARKER", &marker)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(value) = std::env::var("SYSTEMROOT") {
            child.env("SYSTEMROOT", value);
        }
        if let Ok(value) = std::env::var("WINDIR") {
            child.env("WINDIR", value);
        }
        let _ = child.spawn();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }

    // Check timeout flag
    if std::env::var("FAKE_CLI_TIMEOUT").is_ok() {
        std::thread::sleep(Duration::from_secs(12));
        return;
    }

    // Check buffer overflow flag (> 64 KiB)
    if std::env::var("FAKE_CLI_OVERFLOW").is_ok() {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let payload = vec![b'A'; 70 * 1024];
        let _ = handle.write_all(&payload);
        let _ = handle.flush();
        return;
    }

    // Check failure flag
    if std::env::var("FAKE_CLI_FAIL").is_ok() {
        std::process::exit(1);
    }

    let joined = args.join(" ");

    match joined.as_str() {
        "--version" => {
            if std::env::var("FAKE_CLI_MULTILINE").is_ok() {
                println!("fake 1.2.3\nsecond line to ignore\nthird line");
            } else {
                println!("fake 1.2.3");
            }
        }
        "auth status --json" => {
            if std::env::var("FAKE_CLAUDE_EXTRA_FIELDS").is_ok() {
                println!(
                    r#"{{"loggedIn": true, "email": "alice@example.com", "accessToken": "SUPER_SECRET_TOKEN_DO_NOT_LEAK", "orgName": "Acme Corp"}}"#
                );
            } else if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!(r#"{{"loggedIn": false}}"#);
            } else {
                println!(r#"{{"loggedIn": true, "email": "claude@example.com"}}"#);
            }
        }
        "login status" => {
            if std::env::var("FAKE_CODEX_GARBAGE_STDERR").is_ok() {
                eprintln!("some garbage traceback without login status");
            } else if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!("Not logged in");
            } else {
                println!("Logged in using ChatGPT");
            }
        }
        "status --format json" => {
            if let Ok(json) = std::env::var("FAKE_CURSOR_JSON") {
                println!("{json}");
            } else if std::env::var("FAKE_CURSOR_PLAIN").is_ok() {
                eprintln!("flag --format not recognized");
                std::process::exit(1);
            } else if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!(r#"{{"status": "unauthenticated"}}"#);
            } else {
                println!(r#"{{"status": "authenticated"}}"#);
            }
        }
        "status" => {
            if let Ok(text) = std::env::var("FAKE_CURSOR_TEXT") {
                println!("{text}");
            } else if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!("not logged in");
            } else {
                println!("authenticated");
            }
            if let Ok(text) = std::env::var("FAKE_CURSOR_STDERR") {
                eprintln!("{text}");
            }
            if std::env::var("FAKE_CURSOR_FAIL").is_ok() {
                std::process::exit(1);
            }
        }
        _ => {
            println!("unknown command: {joined}");
        }
    }
}
