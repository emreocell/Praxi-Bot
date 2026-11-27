//! Approval memory: "allow this for this chat / this bot", scoped to a target.
//!
//! A remembered rule names a tool and ONE target (a site, a folder, a command),
//! never a whole tool. [`derive`] computes the target of a call; [`Scope::covers`]
//! is deliberately strict (exact origin, folder containment, exact command
//! prefix), so a rule given for one target can never cover another. A call whose
//! target cannot be derived reliably (a path with `..`, a shell command with an
//! operator, a quote, a path argument or a risky program) is [`Derived::Unscopable`]
//! and can only be approved once.

use serde_json::Value;

/// What a rule's pattern names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    /// A site: `scheme://host:port`.
    Origin,
    /// A folder; the rule covers files directly in it and below.
    Folder,
    /// A shell command prefix: the program and, if present, its sub-command.
    Command,
}

impl ScopeKind {
    /// Stable name (audit log, API, UI).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Folder => "folder",
            Self::Command => "command",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Origin, Self::Folder, Self::Command]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// One target a rule can name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// What the pattern names.
    pub kind: ScopeKind,
    /// Normalised target (see [`derive`]).
    pub pattern: String,
}

impl Scope {
    /// Whether a rule with this scope covers a call whose derived scope is `call`.
    #[must_use]
    pub fn covers(&self, call: &Scope) -> bool {
        if self.kind != call.kind {
            return false;
        }
        match self.kind {
            ScopeKind::Origin | ScopeKind::Command => self.pattern == call.pattern,
            ScopeKind::Folder => {
                if self.pattern == "." {
                    // The workspace: any relative folder.
                    return is_relative(&call.pattern);
                }
                call.pattern == self.pattern
                    || call
                        .pattern
                        .strip_prefix(&self.pattern)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        }
    }
}

/// The result of looking for a call's target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Derived {
    /// The call has this one target.
    Scope(Scope),
    /// The tool is one that is remembered per target, but this call's target
    /// cannot be named safely: it is approved once, never remembered.
    Unscopable,
    /// The tool is not remembered per target (the older tool-wide rules apply).
    NotScoped,
}

/// Tools remembered per target. Deletes, sends and payments are not in this list.
#[must_use]
pub fn is_scoped_family(tool: &str) -> bool {
    matches!(
        tool,
        "browser_open"
            | "fs_write"
            | "fs_edit"
            | "fs_move"
            | "shell_run"
            | "shell_interactive"
            // The document and git tools that write or change files are remembered per folder
            // like `fs_write`: "always" must not mean "everywhere".
            | "docx_write"
            | "docx_append"
            | "xlsx_write"
            | "xlsx_append_rows"
            | "xlsx_update_cells"
            | "csv_transform"
            | "doc_convert"
            | "doc_preview"
            | "git_commit"
            | "git_branch"
    )
}

