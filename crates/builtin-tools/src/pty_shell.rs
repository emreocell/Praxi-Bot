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
//! Policy is the same as `shell_run`:
//!
//! * the permission layer sees the command (`raw_command_for`) before `start`, and the typed text
//!   before `write`, so the never-list and the approval rules judge both;
//! * the environment is the same allowlist (no vault, bearer or provider credential);
//! * the working directory defaults to the bot's workspace;
//! * a program that is itself a shell or an interpreter (`sh`, `bash`, `python`, `sudo`, `ssh`...)
//!   is refused: typing into one would run arbitrary commands one keystroke at a time, around
//!   the command check. Use `shell_run` for scripts.
//!
//! Bounds: 4 sessions, 8 KiB of text per read, 4 KiB per write, 10 minutes idle, 1 hour in all.
//! Output is masked with the production redactor before the bot sees it; typed text and output
//! are never logged.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use pty_session::{PtySession, Snapshot, SpawnSpec, State};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::shell::{ShellKind, child_env};
use crate::util::{exec_err, parse, spec};

/// Sessions alive at once (all bots together).
pub const MAX_SESSIONS: usize = 4;
/// No call for this long: the session is killed.
const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How long an ended session stays readable (its last output) before its slot is freed.
const ENDED_KEEP: Duration = Duration::from_secs(2 * 60);
/// No session lives longer than this.
const HARD_LIMIT: Duration = Duration::from_secs(60 * 60);
/// Most text one answer carries.
const MAX_READ_BYTES: usize = 8 * 1024;
/// Output must be quiet this long before a read returns early.
const SETTLE: Duration = Duration::from_millis(400);
const DEFAULT_WAIT_MS: u64 = 3_000;
const MAX_WAIT_MS: u64 = 60_000;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;

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
    /// `start`: the command line, as for `shell_run`.
    command: Option<String>,
    shell: Option<ShellKind>,
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

struct Entry {
    session: Arc<PtySession>,
    owner: Option<String>,
    started: Instant,
    last_used: Instant,
    /// The command as shown to the bot (masked).
    command: String,
}

/// The `shell_interactive` tool.
pub struct PtyShellTool {
    spec: ToolSpec,
    sessions: Mutex<HashMap<String, Entry>>,
    access: Option<Arc<dyn crate::FsAccessProvider>>,
}

