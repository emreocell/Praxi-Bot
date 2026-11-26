//! A stand-in interactive program for the `shell_interactive` tests: never a vendor binary,
//! never the network. `pty-fixture login` behaves like a CLI sign-in (prints a link and a
//! one-time code, a secret-looking debug line, waits for a pasted code); `pty-fixture ask`
//! asks one question; `pty-fixture echo` prints whatever lines it is given until `quit`.
use std::io::{BufRead, Write};

fn flush(text: &str) {
    print!("{text}");
    std::io::stdout().flush().unwrap();
}

fn read_line() -> String {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    line.trim().to_owned()
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "login" => {
            let key = format!("sk-{}", "abcDEF0123456789".repeat(2));
            flush("Open https://auth.example.com/device and enter AB12-CD34\n");
            flush(&format!("debug key {key}\n"));
            flush("Paste code: ");
            if read_line() == "GOODCODE" {
                flush("Signed in as ada@example.com\n");
            } else {
                flush("invalid code\n");
                std::process::exit(4);
            }
        }
        "ask" => {
            flush("Your name: ");
            let name = read_line();
            flush(&format!("hello {name}\n"));
            std::process::exit(3);
        }
        // Prints each line back and says where it runs; `where` shows the folder.
        "echo" => loop {
            flush("> ");
            let line = read_line();
            if line == "quit" {
                break;
            }
            if line == "where" {
                flush(&format!("{}\n", std::env::current_dir().unwrap().display()));
            } else {
                flush(&format!("got {line}\n"));
            }
        },
        // 2000 characters, a tab, more: the transcript's line limit meets a tab stop.
        "tab2000" => {
            flush(&format!("{}\tend\n", "x".repeat(2000)));
            flush("done> ");
            let _ = read_line();
        }
        // More lines than one answer carries, then the program ends.
        "flood" => {
            for n in 0..2000 {
                flush(&format!("line {n:04} ........................\n"));
            }
        }
        // A program that never reads its input and never ends: for the limits.
        "sleep" => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        // Prints one long line (a key or token) hard-wrapped at the width it is given, then waits.
        "wrapped" => {
            let width: usize = std::env::args()
                .nth(2)
                .and_then(|w| w.parse().ok())
                .unwrap_or(20);
            let secret = std::env::args().nth(3).unwrap_or_default();
            for chunk in secret.as_bytes().chunks(width) {
                flush(&format!("{}\n", String::from_utf8_lossy(chunk)));
            }
            flush("done> ");
            let _ = read_line();
        }
        _ => {
            eprintln!("unsupported fixture mode");
            std::process::exit(2);
        }
    }
}