/// The target of this call.
#[must_use]
pub fn derive(tool: &str, input: &Value) -> Derived {
    if !is_scoped_family(tool) {
        return Derived::NotScoped;
    }
    // A switch rewrites the whole work tree of the repository, whatever folder `repo` names,
    // so no folder describes what a remembered rule would allow: approved once, every time.
    if tool == "git_branch"
        && (input.get("action").and_then(Value::as_str) == Some("switch")
            || input.get("switch").and_then(Value::as_bool) == Some(true))
    {
        return Derived::Unscopable;
    }
    let scope = match tool {
        "browser_open" => input
            .get("url")
            .and_then(Value::as_str)
            .and_then(origin_of)
            .map(|pattern| Scope {
                kind: ScopeKind::Origin,
                pattern,
            }),
        "fs_move" => {
            let from = input
                .get("from")
                .and_then(Value::as_str)
                .and_then(folder_of);
            let to = input.get("to").and_then(Value::as_str).and_then(folder_of);
            // One rule names one folder: a move across folders is asked once.
            from.filter(|f| Some(f) == to.as_ref())
                .map(|pattern| Scope {
                    kind: ScopeKind::Folder,
                    pattern,
                })
        }
        "shell_run" => input
            .get("command")
            .and_then(Value::as_str)
            .and_then(command_prefix)
            .map(|pattern| Scope {
                kind: ScopeKind::Command,
                pattern,
            }),
        // A session is a conversation with a program: `start` names it, but `write` types
        // whatever the bot chooses afterwards, so no target describes what a rule would allow.
        // Every call is approved once.
        "shell_interactive" => None,
        // What these write is the output, not the file they read; left out, it lands next to
        // the source.
        "csv_transform" | "doc_convert" => input
            .get("output_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .and_then(folder_of)
            .map(|pattern| Scope {
                kind: ScopeKind::Folder,
                pattern,
            }),
        // The folder the pictures go in (the document's folder when none is named).
        "doc_preview" => match input.get("output_dir").and_then(Value::as_str) {
            Some(dir) => dir_of(dir),
            None => input
                .get("path")
                .and_then(Value::as_str)
                .and_then(folder_of),
        }
        .map(|pattern| Scope {
            kind: ScopeKind::Folder,
            pattern,
        }),
        // A repository is a folder; none named is the bot's own folder.
        "git_commit" | "git_branch" => match input.get("repo").and_then(Value::as_str) {
            Some(repo) => dir_of(repo),
            None => Some(".".to_owned()),
        }
        .map(|pattern| Scope {
            kind: ScopeKind::Folder,
            pattern,
        }),
        _ => input
            .get("path")
            .and_then(Value::as_str)
            .and_then(folder_of)
            .map(|pattern| Scope {
                kind: ScopeKind::Folder,
                pattern,
            }),
    };
    scope.map_or(Derived::Unscopable, Derived::Scope)
}

/// The site of a URL: `scheme://host:port`, lower case, no trailing dot, default
/// port made explicit. Only `http` / `https` with a host qualify.
#[must_use]
pub fn origin_of(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    Some(format!(
        "{}://{host}:{}",
        url.scheme(),
        url.port_or_known_default()?
    ))
}

fn is_relative(folder: &str) -> bool {
    let bytes = folder.as_bytes();
    !(folder.starts_with('/') || bytes.len() >= 2 && bytes[1] == b':')
}

/// `dir` itself as a folder pattern (the same normalisation as [`folder_of`]).
fn dir_of(dir: &str) -> Option<String> {
    let dir = dir.trim_end_matches(['/', '\\']);
    if dir.is_empty() {
        return None;
    }
    folder_of(&format!("{dir}/x"))
}

/// The folder a path lies in, lexically normalised (`/` separators, no `.`,
/// refused when it contains `..` or a NUL). A drive-letter path is lower-cased
/// (Windows paths are case-insensitive). Relative paths stay relative; a bare
/// file name lies in `.`.
fn folder_of(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('\0') || path.len() > 1024 {
        return None;
    }
    let path = path.replace('\\', "/");
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p => parts.push(p),
        }
    }
    parts.pop()?;
    let drive = parts
        .first()
        .is_some_and(|p| p.len() == 2 && p.as_bytes()[1] == b':');
    let mut folder = parts.join("/");
    if drive {
        folder = folder.to_ascii_lowercase();
    }
    Some(match (absolute, folder.is_empty()) {
        (_, true) if !absolute => ".".to_owned(),
        (_, true) => "/".to_owned(),
        (true, false) => format!("/{folder}"),
        (false, false) => folder,
    })
}

/// Programs that can run anything, reach the network, change privileges or
/// delete: never remembered as a command prefix.
const RISKY_PROGRAMS: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "rm",
    "rmdir",
    "del",
    "erase",
    "rd",
    "format",
    "mkfs",
    "dd",
    "shred",
    "chmod",
    "chown",
    "chgrp",
    "kill",
    "pkill",
    "killall",
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "ftp",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "bash",
    "sh",
    "zsh",
    "fish",
    "dash",
    "cmd",
    "powershell",
    "pwsh",
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "lua",
    "eval",
    "exec",
    "env",
    "xargs",
    "find",
    "awk",
    "sed",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "pipx",
    "docker",
    "podman",
    "kubectl",
    "reg",
    "regedit",
    "schtasks",
    "sc",
    "net",
    "crontab",
    "systemctl",
    "service",
    "mount",
    "umount",
    "iptables",
    "ufw",
    "passwd",
    "useradd",
    "userdel",
];

