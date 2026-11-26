//! Hermetic stand-in for the `claude` / `codex` login commands (and, on a terminal, for the
//! `kimi`, `qodercli`, `cursor-agent`, `kiro-cli`, `agy` and `opencode` ones). Never a vendor binary and never
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

/// "The owner approved it in the browser": the test drops an `approve` file into the home.
fn wait_for_approval() -> bool {
    for _ in 0..600 {
        if home().join("approve").exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
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

#[allow(clippy::too_many_lines)] // one flat table of fixture commands
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
        // ---- engines whose login only talks on a terminal (the host runs them on a PTY) ----
        ("kimi", ["login"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush(
                "\nOpening browser for Kimi device login: https://www.kimi.com/code/authorize_device?user_code=WXYZ-5678\n",
            );
            flush("If the browser did not open, paste the URL above and enter code: WXYZ-5678\n");
            flush("Waiting for authorization to complete…\n");
            if wait_for_approval() {
                let dir = home().join(".kimi-code").join("credentials");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("kimi-code.json"), "fixture only").unwrap();
                flush("Logged in to Kimi Code.\n");
                return;
            }
            std::process::exit(1);
        }
        ("qodercli", ["login"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush(
                "Starting browser login...\n  https://qoder.com/device/selectAccounts?nonce=n1&challenge=c1\n\n",
            );
            flush("Waiting for login in browser...\n");
            if wait_for_approval() {
                std::fs::write(home().join("authenticated"), "fixture only").unwrap();
                flush("\nLogin successful! Welcome, owner@example.test.\n");
                return;
            }
            std::process::exit(1);
        }
        ("qodercli", ["status", "-o", "json"]) => {
            println!("{{\"logged_in\":{signed_in}}}");
        }
        ("cursor-agent", ["login"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush(
                "Opening browser to log in...\nIf it does not open, visit:\n  https://cursor.com/loginDeepControl?challenge=abc123&uuid=u-1&mode=login\n",
            );
            if wait_for_approval() {
                std::fs::write(home().join("authenticated"), "fixture only").unwrap();
                flush("Login successful\n");
                return;
            }
            std::process::exit(1);
        }
        ("cursor-agent", ["status", "--format", "json"]) => {
            println!("{{\"isAuthenticated\":{signed_in}}}");
            std::process::exit(i32::from(!signed_in));
        }
        ("kiro-cli", ["login", "--license", "free", "--use-device-flow"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush("Confirm the following code in the browser\nCode: ABCD-1234\n");
            flush(
                "Open this URL: https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234\n",
            );
            if wait_for_approval() {
                flush("Logged in successfully\n");
                return;
            }
            std::process::exit(1);
        }
        // A TUI that keeps running after it signed in (so the host watches the token folder).
        ("agy", []) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush(
                "Welcome to Antigravity\nSign in to continue:\nhttps://accounts.google.com/o/oauth2/v2/auth?client_id=x&response_type=code&state=s1\n",
            );
            flush("Enter the authorization code: ");
            let mut typed = String::new();
            std::io::stdin().lock().read_line(&mut typed).unwrap();
            std::fs::write(home().join("stdin-code"), typed.trim()).unwrap();
            if typed.trim() == "4/0AGYCODE-1234" {
                let dir = home().join(".gemini").join("antigravity-cli");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("token"), "fixture only").unwrap();
                flush("\nSigned in. Starting the agent...\n");
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            std::process::exit(1);
        }
        // A login-method menu that Enter answers, then the console link and a code.
        ("opencode", ["auth", "login", "opencode"]) => {
            std::fs::write(home().join("login-pid"), std::process::id().to_string()).unwrap();
            flush(
                "T  Connect an integration\n|\n*  Select login method\n|  > OpenCode Console account\n|    API key (service account)\n",
            );
            let mut choice = String::new();
            std::io::stdin().lock().read_line(&mut choice).unwrap();
            std::fs::write(home().join("stdin-code"), format!("{:?}", choice.trim())).unwrap();
            flush(
                "\nOpen https://opencode.ai/auth/device in your browser\nEnter code: ABCD-5678\n",
            );
            if wait_for_approval() {
                let dir = home().join(".local").join("share").join("opencode");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("auth.json"), "fixture only").unwrap();
                flush("Logged in\n");
                return;
            }
            std::process::exit(1);
        }
        _ => {
            eprintln!("unsupported fixture command");
            std::process::exit(2);
        }
    }
}
