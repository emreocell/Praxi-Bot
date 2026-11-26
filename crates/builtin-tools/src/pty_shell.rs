//! `shell_interactive` (permission category `Shell`): a program on a pseudo-terminal that the
//! bot can type into.
//!
//! `shell_run` runs one command to the end and never forwards stdin, so it cannot answer an
//! installer's question, a `claude auth login` code prompt or a `vibe --setup` wizard. This tool
//! starts the program on a PTY (see the `pty-session` crate) and then lets the bot send keys,
//! read what the program printed (with a timeout, so it never hangs silently), look at the
//! screen, resize and kill it. Every answer names the state: `running`, `waiting_for_input`,
//! `finished` or `failed`.
//!
//! Policy is at least that of `shell_run`:
//!
//! * NO SHELL runs the command. `start` takes a program and its arguments, split by a strict
//!   tokenizer that understands words and quotes and nothing else (a pipe, a redirect, `$`, a
//!   backtick, a glob or `;` is an error), and the program is spawned directly with that argv.
//!   What is checked is exactly what runs: there is no second parser that could read the line
//!   differently from the one the shell would;
//! * the program is looked up on the child's `PATH`, resolved to its canonical file (symlinks
//!   followed) and refused by the NAME of what it really is: a shell, an interpreter, a launcher
//!   of other programs, a pager or editor with a shell escape, a terminal multiplexer. Typing into
//!   one of those would run arbitrary commands around the command check. Use `shell_run`;
//! * the permission layer sees the command (`raw_command_for`) before `start`, and the typed text
//!   before `write`. Neither sees a command typed in pieces, nor the folder it runs in, so the
//!   tool also judges the whole LOGICAL line typed into each session since its last Enter (a
//!   backspace edits it) against the never-list, from the session's own folder. A line the
//!   never-list flags is refused: approval cannot lift it here, `shell_run` is where it is asked;
//! * the environment is the same allowlist (no vault, bearer or provider credential);
//! * the working directory defaults to the bot's workspace;
//! * what the command fetches (`git clone`, `curl -o`...) is recorded in the provenance ledger.
//!
//! This is damping, not a boundary, like the never-list itself: a program the owner lets a bot
//! start can do what that program does.
//!
//! Bounds: 4 sessions per bot and 16 in all, 8 KiB of text per read, 4 KiB per write, 10 minutes
//! idle, 1 hour in all; a background reaper enforces the time limits even when nobody calls the
//! tool, and dropping the tool ends every session. Output is masked with the daemon's redactor
//! (the vault's values) and the production patterns, also across line breaks, before the bot sees
//! it; typed text and output are never logged.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use permission_engine::NeverList;
use pty_session::{PtySession, Snapshot, SpawnSpec, State};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::provenance::Provenance;
use crate::shell::child_env;
use crate::util::{exec_err, parse, spec};

/// Sessions one bot may have open at once.
pub const MAX_SESSIONS_PER_BOT: usize = 4;
/// Sessions open at once, all bots together.
pub const MAX_SESSIONS_TOTAL: usize = 16;

/// How long sessions may live.
#[derive(Clone, Copy)]
struct Limits {
    /// No call for this long: the session is killed.
    idle: Duration,
    /// How long an ended session stays readable (its last output) before its slot is freed.
    ended_keep: Duration,
    /// No session lives longer than this.
    hard: Duration,
    /// How often the background reaper looks at the sessions.
    reap_every: Duration,
}

const LIMITS: Limits = Limits {
    idle: Duration::from_secs(10 * 60),
    ended_keep: Duration::from_secs(2 * 60),
    hard: Duration::from_secs(60 * 60),
    reap_every: Duration::from_secs(15),
};

/// Most text one answer carries.
const MAX_READ_BYTES: usize = 8 * 1024;
/// Output must be quiet this long before a read returns early.
const SETTLE: Duration = Duration::from_millis(400);
const DEFAULT_WAIT_MS: u64 = 3_000;
const MAX_WAIT_MS: u64 = 60_000;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;
/// Longest command line `start` takes, and most words in it.
const MAX_COMMAND_CHARS: usize = 4096;
const MAX_ARGS: usize = 64;
/// Longest logical line that can be typed into one session before Enter.
const MAX_TYPED_LINE: usize = 16 * 1024;

/// Masks the secrets the daemon knows (the vault's values included) in text the bot is about to
/// see. The daemon passes its shared redactor here.
pub type OutputRedactor = Arc<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Start,
    Read,
    Write,
    Screen,
    Resize,
    Kill,
    List,
}

#[derive(Deserialize)]
struct Input {
    action: Action,
    /// `start`: the program and its arguments (no shell).
    command: Option<String>,
    /// Refused: there is no shell to choose.
    shell: Option<Value>,
    cwd: Option<String>,
    /// The session to act on.
    session: Option<String>,
    /// `write`: text to type.
    #[serde(rename = "input")]
    text: Option<String>,
    /// `write`: press Enter after the text.
    enter: Option<bool>,
    /// `write`: one named key instead of text.
    key: Option<String>,
    /// How long `start`, `read` and `write` wait for output.
    wait_ms: Option<u64>,
    cols: Option<u16>,
    rows: Option<u16>,
}

/// What `start` fetched, for the provenance ledger: recorded before the program runs and again
/// when the session ends (a program killed half way may have written part of it).
struct Fetched {
    ledger: Option<Arc<dyn Provenance>>,
    targets: Vec<(PathBuf, bool)>,
}

impl Fetched {
    fn record(&self) {
        if let Some(ledger) = &self.ledger {
            for (path, tree) in &self.targets {
                crate::provenance::mark_path(
                    ledger.as_ref(),
                    path,
                    *tree,
                    "fetched by a shell command",
                );
            }
        }
    }
}

impl Drop for Fetched {
    fn drop(&mut self) {
        self.record();
    }
}

struct Entry {
    session: Arc<PtySession>,
    owner: Option<String>,
    started: Instant,
    last_used: Instant,
    /// The command as shown to the bot (masked).
    command: String,
    /// The folder the program runs in: where a relative path in what is typed points.
    cwd: PathBuf,
    /// What has been typed since the last Enter: the logical line the program holds.
    line: String,
    _fetched: Fetched,
}

/// The sessions, shared with the background reaper.
struct Sessions {
    map: Mutex<HashMap<String, Entry>>,
    /// A reaper thread is running.
    reaper: AtomicBool,
    limits: Limits,
}

impl Sessions {
    fn new(limits: Limits) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            reaper: AtomicBool::new(false),
            limits,
        }
    }

    /// End sessions that are idle, too old, or finished long ago. Kills them.
    fn sweep(&self) {
        let now = Instant::now();
        let dead: Vec<Entry> = {
            let mut map = self.map.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, e)| {
                    now.duration_since(e.last_used) > self.limits.idle
                        || now.duration_since(e.started) > self.limits.hard
                        // An ended session the bot never read again frees its slot.
                        || (e.session.exit().is_some()
                            && now.duration_since(e.last_used) > self.limits.ended_keep)
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for entry in dead {
            entry.session.kill();
        }
    }
}

impl Drop for Sessions {
    /// The tool is gone (the daemon is shutting down): nothing it started stays running.
    fn drop(&mut self) {
        for (_, entry) in self.map.get_mut().drain() {
            entry.session.kill();
        }
    }
}

