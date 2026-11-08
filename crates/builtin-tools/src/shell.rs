//! `shell_run` (permission category `Shell`).
//!
//! The command is handed to the shell binary as a single argument; it is never
//! assembled from separate user values. `raw_command` (the input's `command`)
//! is what the permission layer matches against the never-list.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use process_wrap::tokio::ChildWrapper;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
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

/// Names copied into a shell child. Vault, bearer and provider credentials are
/// not in this list, so they are never inherited. Nothing here is written back
/// to the daemon process. There is no tool input that grants extra names; a
/// secret would have to be added here on purpose.
const CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LOCALAPPDATA",
    "APPDATA",
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "USERNAME",
    "USER",
    "LOGNAME",
    "USERDOMAIN",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_COLLATE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "TERM",
];

fn child_env_allowed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    CHILD_ENV_ALLOWLIST.iter().any(|allowed| *allowed == upper)
}

/// Replace the child's environment with the allowlist, read from the daemon
/// without modifying it.
fn apply_child_env(cmd: &mut Command) {
    let kept: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| child_env_allowed(key))
        .collect();
    cmd.env_clear();
    for (key, value) in kept {
        cmd.env(key, value);
    }
}

/// Unix process group so `killpg` reaches the tree, Windows job object so
/// `TerminateJobObject` does. `KillOnDrop` is not registered: that flag is
/// what sets `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and closing the job on a
/// successful return would kill background children the shell had already
/// detached. Tree kill is `start_kill` while [`TreeGuard`] is armed.
fn isolate_child(cmd: Command) -> process_wrap::tokio::CommandWrap {
    let mut wrap = process_wrap::tokio::CommandWrap::from(cmd);
    #[cfg(unix)]
    {
        wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        wrap.wrap(process_wrap::tokio::CreationFlags(CREATE_NO_WINDOW));
        wrap.wrap(process_wrap::tokio::JobObject);
    }
    wrap
}

/// Owns the child and its stdout/stderr readers. Drop terminates the tree and
/// reaps it when the call is cancelled, times out, or fails.
struct TreeGuard {
    child: Option<Box<dyn ChildWrapper>>,
    readers: Vec<JoinHandle<()>>,
    /// When set, drop sends killpg / terminates the job before reaping.
    armed: bool,
}

impl TreeGuard {
    fn abort_readers(&mut self) {
        for task in self.readers.drain(..) {
            task.abort();
        }
    }

