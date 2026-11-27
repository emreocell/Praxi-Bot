//! Strip what the file rules hide from git's output.
//!
//! `git diff` / `git show` print file contents and `git status` lists names: a repository can
//! track `.env`, an SSH key or a tool's settings, and `fs_read` refuses those. The tools run
//! git with `--no-renames` (so every header names one path), then drop the whole block of a
//! file the embedder protects. A header that cannot be read with certainty is dropped too:
//! the filter fails closed.

use std::fmt::Write as _;

/// The text left after the hidden entries are removed, and how many there were.
pub(crate) struct Filtered {
    pub text: String,
    pub hidden: usize,
}

impl Filtered {
    /// The text, with a one-line note when something was hidden.
    pub(crate) fn into_text(self) -> String {
        if self.hidden == 0 {
            return self.text;
        }
        let mut text = self.text;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        let _ = writeln!(
            text,
            "...[{} protected file(s) left out: credentials and tool settings are not shown]",
            self.hidden
        );
        text
    }
}

/// Drop the per-file blocks of a diff (`git diff`, `git show`) for protected paths. With `stat`
/// the `path | 3 ++-` lines of a diffstat are filtered as well.
pub(crate) fn filter_diff(text: &str, stat: bool, protected: &dyn Fn(&str) -> bool) -> Filtered {
    let mut out = String::with_capacity(text.len());
    let mut hidden = 0;
    let mut visible = true;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        if let Some(header) = header_paths(body) {
            visible = match header {
                Header::Paths(paths) => !paths.iter().any(|p| protected(p)),
                Header::Unreadable => false,
            };
            if !visible {
                hidden += 1;
            }
        } else if stat
            && let Some((name, _)) = body.rsplit_once(" | ")
            && stat_line(body)
        {
            let name = name.trim();
            let shown = unquote_or_plain(name);
            if shown.as_deref().is_none_or(protected) {
                hidden += 1;
                continue;
            }
        }
        if visible {
            out.push_str(line);
        }
    }
    Filtered { text: out, hidden }
}

/// Drop the entries of `git status --porcelain=v1 --branch` for protected paths. The branch
/// line stays.
pub(crate) fn filter_status(text: &str, protected: &dyn Fn(&str) -> bool) -> Filtered {
    let mut out = String::with_capacity(text.len());
    let mut hidden = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        if body.starts_with("##") || body.len() < 4 {
            out.push_str(line);
            continue;
        }
        let entry = &body[3..];
        // `R  old -> new`; a quoted name may itself hold the arrow, so split only unquoted.
        let names: Vec<&str> = if entry.starts_with('"') {
            vec![entry]
        } else {
            entry.split(" -> ").collect()
        };
        let hide = names.iter().any(|n| match unquote_or_plain(n) {
            Some(path) => protected(path.trim_end_matches('/')),
            None => true,
        });
        if hide {
            hidden += 1;
        } else {
            out.push_str(line);
        }
    }
    Filtered { text: out, hidden }
}

/// A diff header line.
enum Header {
    /// The paths it names.
    Paths(Vec<String>),
    /// A header whose paths cannot be read with certainty.
    Unreadable,
}

/// The header a line is, `None` for any other line.
fn header_paths(line: &str) -> Option<Header> {
    let readable = |paths: Option<Vec<String>>| paths.map_or(Header::Unreadable, Header::Paths);
    if let Some(rest) = line.strip_prefix("diff --git") {
        return Some(readable(rest.strip_prefix(' ').and_then(git_header)));
    }
    let rest = line
        .strip_prefix("diff --cc ")
        .or_else(|| line.strip_prefix("diff --combined "))?;
    Some(readable(unquote_or_plain(rest).map(|p| vec![p])))
}

/// `a/PATH b/PATH` (both spelled the same, as `--no-renames` guarantees), plain or C-quoted.
fn git_header(rest: &str) -> Option<Vec<String>> {
    if let Some(quoted) = rest.strip_prefix('"') {
        let (a, tail) = split_quoted(quoted)?;
        let tail = tail.strip_prefix(' ')?;
        let b = if let Some(q) = tail.strip_prefix('"') {
            let (b, after) = split_quoted(q)?;
            if !after.is_empty() {
                return None;
            }
            b
        } else {
            tail.to_owned()
        };
        return same_path(&a, &b);
    }
    // Unquoted: look for the split where both halves match (a name may hold " b/").
    rest.match_indices(" b/").find_map(|(at, _)| {
        let (a, b) = (&rest[..at], &rest[at + 1..]);
        same_path(a, b)
    })
}

fn same_path(a: &str, b: &str) -> Option<Vec<String>> {
    let (a, b) = (a.strip_prefix("a/")?, b.strip_prefix("b/")?);
    (a == b && !a.is_empty()).then(|| vec![a.to_owned()])
}

/// Text up to the closing quote, C-style unescaped, and what follows it.
fn split_quoted(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut raw = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8_lossy(&raw).into_owned(), &s[i + 1..])),
            b'\\' => {
                i += 1;
                let c = *bytes.get(i)?;
                match c {
                    b'a' => raw.push(7),
                    b'b' => raw.push(8),
                    b'f' => raw.push(12),
                    b'n' => raw.push(b'\n'),
                    b'r' => raw.push(b'\r'),
                    b't' => raw.push(b'\t'),
                    b'v' => raw.push(11),
                    b'"' | b'\\' => raw.push(c),
                    b'0'..=b'3' => {
                        let digits = bytes.get(i..i + 3)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let v = digits
                            .iter()
                            .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
                        raw.push(u8::try_from(v).ok()?);
                        i += 2;
                    }
                    _ => return None,
                }
                i += 1;
            }
            c => {
                raw.push(c);
                i += 1;
            }
        }
    }
    None
}

