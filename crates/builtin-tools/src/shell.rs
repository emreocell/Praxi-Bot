//! `shell_run` (permission category `Shell`).
//!
//! The command is handed to the shell binary as a single argument; it is never
//! assembled from separate user values. `raw_command` (the input's `command`)
//! is what the permission layer matches against the never-list.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, finish_text, parse, spec};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
/// Bytes kept per stream; the rest is read and dropped.
const HARD_CAPTURE_BYTES: usize = 32 * 1024 * 1024;

/// Which shell binary runs the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    /// Windows PowerShell.
    Powershell,
    /// `cmd.exe`.
    Cmd,
    /// `bash -c`.
    Bash,
    /// `sh -c`.
    Sh,
}

impl ShellKind {
    fn default_for_host() -> Self {
        if cfg!(windows) {
            Self::Powershell
        } else {
            Self::Sh
        }
    }
}

#[derive(Deserialize)]
struct Input {
    command: String,
    shell: Option<ShellKind>,
    cwd: Option<String>,
    timeout: Option<u64>,
}

/// The `shell_run` tool.
pub struct ShellTool {
    spec: ToolSpec,
    spill_dir: PathBuf,
}

/// Build the shell tool; `data_dir` hosts spill files.
#[must_use]
pub fn tool(data_dir: &Path) -> Arc<dyn Tool> {
    Arc::new(ShellTool {
        spec: spec(
            "shell_run",
            "Run one shell command and return its exit code, stdout and stderr. Example: {\"command\":\"dir\",\"shell\":\"cmd\",\"timeout\":30}",
            json!({"type":"object","properties":{
                "command":{"type":"string","description":"The command line, passed to the shell as one argument."},
                "shell":{"type":"string","enum":["powershell","cmd","bash","sh"],"description":"Default: powershell on Windows, sh elsewhere."},
                "cwd":{"type":"string","description":"Working directory."},
                "timeout":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_SECS,"description":"Seconds before the command is killed (default 60)."}
            },"required":["command"]}),
            Effect::Destructive,
            PermissionCategory::Shell,
        ),
        spill_dir: data_dir.join("spill"),
    })
}

fn build_command(kind: ShellKind, command: &str) -> Command {
    match kind {
        ShellKind::Powershell => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            // Constant prefix forces UTF-8 output; the user command stays one argument.
            c.arg(format!(
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            ));
            c
        }
        ShellKind::Cmd => {
            let mut c = Command::new("cmd.exe");
            #[cfg(windows)]
            {
                // cmd does not parse its command line like other programs, so the
                // single command argument is passed verbatim.
                c.raw_arg(format!("/d /s /c \"{command}\""));
            }
            #[cfg(not(windows))]
            {
                c.args(["/c", command]);
            }
            c
        }
        ShellKind::Bash => {
            let mut c = Command::new("bash");
            c.args(["-c", command]);
            c
        }
        ShellKind::Sh => {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        }
    }
}

/// Read `r` to EOF into a shared buffer (capped at `HARD_CAPTURE_BYTES`).
///
/// The buffer is shared so partial output survives when the task is aborted
/// because a background grandchild keeps the pipe open.
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut r: R, kept: Arc<parking_lot::Mutex<Vec<u8>>>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut k = kept.lock();
                if k.len() < HARD_CAPTURE_BYTES {
                    let take = n.min(HARD_CAPTURE_BYTES - k.len());
                    k.extend_from_slice(&buf[..take]);
                }
            }
        }
    }
}

fn format_output(code: Option<i32>, stdout: &[u8], stderr: &[u8], note: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(n) = note {
        s.push_str(n);
        s.push('\n');
    }
    match code {
        Some(c) => {
            let _ = writeln!(s, "exit_code: {c}");
        }
        None => s.push_str("exit_code: none\n"),
    }
    s.push_str("--- stdout ---\n");
    s.push_str(&String::from_utf8_lossy(stdout));
    s.push_str("\n--- stderr ---\n");
    s.push_str(&String::from_utf8_lossy(stderr));
    s
}