/// Build the tool; `access` makes `cwd` default to the calling bot's workspace.
#[must_use]
pub fn tool(access: Option<Arc<dyn crate::FsAccessProvider>>) -> Arc<dyn Tool> {
    Arc::new(PtyShellTool {
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
             Not for scripts or shells (use shell_run); shells and interpreters are refused. Example: {\"action\":\"start\",\"command\":\"vibe --setup\"}",
            json!({"type":"object","properties":{
                "action":{"type":"string","enum":["start","read","write","screen","resize","kill","list"]},
                "command":{"type":"string","description":"start: the command line, passed to the shell as one argument."},
                "shell":{"type":"string","enum":["powershell","cmd","bash","sh"],"description":"start: default powershell on Windows, sh elsewhere."},
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
        sessions: Mutex::new(HashMap::new()),
        access,
    })
}

fn redactor() -> &'static secret_store::Redactor {
    static REDACTOR: OnceLock<secret_store::Redactor> = OnceLock::new();
    REDACTOR.get_or_init(secret_store::Redactor::new)
}

fn mask(text: &str) -> String {
    redactor().redact(text)
}

/// Programs whose own prompt runs whatever is typed into it. Typing into them would walk
/// around the per-command check, so they cannot be started here.
const REFUSED_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ash",
    "ksh",
    "csh",
    "tcsh",
    "busybox",
    "powershell",
    "pwsh",
    "cmd",
    "python",
    "python3",
    "py",
    "node",
    "nodejs",
    "deno",
    "bun",
    "perl",
    "ruby",
    "irb",
    "php",
    "lua",
    "sudo",
    "su",
    "doas",
    "ssh",
    "telnet",
    "nc",
    "ncat",
    "socat",
    "script",
    "screen",
    "tmux",
    "env",
    "exec",
    "xargs",
    "nsenter",
    "chroot",
    "docker",
    "podman",
    "kubectl",
    "wsl",
];

/// Why `command` may not run here, if it may not: the program it starts is a shell or
/// interpreter (or a launcher of one). Looks at every simple command of a compound line.
fn refused(command: &str) -> Option<String> {
    for part in command.split(['&', '|', ';', '\n', '(', ')', '`']) {
        let word = part
            .split_whitespace()
            // Leading `VAR=value` assignments and a `-` or `exec`-style prefix are skipped.
            .find(|w| !w.contains('=') || w.starts_with(['/', '.']))
            .unwrap_or_default();
        let name = word
            .trim_matches(['"', '\'', '$', '{', '}'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        if REFUSED_PROGRAMS.contains(&name) {
            return Some(format!(
                "`{name}` is a shell or interpreter: typing into it would run commands around the command check. \
                 Run the real program here, or use shell_run for scripts"
            ));
        }
    }
    None
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

fn program_for(kind: ShellKind, command: &str) -> (OsString, Vec<OsString>) {
    match kind {
        ShellKind::Powershell => (
            "powershell.exe".into(),
            // Constant prefix forces UTF-8; the command stays one argument. Unlike
            // `shell_run`, no `-NonInteractive`: the whole point is that it may ask.
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-Command".into(),
                format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {command}").into(),
            ],
        ),
        ShellKind::Cmd => (
            "cmd.exe".into(),
            vec!["/d".into(), "/s".into(), "/c".into(), command.into()],
        ),
        ShellKind::Bash => ("bash".into(), vec!["-c".into(), command.into()]),
        ShellKind::Sh => ("sh".into(), vec!["-c".into(), command.into()]),
    }
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

fn answer(id: &str, snap: &Snapshot) -> ToolOutput {
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
    out.push_str(&mask(&snap.lines.join("\n")));
    out.push('\n');
    if !snap.prompt.is_empty() {
        out.push_str("--- prompt (unfinished line) ---\n");
        out.push_str(&mask(&snap.prompt));
        out.push('\n');
    }
    ToolOutput::text(out)
}

impl PtyShellTool {
    /// Remove sessions that ended up idle, too old, or finished long ago, killing them.
    fn sweep(&self) {
        let now = Instant::now();
        let dead: Vec<Entry> = {
            let mut map = self.sessions.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, e)| {
                    now.duration_since(e.last_used) > IDLE_LIMIT
                        || now.duration_since(e.started) > HARD_LIMIT
                        // An ended session the bot never read again frees its slot.
                        || (e.session.exit().is_some()
                            && now.duration_since(e.last_used) > ENDED_KEEP)
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for entry in dead {
            entry.session.kill();
        }
    }

    fn find(
        &self,
        ctx: &ToolContext,
        id: Option<&str>,
    ) -> Result<(String, Arc<PtySession>), ToolError> {
        let id = id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidInput("session is required".into()))?;
        let mut map = self.sessions.lock();
        let entry = map
            .get_mut(id)
            .filter(|e| e.owner == ctx.bot_id)
            .ok_or_else(|| {
                exec_err(format!(
                    "no session {id}: it ended, timed out or never existed"
                ))
            })?;
        entry.last_used = Instant::now();
        Ok((id.to_owned(), Arc::clone(&entry.session)))
    }

    fn start(&self, ctx: &ToolContext, i: &Input) -> Result<(String, Arc<PtySession>), ToolError> {
        let command = i.command.as_deref().unwrap_or_default().trim();
        if command.is_empty() {
            return Err(ToolError::InvalidInput("command is empty".into()));
        }
        if let Some(why) = refused(command) {
            return Err(exec_err(why));
        }
        let cwd = {
            let default_ws = self
                .access
                .as_ref()
                .and_then(|a| a.bot_workspace(ctx.bot_id.as_deref()));
            match i.cwd.as_ref().map(PathBuf::from) {
                Some(p) if p.is_relative() => {
                    default_ws.as_ref().map_or(p.clone(), |ws| ws.join(&p))
                }
                Some(p) => p,
                None => default_ws.unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                }),
            }
        };
        let kind = i.shell.unwrap_or(if cfg!(windows) {
            ShellKind::Powershell
        } else {
            ShellKind::Sh
        });
        let (program, args) = program_for(kind, command);
        let mut env = child_env();
        env.retain(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case("TERM"));
        env.push(("TERM".into(), "xterm-256color".into()));
        let mut map = self.sessions.lock();
        if map.len() >= MAX_SESSIONS {
            return Err(exec_err(format!(
                "{MAX_SESSIONS} interactive sessions are already open: kill one first (action list shows them)"
            )));
        }
        let session = PtySession::spawn(&SpawnSpec {
            program,
            args,
            cwd: Some(cwd),
            env,
            cols: i.cols.unwrap_or(DEFAULT_COLS),
            rows: i.rows.unwrap_or(DEFAULT_ROWS),
        })
        .map_err(|e| exec_err(format!("could not start {kind:?}: {e}")))?;
        let session = Arc::new(session);
        let id = format!("pty-{}", &uuid::Uuid::now_v7().simple().to_string()[20..28]);
        let masked = mask(command);
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
            },
        );
        Ok((id, session))
    }
}

fn wait_of(i: &Input) -> Duration {
    Duration::from_millis(i.wait_ms.unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS))
}

