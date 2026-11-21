//! Keep the exit codes in `bot --help` in step with the CLI reference.

use std::collections::BTreeMap;
use std::process::Command;

const CLI_REFERENCE: &str = include_str!("../../../docs/guide/cli-reference.md");

/// The part of a meaning before any parenthesised detail, e.g.
/// `Authentication rejected (or missing API token)` -> `Authentication rejected`.
fn short_meaning(meaning: &str) -> &str {
    meaning.split(" (").next().unwrap_or(meaning).trim()
}

/// `code -> meaning` from the "Exit codes" table of the CLI reference.
fn documented_exit_codes() -> BTreeMap<u8, String> {
    let section = CLI_REFERENCE
        .split("## Exit codes")
        .nth(1)
        .expect("the CLI reference has an `## Exit codes` section");
    let section = section.split("\n## ").next().unwrap_or(section);
    section
        .lines()
        .filter_map(|line| {
            let mut cells = line.trim().trim_matches('|').split('|').map(str::trim);
            let code = cells.next()?.parse::<u8>().ok()?;
            let meaning = cells.next()?;
            Some((code, short_meaning(meaning).to_owned()))
        })
        .collect()
}

/// `code -> meaning` from the "Exit codes:" block at the end of `bot --help`.
fn help_exit_codes(help: &str) -> BTreeMap<u8, String> {
    let block = help
        .split("Exit codes:")
        .nth(1)
        .expect("`bot --help` has an `Exit codes:` block");
    block
        .lines()
        .filter_map(|line| {
            let (code, meaning) = line.trim().split_once(char::is_whitespace)?;
            Some((code.parse::<u8>().ok()?, short_meaning(meaning).to_owned()))
        })
        .collect()
}

#[test]
fn help_lists_the_documented_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("--help")
        .output()
        .expect("run `bot --help`");
    assert!(output.status.success(), "`bot --help` failed: {output:?}");
    let help = String::from_utf8(output.stdout).expect("`bot --help` prints UTF-8");

    let documented = documented_exit_codes();
    assert_eq!(
        documented.keys().copied().collect::<Vec<_>>(),
        (0..=7).collect::<Vec<_>>(),
        "unexpected codes in docs/guide/cli-reference.md"
    );
    assert_eq!(
        help_exit_codes(&help),
        documented,
        "the exit codes in `bot --help` and docs/guide/cli-reference.md disagree"
    );
}