/// The program and optional sub-command of a *plain* command: only letters,
/// digits and ` -_./:=@,+%` (no operators, quotes, variables, globs, `~`), a bare
/// program name, not on the risky list, and no argument that is neither a flag
/// nor the one sub-command word. `git status -s` is `git status`; `ls` is `ls`;
/// `cat notes.txt` is not remembered.
fn command_prefix(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty()
        || command.len() > 400
        || !command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " -_./:=@,+%".contains(c))
    {
        return None;
    }
    let mut tokens = command.split_whitespace();
    let program = tokens.next()?.to_ascii_lowercase();
    if !program
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || RISKY_PROGRAMS.contains(&program.as_str())
    {
        return None;
    }
    let word = |t: &str| {
        t.starts_with(|c: char| c.is_ascii_lowercase())
            && t.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    let mut sub: Option<String> = None;
    for (i, t) in tokens.enumerate() {
        if t.starts_with('-') {
            continue;
        }
        if i == 0 && word(t) {
            sub = Some(t.to_owned());
            continue;
        }
        return None;
    }
    Some(match sub {
        Some(s) => format!("{program} {s}"),
        None => program,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope(tool: &str, input: &Value) -> Scope {
        match derive(tool, input) {
            Derived::Scope(s) => s,
            other => panic!("{tool} {input}: {other:?}"),
        }
    }

    #[test]
    fn a_site_rule_covers_that_origin_and_nothing_else() {
        let rule = scope("browser_open", &json!({"url": "https://Docs.rs./crate/x"}));
        assert_eq!(rule.pattern, "https://docs.rs:443");
        assert!(rule.covers(&scope(
            "browser_open",
            &json!({"url": "https://docs.rs/other"})
        )));
        for other in [
            "https://evil.docs.rs/",
            "http://docs.rs/",
            "https://docs.rs:8443/",
            "https://docs.rs.evil.test/",
        ] {
            assert!(
                !rule.covers(&scope("browser_open", &json!({ "url": other }))),
                "{other}"
            );
        }
        assert_eq!(
            derive("browser_open", &json!({"url": "file:///etc/passwd"})),
            Derived::Unscopable
        );
    }

    #[test]
    fn a_folder_rule_covers_its_subfolders_but_not_siblings_or_parents() {
        let rule = scope("fs_write", &json!({"path": "C:\\Work\\Proj\\src\\a.rs"}));
        assert_eq!(rule.pattern, "c:/work/proj/src");
        let call = |p: &str| scope("fs_edit", &json!({ "path": p }));
        assert!(rule.covers(&call("C:/Work/Proj/src/deep/b.rs")));
        assert!(rule.covers(&call("c:/work/proj/src/c.rs")));
        assert!(!rule.covers(&call("C:/Work/Proj/srcx/c.rs")));
        assert!(!rule.covers(&call("C:/Work/Proj/c.rs")));
        assert!(!rule.covers(&call("C:/Other/src/c.rs")));
        // A path that escapes lexically is never scoped.
        assert_eq!(
            derive("fs_write", &json!({"path": "C:/Work/Proj/src/../../x"})),
            Derived::Unscopable
        );
        // The workspace rule covers relative folders only.
        let ws = scope("fs_write", &json!({"path": "a.txt"}));
        assert_eq!(ws.pattern, ".");
        assert!(ws.covers(&call("notes/x.md")));
        assert!(!ws.covers(&call("/etc/x")));
        assert!(!ws.covers(&call("C:/Work/x")));
    }

    #[test]
    fn document_and_git_writers_are_remembered_per_folder_not_everywhere() {
        for tool in [
            "docx_write",
            "docx_append",
            "xlsx_write",
            "xlsx_append_rows",
            "xlsx_update_cells",
            "csv_transform",
            "doc_convert",
            "doc_preview",
            "git_commit",
            "git_branch",
        ] {
            assert!(is_scoped_family(tool), "{tool}");
        }
        // The read tools stay as they were.
        for tool in [
            "docx_read",
            "xlsx_read",
            "git_status",
            "git_diff",
            "process_list",
        ] {
            assert!(!is_scoped_family(tool), "{tool}");
        }
        let rule = scope("docx_append", &json!({"path": "C:/Work/Proj/a.docx"}));
        assert_eq!(rule.pattern, "c:/work/proj");
        assert!(!rule.covers(&scope("docx_append", &json!({"path": "C:/Other/a.docx"}))));
        // The folder written is the output's, not the source's; left out, the source's.
        let out = scope(
            "doc_convert",
            &json!({"path": "C:/In/a.docx", "to": "pdf", "output_path": "C:/Out/a.pdf"}),
        );
        assert_eq!(out.pattern, "c:/out");
        let dflt = scope("doc_convert", &json!({"path": "C:/In/a.docx", "to": "pdf"}));
        assert_eq!(dflt.pattern, "c:/in");
        assert_eq!(
            scope(
                "doc_preview",
                &json!({"path": "C:/In/a.docx", "output_dir": "C:/Pics"})
            )
            .pattern,
            "c:/pics"
        );
        assert_eq!(
            scope("doc_preview", &json!({"path": "C:/In/a.docx"})).pattern,
            "c:/in"
        );
        // A repository is a folder; none named is the bot's own folder.
        let repo = scope(
            "git_commit",
            &json!({"repo": "C:/Work/App", "paths": ["a"], "message": "m"}),
        );
        assert_eq!(repo.pattern, "c:/work/app");
        assert!(repo.covers(&scope(
            "git_branch",
            &json!({"repo": "C:/Work/App/sub", "action": "create", "name": "x"})
        )));
        // A switch rewrites the whole work tree, so it is never remembered for a folder.
        for input in [
            json!({"repo": "C:/Work/App", "action": "switch", "name": "x"}),
            json!({"repo": "C:/Work/App", "action": "create", "name": "x", "switch": true}),
        ] {
            assert_eq!(derive("git_branch", &input), Derived::Unscopable, "{input}");
        }
        assert!(!repo.covers(&scope(
            "git_commit",
            &json!({"repo": "C:/Work/Other", "paths": ["a"], "message": "m"})
        )));
        assert_eq!(
            scope("git_commit", &json!({"paths": ["a"], "message": "m"})).pattern,
            "."
        );
        // A path that cannot be named safely is approved once.
        assert_eq!(
            derive(
                "git_commit",
                &json!({"repo": "C:/Work/../x", "paths": ["a"], "message": "m"})
            ),
            Derived::Unscopable
        );
    }

    #[test]
    fn a_move_is_scoped_only_within_one_folder() {
        assert!(matches!(
            derive("fs_move", &json!({"from": "a/x", "to": "a/y"})),
            Derived::Scope(_)
        ));
        assert_eq!(
            derive("fs_move", &json!({"from": "a/x", "to": "b/x"})),
            Derived::Unscopable
        );
    }

    #[test]
    fn a_command_rule_is_the_exact_program_and_subcommand() {
        let rule = scope("shell_run", &json!({"command": "git status -s"}));
        assert_eq!(rule.pattern, "git status");
        assert!(rule.covers(&scope(
            "shell_run",
            &json!({"command": "git status --branch"})
        )));
        assert!(!rule.covers(&scope("shell_run", &json!({"command": "git push"}))));
        assert!(!rule.covers(&scope("shell_run", &json!({"command": "git"}))));
        let ls = scope("shell_run", &json!({"command": "ls -la"}));
        assert_eq!(ls.pattern, "ls");
        assert!(!ls.covers(&scope("shell_run", &json!({"command": "cargo build"}))));
    }

    #[test]
    fn risky_or_compound_commands_are_never_remembered() {
        for c in [
            "git status; rm -rf x",
            "ls | sh",
            "echo $(whoami)",
            "cat notes.txt",
            "rm foo",
            "python tool.py",
            "./run.sh",
            "curl http://x",
            "echo \"hi\"",
            "ls > out",
            "",
        ] {
            assert_eq!(
                derive("shell_run", &json!({ "command": c })),
                Derived::Unscopable,
                "{c:?}"
            );
        }
    }

    #[test]
    fn an_interactive_session_is_never_remembered() {
        // `start` names a program, but what `write` types afterwards is the bot's choice: no
        // rule can describe it, so every call is approved once.
        for input in [
            json!({"action": "start", "command": "vibe --setup"}),
            json!({"action": "write", "session": "pty-1", "input": "rm -rf /", "enter": true}),
            json!({"action": "read", "session": "pty-1"}),
            json!({}),
        ] {
            assert!(is_scoped_family("shell_interactive"));
            assert_eq!(
                derive("shell_interactive", &input),
                Derived::Unscopable,
                "{input}"
            );
        }
    }

    #[test]
    fn other_tools_are_not_scoped() {
        assert_eq!(derive("email_send", &json!({})), Derived::NotScoped);
        assert_eq!(
            derive("fs_delete", &json!({"path": "a"})),
            Derived::NotScoped
        );
    }
}