#[async_trait]
impl Tool for PtyShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input = parse(input)?;
        self.sweep();
        match i.action {
            Action::Start => {
                let (id, session) = self.start(ctx, &i)?;
                let snap = session.read(wait_of(&i), SETTLE, MAX_READ_BYTES).await;
                if matches!(snap.state, State::Exited(_)) {
                    self.sessions.lock().remove(&id);
                }
                Ok(answer(&id, &snap))
            }
            Action::Read => {
                let (id, session) = self.find(ctx, i.session.as_deref())?;
                let snap = session.read(wait_of(&i), SETTLE, MAX_READ_BYTES).await;
                if matches!(snap.state, State::Exited(_)) && !snap.more {
                    self.sessions.lock().remove(&id);
                }
                Ok(answer(&id, &snap))
            }
            Action::Write => {
                let (id, session) = self.find(ctx, i.session.as_deref())?;
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
                // The text is never logged: it can be a password or a code.
                session
                    .write(bytes.as_bytes())
                    .map_err(|e| exec_err(format!("could not type into {id}: {e}")))?;
                let snap = session.read(wait_of(&i), SETTLE, MAX_READ_BYTES).await;
                if matches!(snap.state, State::Exited(_)) && !snap.more {
                    self.sessions.lock().remove(&id);
                }
                Ok(answer(&id, &snap))
            }
            Action::Screen => {
                let (id, session) = self.find(ctx, i.session.as_deref())?;
                let state = session.exit().map_or(State::Running, State::Exited);
                let (cols, rows) = session.size();
                Ok(ToolOutput::text(format!(
                    "session: {id}\n{}\nsize: {cols}x{rows}\n--- screen ---\n{}\n",
                    state_line(state),
                    mask(&session.screen())
                )))
            }
            Action::Resize => {
                let (id, session) = self.find(ctx, i.session.as_deref())?;
                let (cols, rows) = session.size();
                session
                    .resize(i.cols.unwrap_or(cols), i.rows.unwrap_or(rows))
                    .map_err(|e| exec_err(format!("could not resize {id}: {e}")))?;
                let (cols, rows) = session.size();
                Ok(ToolOutput::text(format!(
                    "session: {id}\nsize: {cols}x{rows}\n"
                )))
            }
            Action::Kill => {
                let (id, session) = self.find(ctx, i.session.as_deref())?;
                session.kill();
                self.sessions.lock().remove(&id);
                tracing::info!(session = %id, "interactive session killed");
                Ok(ToolOutput::text(format!(
                    "session: {id}\nstate: failed\nreason: killed\n"
                )))
            }
            Action::List => {
                let map = self.sessions.lock();
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

    fn ctx(bot: &str) -> ToolContext {
        ToolContext {
            bot_id: Some(bot.into()),
            ..ToolContext::default()
        }
    }

    #[test]
    fn shells_interpreters_and_launchers_are_refused_however_they_are_spelled() {
        for bad in [
            "bash",
            "sh -i",
            "/bin/bash --login",
            "python3",
            "node",
            "FOO=1 sudo -i",
            "ls && ssh host",
            "echo hi | python",
            "\"/usr/bin/zsh\"",
            "powershell.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "env sh",
            "docker run -it ubuntu",
            "(bash)",
        ] {
            assert!(refused(bad).is_some(), "{bad}");
        }
        for fine in [
            "vibe --setup",
            "claude auth login",
            "codex login --device-auth",
            "npm install -g opencode-ai",
            "curl -fsSL https://example.com/install | tool",
            "./installer.sh",
            "FOO=1 ./run-setup",
        ] {
            assert_eq!(refused(fine), None, "{fine}");
        }
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

    #[test]
    fn answers_mask_secrets() {
        let secret = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
        let snap = Snapshot {
            lines: vec![format!("token {secret}")],
            prompt: format!("key sk-{}", "abcDEF0123456789".repeat(2)),
            state: State::Running,
            dropped: 0,
            more: false,
        };
        let shown = text(&answer("s", &snap));
        assert!(!shown.contains(&secret), "{shown}");
        assert!(!shown.contains("abcDEF0123456789"), "{shown}");
        assert!(shown.contains("[REDACTED]"), "{shown}");
    }

    #[tokio::test]
    async fn the_input_is_validated_before_anything_runs() {
        let t = tool(None);
        let c = ToolContext::default();
        for bad in [
            json!({"action":"start"}),
            json!({"action":"start","command":"   "}),
            json!({"action":"start","command":"bash"}),
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

    fn asker_command() -> (&'static str, &'static str) {
        if cfg!(windows) {
            (
                "$n = Read-Host 'Your name'; Write-Host \"hello $n\"; exit 3",
                "powershell",
            )
        } else {
            (
                "printf 'Your name: '; read n; echo \"hello $n\"; exit 3",
                "sh",
            )
        }
    }

    #[tokio::test]
    async fn a_session_belongs_to_the_bot_that_started_it() {
        let t = tool(None);
        let (command, shell) = asker_command();
        let started = t
            .call(
                &ctx("bot-a"),
                json!({"action":"start","command":command,"shell":shell,"wait_ms":10000}),
            )
            .await
            .unwrap();
        let shown = text(&started);
        let id = shown
            .lines()
            .find_map(|l| l.strip_prefix("session: "))
            .unwrap()
            .to_owned();
        let other = t
            .call(&ctx("bot-b"), json!({"action":"read","session":id}))
            .await;
        assert!(other.is_err(), "another bot must not reach the session");
        let listed = t
            .call(&ctx("bot-b"), json!({"action":"list"}))
            .await
            .unwrap();
        assert!(text(&listed).contains("no interactive sessions"));
        let killed = t
            .call(&ctx("bot-a"), json!({"action":"kill","session":id}))
            .await
            .unwrap();
        assert!(text(&killed).contains("killed"));
    }

    #[tokio::test]
    async fn at_most_four_sessions_are_open() {
        let t = tool(None);
        let (command, shell) = asker_command();
        let c = ctx("bot");
        let mut ids = Vec::new();
        for _ in 0..MAX_SESSIONS {
            let out = t
                .call(
                    &c,
                    json!({"action":"start","command":command,"shell":shell,"wait_ms":0}),
                )
                .await
                .unwrap();
            ids.push(
                text(&out)
                    .lines()
                    .find_map(|l| l.strip_prefix("session: "))
                    .unwrap()
                    .to_owned(),
            );
        }
        let fifth = t
            .call(
                &c,
                json!({"action":"start","command":command,"shell":shell,"wait_ms":0}),
            )
            .await;
        assert!(fifth.is_err());
        for id in ids {
            t.call(&c, json!({"action":"kill","session":id}))
                .await
                .unwrap();
        }
    }
}