    async fn terminate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.armed = false;
        self.abort_readers();
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        for task in &self.readers {
            task.abort();
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Success already waited the direct child and disarmed. Dropping the
        // wrapper must not `killpg` / close a kill-on-close job, and must not
        // block in the process-group wait until background children exit.
        if !self.armed {
            return;
        }
        let _ = child.start_kill();
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
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
            .stderr(Stdio::piped());
        apply_child_env(&mut cmd);
        let mut child = isolate_child(cmd)
            .spawn()
            .map_err(|e| exec_err(format!("could not start {kind:?}: {e}")))?;
        let out_buf = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let err_buf = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let stdout = child.stdout().take().expect("piped stdout");
        let stderr = child.stderr().take().expect("piped stderr");
        let out_task = tokio::spawn(drain(stdout, Arc::clone(&out_buf)));
        let err_task = tokio::spawn(drain(stderr, Arc::clone(&err_buf)));
        let mut guard = TreeGuard {
            child: Some(child),
            readers: vec![out_task, err_task],
            armed: true,
        };

        // The outermost `wait` reaps the whole process group (Unix) or blocks
        // on the job. A shell that backgrounded a child would then sit until
        // that child exits or the tool timeout `killpg`s it. Wait the direct
        // child only; `terminate` / armed `Drop` still kill and reap the tree.
        let waited = tokio::time::timeout(timeout, async {
            guard
                .child
                .as_mut()
                .expect("child present")
                .inner_mut()
                .wait()
                .await
        })
        .await;
        let (code, note) = match waited {
            Ok(Ok(status)) => (status.code(), None),
            Ok(Err(e)) => {
                guard.terminate().await;
                return Err(exec_err(format!("wait failed: {e}")));
            }
            Err(_) => {
                guard.terminate().await;
                (
                    None,
                    Some(format!(
                        "timed out after {}s; the command was killed",
                        timeout.as_secs()
                    )),
                )
            }
        };
        // Grandchildren may keep the pipes open after the shell exits: wait a
        // grace period, then keep whatever was read. The guard owns the reader
        // tasks, so cancellation aborts them instead of detaching them.
        if note.is_none() {
            let grace = Duration::from_secs(2);
            let _ = tokio::time::timeout(grace, async {
                for task in &mut guard.readers {
                    let _ = (&mut *task).await;
                }
            })
            .await;
        }
        guard.abort_readers();
        // The direct child has exited. Disarm before drop so background
        // descendants are left running. Timeout, wait errors and a dropped
        // future leave `armed` set (`terminate` clears it after the tree kill).
        if note.is_none() {
            guard.armed = false;
        }
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

    fn ps_single(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "''"))
    }

    /// Path inside a single-quoted PowerShell string that itself sits inside
    /// another single-quoted string (`''path''` becomes `'path'`).
    fn ps_nested(path: &Path) -> String {
        format!("''{}''", path.display().to_string().replace('\'', "''"))
    }

    /// Parent stays in the foreground; a descendant writes `marker` after a delay.
    /// `spawned` is written once that descendant exists. `pidfile` is for cleanup.
    fn descendant_command(marker: &Path, spawned: &Path, pidfile: &Path) -> String {
        if cfg!(windows) {
            format!(
                "$psi = New-Object System.Diagnostics.ProcessStartInfo; \
                 $psi.FileName = Join-Path $env:SystemRoot 'System32\\WindowsPowerShell\\v1.0\\powershell.exe'; \
                 $psi.Arguments = '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 5; Set-Content -LiteralPath {marker} -Value leaked\"'; \
                 $psi.UseShellExecute = $false; \
                 $psi.CreateNoWindow = $true; \
                 $p = [System.Diagnostics.Process]::Start($psi); \
                 Set-Content -LiteralPath {pid} -Value $p.Id -Encoding Ascii; \
                 Set-Content -LiteralPath {spawned} -Value ok -Encoding Ascii; \
                 Start-Sleep -Seconds 60",
                marker = ps_nested(marker),
                pid = ps_single(pidfile),
                spawned = ps_single(spawned),
            )
        } else {
            format!(
                "(sleep 5; echo leaked > '{}') & echo $! > '{}' && echo ok > '{}' && wait",
                marker.display(),
                pidfile.display(),
                spawned.display(),
            )
        }
    }

    fn kill_pid(pid: u32) {
        let mut cmd = std::process::Command::new(if cfg!(windows) { "taskkill" } else { "kill" });
        if cfg!(windows) {
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
        } else {
            cmd.args(["-9", &pid.to_string()]);
        }
        let _ = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    struct KillLater {
        pidfile: PathBuf,
    }

    impl Drop for KillLater {
        fn drop(&mut self) {
            if let Ok(text) = std::fs::read_to_string(&self.pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                kill_pid(pid);
            }
        }
    }

    #[tokio::test]
    async fn timeout_and_cancel_kill_descendant_before_marker() {
        let work = tempfile::tempdir().unwrap();
        let spill = tempfile::tempdir().unwrap();
        let t = tool(spill.path());
        let marker = work.path().join("marker.txt");
        let spawned = work.path().join("spawned.txt");
        let pidfile = work.path().join("pid.txt");
        let _cleanup = KillLater {
            pidfile: pidfile.clone(),
        };
        let cmd = descendant_command(&marker, &spawned, &pidfile);

        let err = t
            .call(
                &ToolContext::default(),
                json!({"command": cmd, "shell": shell_for_tests(), "timeout": 1}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            spawned.exists(),
            "descendant was not started before the timeout, so the test is inconclusive: {err}"
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(
            !marker.exists(),
            "descendant wrote the marker after timeout"
        );

        let _ = std::fs::remove_file(&spawned);
        let _ = std::fs::remove_file(&pidfile);
        let marker = work.path().join("marker-cancel.txt");
        let spawned = work.path().join("spawned-cancel.txt");
        let pidfile = work.path().join("pid-cancel.txt");
        let _cleanup_cancel = KillLater {
            pidfile: pidfile.clone(),
        };
        let cmd = descendant_command(&marker, &spawned, &pidfile);
        let ctx = ToolContext::default();
        {
            let call = t.call(
                &ctx,
                json!({"command": cmd, "shell": shell_for_tests(), "timeout": 30}),
            );
            tokio::pin!(call);
            let started = std::time::Instant::now();
            loop {
                tokio::select! {
                    result = &mut call => panic!("shell returned before cancellation: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        if spawned.exists() {
                            break;
                        }
                        assert!(
                            started.elapsed() < Duration::from_secs(10),
                            "descendant was not started"
                        );
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(
            !marker.exists(),
            "descendant wrote the marker after the shell future was dropped"
        );
    }

    fn process_alive(pid: u32) -> bool {
        if cfg!(windows) {
            let output = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();
            let Ok(output) = output else {
                return false;
            };
            let text = String::from_utf8_lossy(&output.stdout);
            let want = pid.to_string();
            text.lines().any(|line| {
                line.split(',')
                    .nth(1)
                    .is_some_and(|col| col.trim().trim_matches('"') == want)
            })
        } else {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }
    }

    /// A background child must still be running after a successful `shell_run`.
    ///
    /// Unix `ProcessGroupChild::wait` blocks until the group is empty, so a
    /// group wait turns this into a timeout that `killpg`s the child. Windows
    /// `KILL_ON_JOB_CLOSE` kills it when the job handle drops on return.
    #[tokio::test]
    async fn successful_shell_run_leaves_background_child_alive() {
        let work = tempfile::tempdir().unwrap();
        let spill = tempfile::tempdir().unwrap();
        let t = tool(spill.path());
        let pidfile = work.path().join("pid.txt");
        let _cleanup = KillLater {
            pidfile: pidfile.clone(),
        };
        let cmd = if cfg!(windows) {
            format!(
                "$psi = New-Object System.Diagnostics.ProcessStartInfo; \
                 $psi.FileName = Join-Path $env:SystemRoot 'System32\\WindowsPowerShell\\v1.0\\powershell.exe'; \
                 $psi.Arguments = '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 30\"'; \
                 $psi.UseShellExecute = $false; \
                 $psi.CreateNoWindow = $true; \
                 $p = [System.Diagnostics.Process]::Start($psi); \
                 Set-Content -LiteralPath {pid} -Value $p.Id -Encoding Ascii; \
                 Write-Output started-bg",
                pid = ps_single(&pidfile),
            )
        } else {
            format!(
                "sleep 30 & echo $! > '{}' ; echo started-bg",
                pidfile.display()
            )
        };
        let started = std::time::Instant::now();
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": cmd, "shell": shell_for_tests(), "timeout": 15}),
            )
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("exit_code: 0"), "{s}");
        assert!(s.contains("started-bg"), "{s}");
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "shell_run waited out the background child ({:?})",
            started.elapsed()
        );
        let pid_text = std::fs::read_to_string(&pidfile).expect("background pid file");
        let pid = pid_text
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or_else(|_| panic!("pid file {pid_text:?}"));
        // A job-close kill lands before `call` returns. The pause covers a
        // kill that is only queued by the time the tool result is observed.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            process_alive(pid),
            "background child {pid} was killed after a successful shell_run"
        );
    }

    #[tokio::test]
    #[allow(unsafe_code)]
    async fn child_env_omits_secrets_but_keeps_path() {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::remove_var("BOT_SECRETS_PASSPHRASE");
                    std::env::remove_var("OPENAI_API_KEY");
                    std::env::remove_var("ANTHROPIC_API_KEY");
                    std::env::remove_var("BOT_API_BEARER");
                }
            }
        }
        let _restore = Restore;
        unsafe {
            std::env::set_var("BOT_SECRETS_PASSPHRASE", "sentinel-secret-value");
            std::env::set_var("OPENAI_API_KEY", "sk-provider-sentinel");
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-sentinel");
            std::env::set_var("BOT_API_BEARER", "Bearer-sentinel-token");
        }
        let parent_path = std::env::var("PATH").expect("PATH");
        let d = tempfile::tempdir().unwrap();
        let cmd = if cfg!(windows) {
            "Write-Output $env:PATH; Write-Output $env:BOT_SECRETS_PASSPHRASE; Write-Output $env:OPENAI_API_KEY; Write-Output $env:ANTHROPIC_API_KEY; Write-Output $env:BOT_API_BEARER"
        } else {
            "printf '%s\\n' \"$PATH\" \"$BOT_SECRETS_PASSPHRASE\" \"$OPENAI_API_KEY\" \"$ANTHROPIC_API_KEY\" \"$BOT_API_BEARER\""
        };
        let out = tool(d.path())
            .call(
                &ToolContext::default(),
                json!({"command": cmd, "shell": shell_for_tests()}),
            )
            .await
            .unwrap();
        let s = text(&out);
        assert!(
            s.contains(&parent_path),
            "PATH was not visible to the child:\n{s}"
        );
        assert!(!s.contains("sentinel-secret-value"), "{s}");
        assert!(!s.contains("sk-provider-sentinel"), "{s}");
        assert!(!s.contains("sk-ant-sentinel"), "{s}");
        assert!(!s.contains("Bearer-sentinel-token"), "{s}");
        assert_eq!(
            std::env::var("BOT_SECRETS_PASSPHRASE").unwrap(),
            "sentinel-secret-value",
            "daemon environment was mutated"
        );
    }
}