/// Look at the sessions every `reap_every` while there are any, so that the idle and hour
/// limits hold even when the bot (or nobody) calls the tool again. Ends when the tool is dropped
/// or the last session is gone; the next `start` brings it back.
fn start_reaper(sessions: &Arc<Sessions>) {
    if sessions.reaper.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak: Weak<Sessions> = Arc::downgrade(sessions);
    let started = std::thread::Builder::new()
        .name("pty-reaper".into())
        .spawn(move || {
            loop {
                let Some(sessions) = weak.upgrade() else {
                    return;
                };
                let pause = sessions.limits.reap_every;
                // Not held while asleep: the thread must not keep the sessions alive.
                drop(sessions);
                std::thread::sleep(pause);
                let Some(sessions) = weak.upgrade() else {
                    return;
                };
                sessions.sweep();
                let map = sessions.map.lock();
                if map.is_empty() {
                    // Decided under the lock `start` inserts under: a session started from here
                    // on finds the flag cleared and starts the next reaper.
                    sessions.reaper.store(false, Ordering::SeqCst);
                    return;
                }
            }
        });
    if started.is_err() {
        sessions.reaper.store(false, Ordering::SeqCst);
    }
}

/// The `shell_interactive` tool.
pub struct PtyShellTool {
    spec: ToolSpec,
    sessions: Arc<Sessions>,
    access: Option<Arc<dyn crate::FsAccessProvider>>,
    provenance: Option<Arc<dyn Provenance>>,
    redactor: Option<OutputRedactor>,
    never: NeverList,
}

/// Build the tool. `access` makes `cwd` default to the calling bot's workspace; `provenance`
/// records what a command fetches; `redactor` is the daemon's, with the vault's values.
#[must_use]
pub fn tool(
    access: Option<Arc<dyn crate::FsAccessProvider>>,
    provenance: Option<Arc<dyn Provenance>>,
    redactor: Option<OutputRedactor>,
) -> Arc<dyn Tool> {
    Arc::new(PtyShellTool::new(access, provenance, redactor))
}

fn pattern_redactor() -> &'static secret_store::Redactor {
    static REDACTOR: OnceLock<secret_store::Redactor> = OnceLock::new();
    REDACTOR.get_or_init(secret_store::Redactor::new)
}

// ---------------------------------------------------------------------------------------------
// The command line: words, quotes, nothing else.

/// Split `command` into the program and its arguments the way a person reads it: words
/// separated by blanks, `'...'` and `"..."` grouping (nothing inside is special, no escape),
/// adjacent pieces joining into one word. Everything a shell would give a meaning to is an
/// error instead, because no shell runs this and a silent literal would not be what was meant.
fn split_command(command: &str) -> Result<Vec<String>, String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        if c == '\0' || (c.is_control() && !c.is_whitespace()) || c == '\n' || c == '\r' {
            return Err(
                "the command has a line break or control character: one program per call".into(),
            );
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                started = true;
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '$' | '*' | '?' | '[' | ']' | '{'
            | '}' | '!' => {
                return Err(format!(
                    "`{c}` is shell syntax: this tool starts one program directly (no pipes, redirects, variables, \
                     globs or `&&`). Use shell_run for a command line, or quote the character to pass it as it is"
                ));
            }
            '\\' if !cfg!(windows) => {
                return Err(
                    "a backslash is shell syntax: this tool starts one program directly. Quote the word to pass it as it is"
                        .into(),
                );
            }
            '~' | '#' if !started => {
                return Err(format!(
                    "`{c}` at the start of a word is shell syntax: this tool starts one program directly. \
                     Write the full path, or quote the word"
                ));
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("a quote is not closed".into());
    }
    if started {
        words.push(word);
    }
    if words.is_empty() {
        return Err("the command is empty".into());
    }
    if words.len() > MAX_ARGS {
        return Err(format!("the command has more than {MAX_ARGS} words"));
    }
    Ok(words)
}