/// A quoted name unquoted, a plain name as it is; `None` when it does not parse.
fn unquote_or_plain(name: &str) -> Option<String> {
    match name.strip_prefix('"') {
        Some(q) => {
            let (text, after) = split_quoted(q)?;
            after.is_empty().then_some(text)
        }
        None => Some(name.to_owned()),
    }
}

/// A line of a diffstat: `name | 12 +++--` or `name | Bin 0 -> 12 bytes`.
fn stat_line(line: &str) -> bool {
    line.rsplit_once(" | ").is_some_and(|(_, rest)| {
        let rest = rest.trim_start();
        rest.starts_with("Bin") || rest.chars().next().is_some_and(|c| c.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_only(p: &str) -> bool {
        p == ".env" || p.ends_with("/.env") || p == ".ssh" || p.starts_with(".ssh/")
    }

    const DIFF: &str = "diff --git a/.env b/.env\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/.env\n@@ -0,0 +1 @@\n+TOKEN=hunter2\ndiff --git a/src/a.txt b/src/a.txt\nindex 1..2 100644\n--- a/src/a.txt\n+++ b/src/a.txt\n@@ -1 +1 @@\n-one\n+two\ndiff --git a/.ssh/id_rsa b/.ssh/id_rsa\nindex 1..2 100644\n--- a/.ssh/id_rsa\n+++ b/.ssh/id_rsa\n@@ -1 +1 @@\n-KEY\n+KEY2\n";

    #[test]
    fn protected_blocks_are_removed_and_the_rest_is_untouched() {
        let f = filter_diff(DIFF, false, &env_only);
        assert_eq!(f.hidden, 2);
        assert!(!f.text.contains("hunter2") && !f.text.contains("KEY"));
        assert!(f.text.contains("src/a.txt") && f.text.contains("+two"));
        let text = f.into_text();
        assert!(text.contains("2 protected file(s) left out"), "{text}");
    }

    #[test]
    fn nothing_hidden_means_the_text_is_returned_as_it_was() {
        let f = filter_diff(DIFF, false, &|_| false);
        assert_eq!(f.hidden, 0);
        assert_eq!(f.into_text(), DIFF);
    }

    #[test]
    fn a_commit_message_before_the_diff_survives() {
        let show = "commit abc\nAuthor: T <t@x>\n\n    add the env file\n\ndiff --git a/.env b/.env\n+++ b/.env\n+SECRET\n";
        let f = filter_diff(show, false, &env_only);
        assert!(f.text.contains("add the env file") && !f.text.contains("SECRET"));
    }

    #[test]
    fn a_header_that_cannot_be_read_is_dropped() {
        // truncated, mismatched halves, a rename-style header and a bad escape
        for header in [
            "diff --git a/.env b/.e",
            "diff --git a/one b/two",
            "diff --git",
            "diff --git \"a/x\\q\" \"b/x\\q\"",
            "diff --git a/ b/",
        ] {
            let text = format!("{header}\n+leak\n");
            let f = filter_diff(&text, false, &|_| false);
            assert_eq!(f.hidden, 1, "{header}");
            assert!(!f.text.contains("leak"), "{header}");
        }
    }

    #[test]
    fn quoted_and_spaced_names_are_matched() {
        let spaced = "diff --git a/my dir/.env b/my dir/.env\n+leak\n";
        assert_eq!(filter_diff(spaced, false, &env_only).hidden, 1);
        let quoted = "diff --git \"a/dir/.env\" \"b/dir/.env\"\n+leak\n";
        assert_eq!(filter_diff(quoted, false, &env_only).hidden, 1);
        let octal = "diff --git \"a/caf\\303\\251/.env\" \"b/caf\\303\\251/.env\"\n+leak\n";
        assert_eq!(filter_diff(octal, false, &env_only).hidden, 1);
        let combined = "diff --cc .env\n+leak\n";
        assert_eq!(filter_diff(combined, false, &env_only).hidden, 1);
    }

    #[test]
    fn a_diffstat_loses_the_protected_lines() {
        let stat = " .env       | 1 +\n src/a.txt | 2 +-\n 2 files changed, 2 insertions(+), 1 deletion(-)\n";
        let f = filter_diff(stat, true, &env_only);
        assert_eq!(f.hidden, 1);
        assert!(!f.text.contains(".env") && f.text.contains("src/a.txt"));
        assert!(f.text.contains("2 files changed"));
        // Without `stat` a line that merely looks like one is left alone.
        assert_eq!(filter_diff(stat, false, &env_only).hidden, 0);
    }

    #[test]
    fn status_entries_for_protected_paths_are_dropped_and_the_branch_stays() {
        let status = "## main...origin/main\n M src/a.txt\n?? .env\nR  old.txt -> .ssh/id_rsa\n?? .ssh/\nA  \"we ird/.env\"\n";
        let f = filter_status(status, &env_only);
        assert_eq!(f.hidden, 4);
        assert!(f.text.contains("## main...origin/main") && f.text.contains("src/a.txt"));
        assert!(!f.text.contains(".env") && !f.text.contains("id_rsa"));
    }
}