#[async_trait]
impl Tool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input = parse(input)?;
        if i.command.trim().is_empty() {
            return Err(ToolError::InvalidInput("command is empty".into()));
        }
        let kind = i.shell.unwrap_or_else(ShellKind::default_for_host);
        let timeout = Duration::from_secs(
            i.timeout
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS),
        );
        let mut cmd = build_command(kind, &i.command);
        if let Some(cwd) = &i.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| exec_err(format!("could not start {kind:?}: {e}")))?;
        let out_buf = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let err_buf = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut out_task = tokio::spawn(drain(
            child.stdout.take().expect("piped stdout"),
            out_buf.clone(),
        ));
        let mut err_task = tokio::spawn(drain(
            child.stderr.take().expect("piped stderr"),
            err_buf.clone(),
        ));

        let waited = tokio::time::timeout(timeout, child.wait()).await;
        let (code, note) = match waited {
            Ok(Ok(status)) => (status.code(), None),
            Ok(Err(e)) => return Err(exec_err(format!("wait failed: {e}"))),
            Err(_) => {
                let _ = child.kill().await;
                (
                    None,
                    Some(format!(
                        "timed out after {}s; the command was killed",
                        timeout.as_secs()
                    )),
                )
            }
        };
        // Grandchildren may keep the pipes open after the shell exits or is
        // killed: wait a grace period, then keep whatever was read and stop the
        // reader tasks so they do not accumulate output in the background.
        let grace = Duration::from_secs(2);
        let _ = tokio::time::timeout(grace, async {
            let _ = (&mut out_task).await;
            let _ = (&mut err_task).await;
        })
        .await;
        out_task.abort();
        err_task.abort();
        let stdout = std::mem::take(&mut *out_buf.lock());
        let stderr = std::mem::take(&mut *err_buf.lock());
        let text = format_output(code, &stdout, &stderr, note.as_deref());
        if let Some(n) = note {
            let out = finish_text(ctx, &self.spill_dir, text);
            let shown = match out.content.first() {
                Some(tool_registry::ContentPart::Text { text }) => text.clone(),
                _ => String::new(),
            };
            return Err(exec_err(format!("{n}. Partial output:\n{shown}")));
        }
        Ok(finish_text(ctx, &self.spill_dir, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tool_registry::ContentPart;

    fn shell_for_tests() -> &'static str {
        if cfg!(windows) { "powershell" } else { "sh" }
    }

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn runs_command_and_reports_exit_code() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": "echo hello-shell", "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("exit_code: 0"), "{s}");
        assert!(s.contains("hello-shell"), "{s}");

        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": "exit 3", "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        assert!(text(&out).contains("exit_code: 3"));
    }

    #[tokio::test]
    async fn timeout_kills_the_command() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        let sleeper = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let started = std::time::Instant::now();
        let err = t
            .call(
                &ToolContext::default(),
                json!({"command": sleeper, "shell": shell_for_tests(), "timeout": 1}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[tokio::test]
    async fn large_output_is_capped_and_spilled() {
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        let gen_cmd = if cfg!(windows) {
            "'a' * 100000"
        } else {
            "head -c 100000 /dev/zero | tr '\\0' a"
        };
        let ctx = ToolContext {
            run_id: "r".into(),
            max_inline_bytes: 1000,
            bot_id: None,
        };
        let out = t
            .call(
                &ctx,
                json!({"command": gen_cmd, "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        let spill = out.spill.clone().expect("spilled");
        assert!(spill.size_bytes >= 100_000);
        assert!(std::fs::metadata(&spill.path).unwrap().len() >= 100_000);
        assert!(text(&out).len() < 2000);
    }

    #[tokio::test]
    async fn command_is_a_single_argument_no_injection_of_extra_args() {
        // Quotes and metacharacters inside the command stay inside it.
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        let cmd = if cfg!(windows) {
            "Write-Output 'a \"quoted\" value'"
        } else {
            "echo 'a \"quoted\" value'"
        };
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": cmd, "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        assert!(text(&out).contains("a \"quoted\" value"), "{}", text(&out));
    }

    #[tokio::test]
    async fn background_grandchild_holding_pipe_keeps_partial_output() {
        // NOTE: only a true regression test on Unix (`sleep 8 &` inherits the pipe).
        // On Windows `Start-Process -NoNewWindow` may not inherit the handles, so
        // this can pass even without the drain/grace fix.
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        let cmd = if cfg!(windows) {
            "Start-Process -NoNewWindow powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 8'; echo started-bg"
        } else {
            "sleep 8 & echo started-bg"
        };
        let started = std::time::Instant::now();
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": cmd, "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("started-bg"), "{s}");
        assert!(started.elapsed() < Duration::from_secs(7));
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let err = tool(d.path())
            .call(&ToolContext::default(), json!({"command": "  "}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