/// The words as one line for the policy checks, quoted where a word has blanks or quotes so the
/// never-list reads the same words the program gets.
fn join_for_policy(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.is_empty() || w.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
                format!("\"{}\"", w.replace('"', "'"))
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------------
// Which programs may not be started here.

/// Programs whose own prompt runs whatever is typed into it, or that start another program for
/// it, or whose pager or editor has a shell escape. Typing into them would walk around the
/// per-command check. Compared with the lower-case NAME of what the program really is, after
/// `.exe` and a version number (`python3.12`, `bash-5.2`) are taken off.
const REFUSED_PROGRAMS: &[&str] = &[
    // shells
    "sh",
    "bash",
    "rbash",
    "zsh",
    "fish",
    "dash",
    "ash",
    "ksh",
    "ksh93",
    "mksh",
    "pdksh",
    "csh",
    "tcsh",
    "rc",
    "es",
    "elvish",
    "nu",
    "nushell",
    "xonsh",
    "busybox",
    "toybox",
    "powershell",
    "powershell_ise",
    "pwsh",
    "cmd",
    "command",
    "conhost",
    "wsl",
    "wslhost",
    "login",
    // interpreters and REPLs
    "python",
    "python2",
    "pythonw",
    "pypy",
    "pypy3",
    "py",
    "ipython",
    "bpython",
    "ptpython",
    "node",
    "nodejs",
    "deno",
    "bun",
    "tsx",
    "ts-node",
    "perl",
    "ruby",
    "irb",
    "pry",
    "php",
    "lua",
    "luajit",
    "tclsh",
    "wish",
    "expect",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "dc",
    "rscript",
    "r",
    "julia",
    "groovy",
    "jshell",
    "scala",
    "ghci",
    "ocaml",
    "utop",
    "guile",
    "racket",
    "csi",
    "iex",
    "erl",
    "swipl",
    "sbcl",
    "clisp",
    "gdb",
    "lldb",
    "sqlite3",
    "psql",
    "mysql",
    "mariadb",
    "mongosh",
    "osascript",
    "cscript",
    "wscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "cmstp",
    // launchers of other programs
    "sudo",
    "su",
    "doas",
    "pkexec",
    "run0",
    "runuser",
    "runas",
    "newgrp",
    "sg",
    "env",
    "exec",
    "eval",
    "xargs",
    "nohup",
    "setsid",
    "timeout",
    "nice",
    "ionice",
    "renice",
    "chrt",
    "taskset",
    "stdbuf",
    "flock",
    "rlwrap",
    "watch",
    "time",
    "parallel",
    "find",
    "start",
    "strace",
    "ltrace",
    "script",
    "scriptreplay",
    "nsenter",
    "unshare",
    "chroot",
    "setpriv",
    "capsh",
    "fakeroot",
    "proot",
    "bwrap",
    "firejail",
    "systemd-run",
    "systemd-nspawn",
    "machinectl",
    "docker",
    "podman",
    "nerdctl",
    "kubectl",
    "crictl",
    "ctr",
    "lxc",
    "ssh",
    "sshpass",
    "mosh",
    "scp",
    "sftp",
    "telnet",
    "nc",
    "ncat",
    "netcat",
    "socat",
    // terminal multiplexers and emulators
    "screen",
    "tmux",
    "byobu",
    "dtach",
    "abduco",
    "xterm",
    "gnome-terminal",
    "konsole",
    "x-terminal-emulator",
    "alacritty",
    "kitty",
    "wezterm",
    "tilix",
    "terminator",
    // pagers, editors, mail and file managers with a shell escape
    "less",
    "more",
    "most",
    "pg",
    "man",
    "info",
    "vi",
    "vim",
    "nvim",
    "view",
    "ex",
    "ed",
    "nano",
    "pico",
    "emacs",
    "micro",
    "joe",
    "mcedit",
    "code",
    "git",
    "ftp",
    "lftp",
    "mail",
    "mailx",
    "mutt",
    "neomutt",
    "alpine",
    "pine",
    "ranger",
    "mc",
    "nnn",
    "lf",
    "vifm",
];

/// `Some(name)` when `file_name` is one of [`REFUSED_PROGRAMS`]: lower-cased, a Windows
/// extension and a trailing version (`3.12`, `-5.2`, `22`) taken off.
fn refused_name(file_name: &str) -> Option<&'static str> {
    let lower = file_name.to_ascii_lowercase();
    let mut name = lower.as_str();
    for ext in [".exe", ".com", ".bat", ".cmd", ".ps1"] {
        if let Some(stem) = name.strip_suffix(ext) {
            name = stem;
            break;
        }
    }
    let bare = name.trim_end_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    REFUSED_PROGRAMS
        .iter()
        .copied()
        .find(|p| *p == name || (!bare.is_empty() && *p == bare))
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn refusal(name: &str, listed: &str) -> String {
    format!(
        "`{name}` is refused here ({listed}: a shell, an interpreter, a launcher of other programs, or a pager or \
         editor with a shell escape): typing into it would run commands around the command check. \
         Run the real program, or use shell_run for scripts"
    )
}

/// The program to run, found and checked.
#[derive(Debug)]
struct Plan {
    /// The file to start (as found on `PATH`, or the path given).
    program: PathBuf,
    args: Vec<OsString>,
    /// The words as one line, for the never-list and the provenance ledger.
    line: String,
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Where `name` is: a path (relative to `cwd`) when it has a separator, else the first match
/// on the absolute directories of `path_var`. Windows also tries `.exe` and `.com`.
fn find_program(name: &str, cwd: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let with_ext = |base: PathBuf| -> Vec<PathBuf> {
        if cfg!(windows) && base.extension().is_none() {
            vec![base.with_extension("exe"), base.with_extension("com")]
        } else {
            vec![base]
        }
    };
    let candidates: Vec<PathBuf> = if name.contains('/') || (cfg!(windows) && name.contains('\\')) {
        let p = PathBuf::from(name);
        with_ext(if p.is_absolute() { p } else { cwd.join(p) })
    } else {
        // A relative entry (an empty one means "here") is never searched.
        std::env::split_paths(path_var.unwrap_or_default())
            .filter(|d| d.is_absolute())
            .flat_map(|d| with_ext(d.join(name)))
            .collect()
    };
    candidates.into_iter().find(|c| is_executable_file(c))
}

/// Split `command`, find the program, and refuse it by what it really is. Nothing runs.
fn plan(command: &str, cwd: &Path, path_var: Option<&OsStr>) -> Result<Plan, String> {
    let words = split_command(command)?;
    let typed = words[0].as_str();
    let typed_name = file_name_of(Path::new(typed));
    if typed.contains('=') && !typed.contains('/') {
        return Err("a VAR=value assignment cannot come first: name the program".into());
    }
    // By the name as typed first, so the reason is the same whether or not it is installed.
    if let Some(listed) = refused_name(&typed_name) {
        return Err(refusal(&typed_name, listed));
    }
    let program = find_program(typed, cwd, path_var)
        .ok_or_else(|| format!("`{typed}` was not found (it must be installed and on the PATH)"))?;
    let canonical = std::fs::canonicalize(&program)
        .map_err(|e| format!("`{typed}` could not be resolved: {e}"))?;
    // And by what it really is: a link called `harmless` to `/bin/bash`, a `python3.12` that
    // `python3` points to, `sh` that is `busybox`.
    for path in [&program, &canonical] {
        let name = file_name_of(path);
        if let Some(listed) = refused_name(&name) {
            return Err(refusal(&format!("{typed} ({name})"), listed));
        }
    }
    // On Windows a batch file is run by `cmd.exe`, with its argument parsing.
    if matches!(
        canonical
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref(),
        Some("bat" | "cmd")
    ) {
        return Err(refusal(typed, "a batch file is run by cmd.exe"));
    }
    Ok(Plan {
        program,
        args: words[1..].iter().map(OsString::from).collect(),
        line: join_for_policy(&words),
    })
}

// ---------------------------------------------------------------------------------------------
// What is typed.

/// The logical line the program holds, kept per session, so a command typed in pieces is judged
/// as the whole it becomes.
struct TypedLine;

impl TypedLine {
    /// The line after `text` is typed onto `line`, or why it may not be. Judges every line the
    /// text completes and the one it leaves open; `line` is changed only when all pass.
    fn type_text(
        line: &mut String,
        text: &str,
        cwd: &Path,
        never: &NeverList,
    ) -> Result<(), String> {
        let mut now = line.clone();
        for c in text.chars() {
            if c == '\n' {
                Self::judge(&now, cwd, never)?;
                now.clear();
            } else {
                now.push(c);
                if now.len() > MAX_TYPED_LINE {
                    return Err(format!(
                        "a line of more than {MAX_TYPED_LINE} characters cannot be typed: press Enter first"
                    ));
                }
            }
        }
        Self::judge(&now, cwd, never)?;
        *line = now;
        Ok(())
    }

    /// The line after a named key, or why it may not be pressed. Space and backspace edit the
    /// line (and the result is judged), Enter and Ctrl-C end it; the other keys leave it.
    fn press(line: &mut String, key: &str, cwd: &Path, never: &NeverList) -> Result<(), String> {
        match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => {
                Self::judge(line, cwd, never)?;
                line.clear();
            }
            "ctrl_c" => line.clear(),
            "space" => {
                let mut now = line.clone();
                now.push(' ');
                Self::judge(&now, cwd, never)?;
                *line = now;
            }
            "backspace" => {
                let mut now = line.clone();
                now.pop();
                Self::judge(&now, cwd, never)?;
                *line = now;
            }
            _ => {}
        }
        Ok(())
    }

    fn judge(line: &str, cwd: &Path, never: &NeverList) -> Result<(), String> {
        if line.trim().is_empty() || !never.matches_in(line, cwd.to_str()) {
            return Ok(());
        }
        // The text is not repeated: it may be a password or a code.
        Err(
            "what is typed on this line matches the never-list (a catastrophic command, a Praxi \
             credential or a coding CLI's sign-in store) and is not sent. An interactive session \
             cannot ask for approval of it: use shell_run, which asks"
                .into(),
        )
    }
}

fn key_bytes(key: &str) -> Option<&'static str> {
    Some(match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => "\r",
        "tab" => "\t",
        "esc" | "escape" => "\x1b",
        "space" => " ",
        "up" => "\x1b[A",
        "down" => "\x1b[B",
        "right" => "\x1b[C",
        "left" => "\x1b[D",
        "backspace" => "\x7f",
        "ctrl_c" => "\x03",
        "ctrl_d" => "\x04",
        _ => return None,
    })
}

