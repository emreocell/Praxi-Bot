//! Hermetic stand-in for the `claude` / `codex` login commands. Never a vendor binary and never
//! the network. Which CLI it plays is the executable's file name; `FAKE_MODE` picks a scenario.
//! State lives under `$HOME` (the service user's home, as for the real CLIs).
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from(
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect("HOME"),
    )
}

fn flush(text: &str) {
    print!("{text}");
    std::io::stdout().flush().unwrap();
}

/// A child that outlives the leader unless the whole tree is killed.
fn spawn_grandchild() {
    let exe = std::env::current_exe().unwrap();
    #[allow(clippy::zombie_processes)] // deliberately outlives the leader: the test kills the tree
    let child = std::process::Command::new(exe)
        .arg("--sleep-forever")
        .spawn()
        .unwrap();
    std::fs::write(home().join("grandchild-pid"), child.id().to_string()).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = std::env::current_exe()
        .unwrap()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let mode = std::env::var("FAKE_MODE").unwrap_or_default();
    let signed_in = home().join("authenticated").exists();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match (name.as_str(), a.as_slice()) {
        (_, ["--sleep-forever"]) => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        (_, ["--version"]) => println!("9.9.9 (Fixture)"),
        ("claude", ["auth", "status", "--json"]) => {
            println!("{{\"loggedIn\":{signed_in},\"email\":\"owner@example.test\"}}");
            std::process::exit(i32::from(!signed_in));
        }
        ("codex", ["login", "status"]) => {
            if signed_in {
                println!("Logged in using ChatGPT");
            } else {
                println!("Not logged in");
                std::process::exit(1);
            }
        }
        ("claude", ["auth", "login"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            spawn_grandchild();
            if mode == "no-link" {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            flush("Opening browser to sign in...\n");
            let link = if mode == "evil-link" {
                "https://evil.example/oauth/authorize?code=true"
            } else {
                "https://claude.com/cai/oauth/authorize?code=true&client_id=fixture&state=abc"
            };
            flush(&format!("If the browser did not open, visit: {link}\n"));
            flush("Paste code here if prompted > ");
            let mut typed = String::new();
            std::io::stdin().lock().read_line(&mut typed).unwrap();
            let code = typed.trim();
            if typed.is_empty() {
                // EOF: the sign-in was cancelled and its stdin closed; nothing was typed.
                std::process::exit(1);
            }
            // Proof that the code arrived on stdin (and where it must never appear).
            std::fs::write(home().join("stdin-code"), code).unwrap();
            if code == "GOODCODE#abc" {
                std::fs::write(home().join("authenticated"), "fixture only").unwrap();
                println!("Login successful");
            } else {
                eprintln!("Invalid authorization code");
                std::process::exit(1);
            }
        }
        ("codex", ["login", "--device-auth"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            spawn_grandchild();
            flush("Welcome to Codex\n");
            flush("1. Open this link in your browser\n   https://auth.openai.com/codex/device\n");
            flush("2. Enter this one-time code\n   ABCD-1234\n");
            // Wait for the "owner approved it in the browser" marker.
            let approve = home().join("approve");
            for _ in 0..600 {
                if approve.exists() {
                    std::fs::write(home().join("authenticated"), "fixture only").unwrap();
                    println!("Successfully logged in");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            std::process::exit(1);
        }
        _ => {
            eprintln!("unsupported fixture command");
            std::process::exit(2);
        }
    }
}
