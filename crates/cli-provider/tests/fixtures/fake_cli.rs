use std::io::Write;
use std::time::Duration;

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

fn main() {
    if leaked_sentinel() {
        println!("LEAKED");
        return;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

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
            if std::env::var("FAKE_CURSOR_PLAIN").is_ok() {
                eprintln!("flag --format not recognized");
                std::process::exit(1);
            } else if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!(r#"{{"status": "unauthenticated"}}"#);
            } else {
                println!(r#"{{"status": "authenticated"}}"#);
            }
        }
        "status" => {
            if std::env::var("FAKE_AUTH_SIGNED_OUT").is_ok() {
                println!("not logged in");
            } else {
                println!("authenticated");
            }
        }
        _ => {
            println!("unknown command: {joined}");
        }
    }
}