/// How the state is named in an answer.
fn state_line(state: State) -> String {
    match state {
        State::Running => "state: running".into(),
        State::WaitingForInput => "state: waiting_for_input".into(),
        State::Exited(info) if info.killed => "state: failed\nreason: killed".into(),
        State::Exited(info) => match info.code {
            Some(0) => "state: finished\nexit_code: 0".into(),
            Some(code) => format!("state: failed\nexit_code: {code}"),
            None => "state: failed\nreason: ended by a signal".into(),
        },
    }
}

fn wait_of(i: &Input) -> Duration {
    Duration::from_millis(i.wait_ms.unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS))
}

/// A session found for the calling bot.
struct Found {
    id: String,
    session: Arc<PtySession>,
    cwd: PathBuf,
}

impl PtyShellTool {
    fn new(
        access: Option<Arc<dyn crate::FsAccessProvider>>,
        provenance: Option<Arc<dyn Provenance>>,
        redactor: Option<OutputRedactor>,
    ) -> Self {
        Self {
            spec: spec(
                "shell_interactive",
                "Run an INTERACTIVE program (installer, login, setup wizard) on a terminal and talk to it. \
                 action start {command} opens it and returns its first output and a session id; \
                 read {session} waits for more output; write {session, input, enter} types text (enter presses Enter), \
                 or {session, key} presses one key (enter, tab, esc, space, up, down, left, right, backspace, ctrl_c, ctrl_d); \
                 screen {session} shows the whole screen; resize {session, cols, rows}; kill {session}; list. \
                 Every answer says the state: running, waiting_for_input, finished or failed (with exit_code). \
                 When the program shows a link or a code that the USER must act on (browser sign-in, device code), \
                 show it to the user and wait for them; do not guess it. Kill the session when done. \
                 The command is a program and its arguments, started directly: NO shell, so no pipes, redirects, \
                 variables, globs or &&. Shells, interpreters, launchers (sudo, env, nohup, timeout...), pagers and \
                 editors with a shell escape (less, vim, git...) are refused: use shell_run for those and for scripts. \
                 Example: {\"action\":\"start\",\"command\":\"vibe --setup\"}",
                json!({"type":"object","properties":{
                    "action":{"type":"string","enum":["start","read","write","screen","resize","kill","list"]},
                    "command":{"type":"string","description":"start: the program and its arguments, started directly (no shell)."},
                    "cwd":{"type":"string","description":"start: working directory (defaults to the bot's workspace)."},
                    "session":{"type":"string","description":"The id start returned."},
                    "input":{"type":"string","description":"write: text to type (up to 4096 characters, no escape characters)."},
                    "enter":{"type":"boolean","description":"write: press Enter after the text."},
                    "key":{"type":"string","description":"write: one named key instead of text."},
                    "wait_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS,"description":"How long to wait for output (default 3000)."},
                    "cols":{"type":"integer","minimum":20,"maximum":300},
                    "rows":{"type":"integer","minimum":5,"maximum":100}
                },"required":["action"]}),
                Effect::Destructive,
                PermissionCategory::Shell,
            ),
            sessions: Arc::new(Sessions::new(LIMITS)),
            access,
            provenance,
            redactor,
            never: NeverList::default_list(),
        }
    }

    /// Mask the daemon's known secrets and the common key shapes.
    fn mask(&self, text: &str) -> String {
        let known = self
            .redactor
            .as_ref()
            .map_or_else(|| text.to_owned(), |r| r(text));
        pattern_redactor().redact(&known)
    }

    /// [`Self::mask`] for several lines. A secret broken across a line break (by the program's
    /// own wrapping, or by a terminal only 20 columns wide) matches no pattern on either line, so
    /// the lines are also masked as the one line they were. When that finds more than the
    /// line-wise pass did, the joined, masked text is shown instead of the broken-up one.
    fn mask_lines(&self, text: &str) -> String {
        let masked = self.mask(text);
        if !text.contains('\n') {
            return masked;
        }
        let joined = self.mask(&text.replace('\n', ""));
        if masked.replace('\n', "") == joined {
            return masked;
        }
        format!("[a secret ran across line breaks: the lines are shown joined]\n{joined}")
    }

    fn answer(&self, id: &str, snap: &Snapshot) -> ToolOutput {
        let mut out = format!("session: {id}\n{}\n", state_line(snap.state));
        if snap.dropped > 0 {
            let _ = writeln!(
                out,
                "note: {} older lines were dropped unread",
                snap.dropped
            );
        }
        if snap.more {
            out.push_str("more_output: true (read again for the rest)\n");
        }
        out.push_str("--- output ---\n");
        out.push_str(&self.mask_lines(&snap.lines.join("\n")));
        out.push('\n');
        if !snap.prompt.is_empty() {
            out.push_str("--- prompt (unfinished line) ---\n");
            out.push_str(&self.mask(&snap.prompt));
            out.push('\n');
        }
        ToolOutput::text(out)
    }

