//! Inspect the English/Turkish schedule parser without creating a schedule or running a daemon.
//!
//! From the workspace root:
//!
//! ```sh
//! cargo run -p scheduler --example parse_schedule -- "every weekday at 8:15am" "in 30 minutes"
//! cargo run -p scheduler --example parse_schedule -- --tz Europe/Istanbul "her gün 09:00"
//! ```

use std::env;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "Usage: parse_schedule [--tz <IANA timezone>] <phrase> [<phrase> ...]";

fn usage_error(message: &str) -> ! {
    eprintln!("{message}");
    eprintln!("{USAGE}");
    process::exit(2);
}

fn main() {
    let mut timezone: Option<String> = None;
    let mut phrases = Vec::new();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--tz" => {
                let Some(name) = args.next() else {
                    usage_error("--tz requires an IANA timezone name");
                };
                if name.trim().is_empty() || name.starts_with('-') {
                    usage_error("--tz requires an IANA timezone name");
                }
                if timezone.replace(name).is_some() {
                    usage_error("--tz may be provided only once");
                }
            }
            _ if arg.starts_with('-') => usage_error(&format!("Unknown option: {arg}")),
            _ => phrases.push(arg),
        }
    }

    if phrases.is_empty() {
        usage_error("Provide at least one schedule phrase");
    }

    let now_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => match i64::try_from(duration.as_millis()) {
            Ok(ms) => ms,
            Err(_) => usage_error("System clock timestamp is out of range"),
        },
        Err(_) => usage_error("System clock is before the Unix epoch"),
    };

    for phrase in phrases {
        match scheduler::parse_natural_language_with_tz(&phrase, now_ms, timezone.as_deref()) {
            Some(spec) => println!("{phrase:?}: {spec:?}"),
            None => println!("{phrase:?}: no match"),
        }
    }
}