    fn find(&self, ctx: &ToolContext, id: Option<&str>) -> Result<Found, ToolError> {
        let id = id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidInput("session is required".into()))?;
        let mut map = self.sessions.map.lock();
        let entry = map
            .get_mut(id)
            .filter(|e| e.owner == ctx.bot_id)
            .ok_or_else(|| {
                exec_err(format!(
                    "no session {id}: it ended, timed out or never existed"
                ))
            })?;
        entry.last_used = Instant::now();
        Ok(Found {
            id: id.to_owned(),
            session: Arc::clone(&entry.session),
            cwd: entry.cwd.clone(),
        })
    }

    /// Judge what is about to be typed into the session against the never-list, with what was
    /// typed before it on the same line and from the session's own folder, and remember it (only
    /// when all of it passes). Returns what the line was, so a failed write can put it back.
    fn type_into_line(
        &self,
        found: &Found,
        what: impl FnOnce(&mut String, &Path, &NeverList) -> Result<(), String>,
    ) -> Result<String, ToolError> {
        let mut map = self.sessions.map.lock();
        let entry = map
            .get_mut(&found.id)
            .ok_or_else(|| exec_err(format!("no session {}: it ended", found.id)))?;
        let mut line = entry.line.clone();
        what(&mut line, &found.cwd, &self.never).map_err(exec_err)?;
        Ok(std::mem::replace(&mut entry.line, line))
    }

    fn restore_line(&self, id: &str, line: String) {
        if let Some(entry) = self.sessions.map.lock().get_mut(id) {
            entry.line = line;
        }
    }

    /// Forget a session that has ended and was read to the end.
    fn finish(&self, id: &str, snap: &Snapshot) {
        if matches!(snap.state, State::Exited(_)) && !snap.more {
            self.sessions.map.lock().remove(id);
        }
    }

    fn start(&self, ctx: &ToolContext, i: &Input) -> Result<(String, Arc<PtySession>), ToolError> {
        if i.shell.as_ref().is_some_and(|v| !v.is_null()) {
            return Err(ToolError::InvalidInput(
                "there is no shell to choose: the program is started directly. Give the program and its arguments as command"
                    .into(),
            ));
        }
        let command = i.command.as_deref().unwrap_or_default().trim();
        if command.is_empty() {
            return Err(ToolError::InvalidInput("command is empty".into()));
        }
        if command.chars().count() > MAX_COMMAND_CHARS {
            return Err(ToolError::InvalidInput(format!(
                "command is longer than {MAX_COMMAND_CHARS} characters"
            )));
        }
        let cwd = {
            let default_ws = self
                .access
                .as_ref()
                .and_then(|a| a.bot_workspace(ctx.bot_id.as_deref()));
            let cwd = match i.cwd.as_ref().map(PathBuf::from) {
                Some(p) if p.is_relative() => {
                    default_ws.as_ref().map_or(p.clone(), |ws| ws.join(&p))
                }
                Some(p) => p,
                None => default_ws.unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                }),
            };
            std::path::absolute(&cwd).unwrap_or(cwd)
        };
        let mut env = child_env();
        env.retain(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case("TERM"));
        env.push(("TERM".into(), "xterm-256color".into()));
        let path_var = env
            .iter()
            .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, v)| v.clone());
        let plan = plan(command, &cwd, path_var.as_deref()).map_err(exec_err)?;
        // What will really run, from the folder it runs in, against the never-list. The permission
        // layer saw the raw text; this is the program and its words as they are executed.
        if self.never.matches_in(&plan.line, cwd.to_str()) {
            return Err(exec_err(
                "this command line matches the never-list (a catastrophic command, a Praxi credential or a coding \
                 CLI's sign-in store). An interactive session cannot ask for approval of it: use shell_run, which asks",
            ));
        }
        let fetched = Fetched {
            ledger: self.provenance.clone(),
            targets: crate::provenance::shell_fetch_targets(&plan.line, &cwd),
        };
        fetched.record();

        self.sessions.sweep();
        let mut map = self.sessions.map.lock();
        if map.values().filter(|e| e.owner == ctx.bot_id).count() >= MAX_SESSIONS_PER_BOT {
            return Err(exec_err(format!(
                "this bot already has {MAX_SESSIONS_PER_BOT} interactive sessions open: kill one first (action list shows them)"
            )));
        }
        if map.len() >= MAX_SESSIONS_TOTAL {
            return Err(exec_err(format!(
                "{MAX_SESSIONS_TOTAL} interactive sessions are open on this computer: try again when one has ended"
            )));
        }
        let session = PtySession::spawn(&SpawnSpec {
            program: plan.program.into_os_string(),
            args: plan.args,
            cwd: Some(cwd.clone()),
            env,
            cols: i.cols.unwrap_or(DEFAULT_COLS),
            rows: i.rows.unwrap_or(DEFAULT_ROWS),
        })
        .map_err(|e| exec_err(format!("could not start the program: {e}")))?;
        let session = Arc::new(session);
        let id = format!("pty-{}", &uuid::Uuid::now_v7().simple().to_string()[20..28]);
        let masked = self.mask(command);
        tracing::info!(session = %id, command = %masked, "interactive session started");
        let now = Instant::now();
        map.insert(
            id.clone(),
            Entry {
                session: Arc::clone(&session),
                owner: ctx.bot_id.clone(),
                started: now,
                last_used: now,
                command: masked,
                cwd,
                line: String::new(),
                _fetched: fetched,
            },
        );
        drop(map);
        start_reaper(&self.sessions);
        Ok((id, session))
    }
}

#[async_trait]
impl Tool for PtyShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input = parse(input)?;
        self.sessions.sweep();
        match i.action {
            Action::Start => {
                let (id, session) = self.start(ctx, &i)?;
                let snap = session.read(wait_of(&i), SETTLE, MAX_READ_BYTES).await;
                // An ended session whose output was not all returned stays, to be read again.
                self.finish(&id, &snap);
                Ok(self.answer(&id, &snap))
            }
            Action::Read => {
                let found = self.find(ctx, i.session.as_deref())?;
                let snap = found
                    .session
                    .read(wait_of(&i), SETTLE, MAX_READ_BYTES)
                    .await;
                self.finish(&found.id, &snap);
                Ok(self.answer(&found.id, &snap))
            }
            Action::Write => {
                let found = self.find(ctx, i.session.as_deref())?;
                let mut bytes = String::new();
                match (&i.key, &i.text) {
                    (Some(_), Some(_)) => {
                        return Err(ToolError::InvalidInput(
                            "give input or key, not both".into(),
                        ));
                    }
                    (Some(key), None) => {
                        let named = key_bytes(key).ok_or_else(|| {
                            ToolError::InvalidInput(format!("unknown key {key:?}"))
                        })?;
                        bytes.push_str(named);
                    }
                    (None, Some(text)) => {
                        if text
                            .chars()
                            .any(|c| c.is_control() && c != '\t' && c != '\n')
                        {
                            return Err(ToolError::InvalidInput(
                                "input has control characters: use key for Enter, Esc, arrows and Ctrl-C".into(),
                            ));
                        }
                        bytes.push_str(&text.replace('\n', "\r"));
                    }
                    (None, None) => {
                        if i.enter != Some(true) {
                            return Err(ToolError::InvalidInput("nothing to write".into()));
                        }
                    }
                }
                if i.enter == Some(true) {
                    bytes.push('\r');
                }
                // The logical line the program will hold is judged before anything is sent.
                let before = self.type_into_line(&found, |line, cwd, never| {
                    if let Some(text) = &i.text {
                        TypedLine::type_text(line, text, cwd, never)?;
                    }
                    if let Some(key) = &i.key {
                        TypedLine::press(line, key, cwd, never)?;
                    }
                    if i.enter == Some(true) {
                        TypedLine::press(line, "enter", cwd, never)?;
                    }
                    Ok(())
                })?;
                // The text is never logged: it can be a password or a code.
                if let Err(e) = found.session.write(bytes.as_bytes()) {
                    self.restore_line(&found.id, before);
                    return Err(exec_err(format!("could not type into {}: {e}", found.id)));
                }
                let snap = found
                    .session
                    .read(wait_of(&i), SETTLE, MAX_READ_BYTES)
                    .await;
                self.finish(&found.id, &snap);
                Ok(self.answer(&found.id, &snap))
            }
            Action::Screen => {
                let found = self.find(ctx, i.session.as_deref())?;
                let state = found.session.exit().map_or(State::Running, State::Exited);
                let (cols, rows) = found.session.size();
                Ok(ToolOutput::text(format!(
                    "session: {}\n{}\nsize: {cols}x{rows}\n--- screen ---\n{}\n",
                    found.id,
                    state_line(state),
                    self.mask_lines(&found.session.screen())
                )))
            }
            Action::Resize => {
                let found = self.find(ctx, i.session.as_deref())?;
                let (cols, rows) = found.session.size();
                found
                    .session
                    .resize(i.cols.unwrap_or(cols), i.rows.unwrap_or(rows))
                    .map_err(|e| exec_err(format!("could not resize {}: {e}", found.id)))?;
                let (cols, rows) = found.session.size();
                Ok(ToolOutput::text(format!(
                    "session: {}\nsize: {cols}x{rows}\n",
                    found.id
                )))
            }
            Action::Kill => {
                let found = self.find(ctx, i.session.as_deref())?;
                found.session.kill();
                self.sessions.map.lock().remove(&found.id);
                tracing::info!(session = %found.id, "interactive session killed");
                Ok(ToolOutput::text(format!(
                    "session: {}\nstate: failed\nreason: killed\n",
                    found.id
                )))
            }
            Action::List => {
                let map = self.sessions.map.lock();
                let mut rows: Vec<String> = map
                    .iter()
                    .filter(|(_, e)| e.owner == ctx.bot_id)
                    .map(|(id, e)| {
                        let state = e.session.exit().map_or("running".to_owned(), |x| {
                            format!("ended (code {:?})", x.code)
                        });
                        format!(
                            "{id}  {state}  {}s  {}",
                            e.started.elapsed().as_secs(),
                            e.command
                        )
                    })
                    .collect();
                rows.sort();
                Ok(ToolOutput::text(if rows.is_empty() {
                    "no interactive sessions\n".to_owned()
                } else {
                    rows.join("\n") + "\n"
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tool_registry::ContentPart;

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!("not text"),
        }
    }

    fn bare() -> PtyShellTool {
        PtyShellTool::new(None, None, None)
    }

    /// A directory with an executable called `name` in it, and a `PATH` that holds only it.
    #[cfg(unix)]
    fn bin_dir(names: &[&str]) -> (tempfile::TempDir, OsString) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            let file = dir.path().join(name);
            std::fs::write(&file, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([dir.path()]).unwrap();
        (dir, path)
    }

    #[test]
    fn a_command_is_words_and_quotes_and_nothing_else() {
        assert_eq!(split_command("vibe --setup").unwrap(), ["vibe", "--setup"]);
        assert_eq!(
            split_command("  tool  \"a b\"  'c d'  e\"f\"g ").unwrap(),
            ["tool", "a b", "c d", "efg"]
        );
        // Quotes make shell characters plain text: nothing interprets them.
        assert_eq!(
            split_command("tool 'a|b;c' \"$HOME\" \"`x`\"").unwrap(),
            ["tool", "a|b;c", "$HOME", "`x`"]
        );
        assert_eq!(split_command("tool ''").unwrap(), ["tool", ""]);
        for bad in [
            "",
            "   ",
            "a | b",
            "a || b",
            "a && b",
            "a & b",
            "a; b",
            "a > out",
            "a < in",
            "a >> out",
            "a 2>&1",
            "echo $(whoami)",
            "echo `whoami`",
            "echo $HOME",
            "a *.txt",
            "a file?.txt",
            "a [x]",
            "a {1,2}",
            "! tool",
            "{ tool; }",
            "(tool)",
            "~/tool",
            "tool ~",
            "# tool",
            "a\nb",
            "a\rb",
            "a\0b",
            "a\x07b",
            "tool 'unclosed",
            "tool \"unclosed",
        ] {
            assert!(split_command(bad).is_err(), "{bad:?}");
        }
        #[cfg(unix)]
        assert!(split_command("tool a\\ b").is_err());
        assert!(split_command(&"x ".repeat(MAX_ARGS + 1)).is_err());
    }

    #[test]
    fn a_program_is_refused_by_the_name_of_what_it_is() {
        for bad in [
            "sh",
            "bash",
            "zsh",
            "dash",
            "BASH",
            "bash.exe",
            "pwsh",
            "powershell.exe",
            "cmd.exe",
            "python",
            "python3",
            "python3.12",
            "python3.12.exe",
            "bash-5.2",
            "perl5.38.2",
            "node22",
            "nodejs",
            "ruby3.2",
            "sudo",
            "su",
            "env",
            "command",
            "eval",
            "exec",
            "nohup",
            "setsid",
            "timeout",
            "nice",
            "stdbuf",
            "rlwrap",
            "xargs",
            "ssh",
            "tmux",
            "screen",
            "docker",
            "busybox",
            "less",
            "more",
            "vim",
            "vi",
            "nvim",
            "nano",
            "emacs",
            "man",
            "git",
            "awk",
            "gawk",
            "find",
            "sqlite3",
            "wsl",
        ] {
            assert!(refused_name(bad).is_some(), "{bad}");
        }
        for fine in [
            "vibe",
            "claude",
            "codex",
            "opencode",
            "agy",
            "cursor-agent",
            "kiro-cli",
            "kimi",
            "qodercli",
            "gh",
            "npm",
            "npx",
            "install.sh",
            "installer",
            "setup",
            "shred",
            "bashful",
            "pythonista",
            "rustup",
            "7z",
            "tool",
            "docker-compose-x",
        ] {
            assert_eq!(refused_name(fine), None, "{fine}");
        }
    }

    /// The bypass strings of the security review, and others of the same kind: every one is an
    /// error, whether the program they name is installed or not, and nothing runs.
    #[cfg(unix)]
    #[test]
    fn the_known_ways_around_the_shell_refusal_all_fail() {
        let (_dir, path) = bin_dir(&["tool"]);
        let cwd = std::env::temp_dir();
        for bad in [
            // quoting and escaping
            "b\"\"ash",
            "'b'ash",
            "\\bash",
            "p`wsh",
            "c^md",
            "\"/bin/bash\"",
            "/bin/bash --login",
            "/usr/bin/env bash",
            // keywords and grouping
            "{ bash; }",
            "! sh",
            "(bash)",
            "FOO=1 bash",
            "time bash",
            // wrappers and launchers
            "command bash",
            "eval bash",
            "exec bash",
            "nohup bash",
            "setsid bash",
            "timeout 100 bash",
            "nice bash",
            "stdbuf -o0 bash",
            "rlwrap bash",
            "sudo -i",
            "env sh",
            "xargs sh",
            "script -qc bash /dev/null",
            "tmux",
            "screen",
            "ssh host",
            // interpreters under any name
            "python3.12",
            "python3 -i",
            "perl -de0",
            "node",
            "ruby -e 1",
            "lua",
            "awk 'BEGIN{system(\"sh\")}'",
            // command substitution and chaining
            "$(echo bash)",
            "`echo bash`",
            "echo hi; bash",
            "ls && bash",
            "echo hi | sh",
            "a\nbash",
            // a shell escape inside an allowed-looking TUI
            "less /etc/passwd",
            "vim",
            "git log",
            "man ls",
        ] {
            assert!(plan(bad, &cwd, Some(&path)).is_err(), "{bad:?}");
        }
        // A real program on the path is accepted.
        let ok = plan("tool --flag \"two words\"", &cwd, Some(&path)).unwrap();
        assert!(ok.program.ends_with("tool"));
        assert_eq!(ok.args, ["--flag", "two words"]);
        assert_eq!(ok.line, "tool --flag \"two words\"");
    }

    /// What a program really is decides, not what it is called: a link, a script of the same
    /// name as a shell, a `python3` that points to `python3.12`, `sh` that is `busybox`.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_shell_is_a_shell() {
        let (dir, _) = bin_dir(&["real-tool"]);
        let sh = ["/bin/sh", "/usr/bin/sh", "/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists())
            .expect("a shell");
        std::os::unix::fs::symlink(sh, dir.path().join("harmless")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("harmless"), dir.path().join("second-hop"))
            .unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let cwd = dir.path();
        for name in ["harmless", "second-hop", "./harmless"] {
            let err = plan(name, cwd, Some(&path)).unwrap_err();
            assert!(err.contains("refused"), "{name}: {err}");
        }
        // By path too, with the folder the program runs in as the base of a relative one.
        let err = plan(&dir.path().join("harmless").to_string_lossy(), cwd, None).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert!(plan("real-tool", cwd, Some(&path)).is_ok());
        assert!(plan("./real-tool", cwd, None).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn only_absolute_path_entries_are_searched_and_the_file_must_be_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let cwd = dir.path();
        assert!(plan("tool", cwd, Some(&path)).is_err(), "not executable");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(plan("tool", cwd, Some(&path)).is_ok());
        // `.` and an empty entry mean "here": never searched.
        assert!(plan("tool", cwd, Some(OsStr::new("."))).is_err());
        assert!(plan("tool", cwd, Some(OsStr::new(""))).is_err());
        assert!(plan("tool", cwd, None).is_err());
        assert!(plan("FOO=1 tool", cwd, Some(&path)).is_err());
    }

    #[test]
    fn a_command_typed_in_pieces_is_judged_as_the_line_it_becomes() {
        let never = NeverList::default_list();
        let cwd = std::env::temp_dir();
        let mut line = String::new();
        // `rm ` then `-rf /`: neither piece is a catastrophe on its own.
        assert!(NeverList::default_list().matches("rm -rf /"));
        assert!(!never.matches_in("rm ", cwd.to_str()));
        TypedLine::type_text(&mut line, "rm ", &cwd, &never).unwrap();
        assert_eq!(line, "rm ");
        let refused = TypedLine::type_text(&mut line, "-rf /", &cwd, &never).unwrap_err();
        assert!(refused.contains("never-list"), "{refused}");
        assert!(
            !refused.contains("-rf"),
            "the text is not echoed: {refused}"
        );
        assert_eq!(line, "rm ", "a refused piece is not kept");
        // Pieces and named keys: `rm`, space, `-rf`, space, `/`.
        let mut line = String::new();
        for piece in ["rm", " -rf"] {
            TypedLine::type_text(&mut line, piece, &cwd, &never).unwrap();
        }
        TypedLine::press(&mut line, "space", &cwd, &never).unwrap();
        assert!(TypedLine::type_text(&mut line, "/", &cwd, &never).is_err());
        // A line break starts the next line: `echo hi` is fine, `rm -rf /` after it is not.
        let mut line = String::new();
        assert!(TypedLine::type_text(&mut line, "echo hi\nrm -rf /\n", &cwd, &never).is_err());
        assert_eq!(line, "");
        TypedLine::type_text(&mut line, "echo hi\nsecond", &cwd, &never).unwrap();
        assert_eq!(line, "second");
        // Enter ends the line.
        TypedLine::press(&mut line, "enter", &cwd, &never).unwrap();
        assert_eq!(line, "");
        TypedLine::type_text(&mut line, "-rf /", &cwd, &never).unwrap();
        // A backspace that turns a harmless line into a catastrophic one is refused.
        let mut line = "rm -rf /x".to_owned();
        assert!(!never.matches_in(&line, cwd.to_str()));
        assert!(TypedLine::press(&mut line, "backspace", &cwd, &never).is_err());
        assert_eq!(line, "rm -rf /x");
        // Ctrl-C clears; a very long line is refused.
        TypedLine::press(&mut line, "ctrl_c", &cwd, &never).unwrap();
        assert_eq!(line, "");
        assert!(
            TypedLine::type_text(&mut line, &"a".repeat(MAX_TYPED_LINE + 1), &cwd, &never).is_err()
        );
    }

    /// The session's folder is what a relative path in the typed text means: `cat .claude/x`
    /// typed into a session that runs in the home folder names a CLI sign-in store.
    #[test]
    fn a_relative_path_is_judged_from_the_session_folder() {
        let Some(home) = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
        else {
            return;
        };
        let never = NeverList::default_list();
        let typed = "cat .claude/.credentials.json";
        let mut line = String::new();
        assert!(TypedLine::type_text(&mut line, typed, &home, &never).is_err());
        // The same words from another folder name some other file.
        let elsewhere = std::env::temp_dir().join("praxi-pty-shell-elsewhere");
        let mut line = String::new();
        assert!(TypedLine::type_text(&mut line, typed, &elsewhere, &never).is_ok());
    }

    #[test]
    fn named_keys_map_to_terminal_bytes() {
        assert_eq!(key_bytes("Enter"), Some("\r"));
        assert_eq!(key_bytes("down"), Some("\x1b[B"));
        assert_eq!(key_bytes("ctrl_c"), Some("\x03"));
        assert_eq!(key_bytes("rm -rf"), None);
    }

    #[test]
    fn states_are_named_plainly() {
        use pty_session::ExitInfo;
        assert_eq!(state_line(State::Running), "state: running");
        assert_eq!(
            state_line(State::WaitingForInput),
            "state: waiting_for_input"
        );
        let exited = |code, killed| State::Exited(ExitInfo { code, killed });
        assert_eq!(
            state_line(exited(Some(0), false)),
            "state: finished\nexit_code: 0"
        );
        assert_eq!(
            state_line(exited(Some(2), false)),
            "state: failed\nexit_code: 2"
        );
        assert!(state_line(exited(None, true)).contains("killed"));
        assert!(state_line(exited(None, false)).contains("signal"));
    }

    fn snapshot(lines: &[&str], prompt: &str) -> Snapshot {
        Snapshot {
            lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            prompt: prompt.to_owned(),
            state: State::Running,
            dropped: 0,
            more: false,
        }
    }

    #[test]
    fn answers_mask_secrets() {
        let secret = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
        let shown = text(&bare().answer(
            "s",
            &snapshot(
                &[&format!("token {secret}")],
                &format!("key sk-{}", "abcDEF0123456789".repeat(2)),
            ),
        ));
        assert!(!shown.contains(&secret), "{shown}");
        assert!(!shown.contains("abcDEF0123456789"), "{shown}");
        assert!(shown.contains("[REDACTED]"), "{shown}");
    }

    /// A value only the daemon knows (a vault entry has no key shape) is masked, in the output,
    /// the prompt and the screen.
    #[test]
    fn the_daemons_own_secrets_are_masked() {
        let vault_value = "correct-horse-battery-staple-9137";
        let mut known = secret_store::Redactor::new();
        known.add_secret(vault_value);
        let tool = PtyShellTool::new(None, None, Some(Arc::new(move |t: &str| known.redact(t))));
        let shown = text(&tool.answer(
            "s",
            &snapshot(
                &[&format!("password is {vault_value}")],
                &format!("again {vault_value}"),
            ),
        ));
        assert!(!shown.contains(vault_value), "{shown}");
        assert!(
            !tool
                .mask_lines(&format!("a\n{vault_value}\nb"))
                .contains(vault_value)
        );
        // Without the daemon's redactor the value has no shape to match: the gap this closes.
        assert!(bare().mask(vault_value).contains(vault_value));
    }

    /// A secret broken across a line break (the program's own wrapping, or a terminal only 20
    /// columns wide) is masked as the one secret it is.
    #[test]
    fn a_secret_wrapped_across_lines_is_masked() {
        let key = format!("sk-{}", "abcDEF0123456789".repeat(3));
        let (head, tail) = key.split_at(20);
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        for secret in [key.as_str(), jwt] {
            let (a, b) = secret.split_at(secret.len() / 2);
            let tool = bare();
            // Masked line by line, as before, a half of it gets through: this is what is closed.
            let line_wise = tool.mask(&format!("{a}\n{b}"));
            assert!(
                line_wise.contains(a) || line_wise.contains(b),
                "{line_wise}"
            );
            // The output of a read, and the screen of a narrow terminal.
            for shown in [
                text(&tool.answer("s", &snapshot(&["first", a, b, "last"], ""))),
                tool.mask_lines(&format!("first\n{a}\n{b}\nlast")),
            ] {
                assert!(!shown.contains(a) && !shown.contains(b), "{shown}");
                assert!(!shown.contains(&secret[5..40]), "{shown}");
            }
        }
        let shown = bare().mask_lines(&format!("{head}\n{tail}"));
        assert!(!shown.contains(head) && !shown.contains(tail), "{shown}");
        // Text with no secret is left as it is, line breaks included.
        assert_eq!(bare().mask_lines("a\nb\nc"), "a\nb\nc");
    }

    #[cfg(unix)]
    fn start_input(command: &str, cwd: Option<&Path>) -> Input {
        parse(json!({"action":"start","command":command,"cwd":cwd})).unwrap()
    }

    #[cfg(unix)]
    fn pid_alive(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    #[cfg(unix)]
    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..100 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never happened: {what}");
    }

    /// Nobody calls the tool again: the idle limit is still enforced, by the reaper thread.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_abandoned_session_is_reaped_without_another_call() {
        let mut tool = bare();
        tool.sessions = Arc::new(Sessions::new(Limits {
            idle: Duration::from_millis(300),
            reap_every: Duration::from_millis(50),
            ..LIMITS
        }));
        let (_id, session) = tool
            .start(&ToolContext::default(), &start_input("sleep 300", None))
            .unwrap();
        let pid = session.pid().unwrap();
        assert!(pid_alive(pid));
        drop(session);
        until("the session is reaped", || {
            tool.sessions.map.lock().is_empty()
        })
        .await;
        until("its process is gone", || !pid_alive(pid)).await;
        // The reaper stopped with the last session and starts again with the next.
        until("the reaper stops", || {
            !tool.sessions.reaper.load(Ordering::SeqCst)
        })
        .await;
        let (_id, session) = tool
            .start(&ToolContext::default(), &start_input("sleep 300", None))
            .unwrap();
        assert!(tool.sessions.reaper.load(Ordering::SeqCst));
        let pid = session.pid().unwrap();
        drop(session);
        until("the next session is reaped too", || !pid_alive(pid)).await;
    }

    /// The hour limit holds for a session that is used all the time.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_in_use_still_ends_at_the_hour_limit() {
        let mut tool = bare();
        tool.sessions = Arc::new(Sessions::new(Limits {
            hard: Duration::from_millis(300),
            reap_every: Duration::from_millis(50),
            ..LIMITS
        }));
        let (id, session) = tool
            .start(&ToolContext::default(), &start_input("sleep 300", None))
            .unwrap();
        let pid = session.pid().unwrap();
        drop(session);
        for _ in 0..4 {
            let _ = tool.find(&ToolContext::default(), Some(&id));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        until("its process is gone", || !pid_alive(pid)).await;
    }

    /// Dropping the tool (the daemon shuts down) ends everything it started.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_tool_kills_every_session() {
        let tool = bare();
        let mut pids = Vec::new();
        for bot in ["a", "b"] {
            let ctx = ToolContext {
                bot_id: Some(bot.into()),
                ..ToolContext::default()
            };
            let (_id, session) = tool.start(&ctx, &start_input("sleep 300", None)).unwrap();
            pids.push(session.pid().unwrap());
        }
        assert!(pids.iter().all(|p| pid_alive(*p)));
        drop(tool);
        until("everything is gone", || pids.iter().all(|p| !pid_alive(*p))).await;
    }

    /// Every slot is counted against its bot, and the total has a ceiling.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_quotas_count_per_bot_and_in_all() {
        let tool = bare();
        let ctx = |bot: &str| ToolContext {
            bot_id: Some(bot.into()),
            ..ToolContext::default()
        };
        let mut open = Vec::new();
        for n in 0..MAX_SESSIONS_TOTAL / MAX_SESSIONS_PER_BOT {
            let bot = format!("bot-{n}");
            for _ in 0..MAX_SESSIONS_PER_BOT {
                open.push(
                    tool.start(&ctx(&bot), &start_input("sleep 300", None))
                        .unwrap(),
                );
            }
            let over = tool
                .start(&ctx(&bot), &start_input("sleep 300", None))
                .map(|_| ())
                .unwrap_err()
                .to_string();
            assert!(over.contains("already has"), "{over}");
        }
        // Every bot is at its own quota and the computer is full.
        let full = tool
            .start(&ctx("one-more"), &start_input("sleep 300", None))
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(full.contains("on this computer"), "{full}");
    }

    #[cfg(unix)]
    struct Ledger(Mutex<Vec<(String, bool, String)>>);
    #[cfg(unix)]
    impl Provenance for Ledger {
        fn is_marked(&self, _normalized: &str) -> bool {
            false
        }
        fn mark(&self, normalized: &str, tree: bool, origin: &str) {
            self.0
                .lock()
                .push((normalized.to_owned(), tree, origin.to_owned()));
        }
    }

    /// What the command fetches is in the provenance ledger, as for `shell_run`.
    #[cfg(unix)]
    #[tokio::test]
    async fn what_a_command_fetches_is_recorded_in_the_provenance_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Ledger(Mutex::new(Vec::new())));
        let tool = PtyShellTool::new(None, Some(Arc::clone(&ledger) as Arc<dyn Provenance>), None);
        let started = tool.start(
            &ToolContext::default(),
            &start_input(
                "curl -s -m 2 -o fetched.bin http://127.0.0.1:9/x",
                Some(dir.path()),
            ),
        );
        let Ok((_id, session)) = started else {
            // No `curl` on this machine: nothing to run; the target detection has its own tests.
            return;
        };
        let marks = ledger.0.lock().clone();
        assert!(
            marks
                .iter()
                .any(|(path, tree, origin)| path.ends_with("/fetched.bin")
                    && !tree
                    && origin == "fetched by a shell command"),
            "{marks:?}"
        );
        session.kill();
    }

    #[tokio::test]
    async fn the_input_is_validated_before_anything_runs() {
        let t = tool(None, None, None);
        let c = ToolContext::default();
        for bad in [
            json!({"action":"start"}),
            json!({"action":"start","command":"   "}),
            json!({"action":"start","command":"bash"}),
            json!({"action":"start","command":"echo hi | cat"}),
            json!({"action":"start","command":"tool","shell":"bash"}),
            json!({"action":"read"}),
            json!({"action":"read","session":"pty-nope"}),
            json!({"action":"write","session":"pty-nope","input":"x"}),
            json!({"action":"nonsense"}),
        ] {
            assert!(t.call(&c, bad.clone()).await.is_err(), "{bad}");
        }
        let listed = t.call(&c, json!({"action":"list"})).await.unwrap();
        assert!(text(&listed).contains("no interactive sessions"));
    }
}
