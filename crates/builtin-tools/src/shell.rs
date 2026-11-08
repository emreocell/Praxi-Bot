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

    // PowerShell startup on Windows runners exhausted the old timing budgets
    // even for echo commands. Keep these semantic checks from competing with
    // native lifetime fixtures; their timing assertions stay unchanged.
    #[cfg(windows)]
    static WINDOWS_SHELL_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
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
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
        let work = tempfile::tempdir().unwrap();
        let delay = Duration::from_secs(TIMEOUT_DESCENDANT_DELAY_SECS);
        // The parent stays in the foreground for the whole tool timeout. The
        // descendant delay is longer than that timeout, so the kill lands first.
        let fixture = spawn_delayed_marker(
            work.path(),
            delay,
            Duration::from_secs(BACKGROUND_LIFE_SECS),
        );
        let t = tool(work.path());
        let started = std::time::Instant::now();
        let ctx = ToolContext::default();
        let call = t.call(
            &ctx,
            json!({"command": fixture.command, "shell": fixture.shell, "cwd": fixture.dir, "timeout": TIMEOUT_KILL_SECS}),
        );
        tokio::pin!(call);
        // Readiness is the marker the child writes before sleeping, not a 1s budget.
        let mut ready_at = None;
        let result = loop {
            tokio::select! {
                biased;
                result = &mut call => break result,
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    if ready_at.is_none()
                        && fixture.ready.exists()
                        && let Some(pid) = recorded_pid(&fixture.child_pid)
                    {
                        ready_at = Some(started.elapsed());
                        // Task list can lag the pid file on a busy runner.
                        assert!(
                            became_alive(pid, Duration::from_secs(10)).await,
                            "descendant {pid} was not alive when the ready marker appeared"
                        );
                    }
                }
            }
        };
        if ready_at.is_none() && fixture.ready.exists() {
            ready_at = Some(started.elapsed());
        }
        let err = result.expect_err("shell succeeded instead of timing out");
        let message = err.to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(
            started.elapsed() < Duration::from_secs(TIMEOUT_KILL_SECS + 60),
            "timeout kill took {:?}: {message}",
            started.elapsed()
        );
        let ready_at = ready_at.unwrap_or_else(|| {
            panic!("descendant was not ready before timeout; fixture is inconclusive: {message}")
        });
        assert!(
            recorded_pid(&fixture.child_pid).is_some(),
            "descendant pid was not recorded; fixture is inconclusive: {message}"
        );
        // `delay` exceeds the tool timeout, so this deadline is after the write
        // the descendant would have made had the tree survived.
        assert_marker_absent_until(&fixture, started, ready_at + delay + MARKER_GRACE).await;
    }

    #[tokio::test]
    async fn large_output_is_capped_and_spilled() {
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
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
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
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
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
        let d = tempfile::tempdir().unwrap();
        let t = tool(d.path());
        // The child explicitly inherits both pipes on Windows and Unix, and
        // writes its marker only after `BACKGROUND_LIFE_SECS`. Returning before
        // that file exists, under `SHELL_RETURN_BOUND`, proves the tool did not
        // wait out the grandchild. Startup on a slow runner may itself exceed a
        // few seconds, so the bound is generous and the signal is the marker.
        let fixture = spawn_fixture(
            d.path(),
            Duration::from_secs(BACKGROUND_LIFE_SECS),
            Duration::ZERO,
            true,
            true,
        );
        let started = std::time::Instant::now();
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": fixture.command, "shell": fixture.shell, "cwd": fixture.dir, "timeout": SHELL_CALL_TIMEOUT_SECS}),
            )
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "shell_run did not finish while the grandchild was still running: {err}"
                )
            });
        let s = text(&out);
        assert!(s.contains("started-bg"), "{s}");
        assert!(
            started.elapsed() < SHELL_RETURN_BOUND,
            "shell_run waited out the background grandchild ({:?})",
            started.elapsed()
        );
        assert!(
            !fixture.marker.exists(),
            "background grandchild finished before shell_run returned"
        );
        assert!(fixture.ready.exists(), "background child never started");
        let pid = await_recorded_pid(&fixture.child_pid).await;
        assert!(
            became_alive(pid, READY_BOUND).await,
            "pipe-holding child {pid} was not running after shell_run returned"
        );
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

    /// Cold process start on a busy runner can take well over a minute.
    /// Tests wait for marker files until this cap instead of a fixed 1s budget.
    const READY_BOUND: Duration = Duration::from_secs(120);
    /// A background child stays up this long, so a slow-but-successful return
    /// still happens before it exits or writes its marker.
    const BACKGROUND_LIFE_SECS: u64 = 300;
    /// Upper bound for a `shell_run` that must return while that child is alive.
    const SHELL_RETURN_BOUND: Duration = Duration::from_secs(180);
    /// Tool timeout above `SHELL_RETURN_BOUND` and below `BACKGROUND_LIFE_SECS`,
    /// so waiting out the child fails and a slow start is not killed.
    const SHELL_CALL_TIMEOUT_SECS: u64 = 240;
    /// Tool timeout for the kill-on-timeout path. Longer than runner startup,
    /// shorter than the descendant's marker delay below.
    const TIMEOUT_KILL_SECS: u64 = 90;
    /// Descendant writes the leaked marker only after this delay, which is
    /// longer than `TIMEOUT_KILL_SECS`, so a timeout always lands first.
    const TIMEOUT_DESCENDANT_DELAY_SECS: u64 = 130;
    /// Cancel path: the future is dropped once the child is ready. The
    /// post-drop wait is this delay plus `MARKER_GRACE`.
    const CANCEL_DESCENDANT_DELAY_SECS: u64 = 10;
    /// Slack after the scheduled marker write so a late timer still fails the test.
    const MARKER_GRACE: Duration = Duration::from_secs(20);
    /// Positive control has to outlive a slow start plus the descendant delay.
    /// Parent sleep starts only after the child is ready, so this is longer
    /// than `READY_BOUND` + `TIMEOUT_DESCENDANT_DELAY_SECS`.
    const POSITIVE_CONTROL_TIMEOUT_SECS: u64 = 360;

    const FIXTURE_MODE: &str = "PRAXI_SHELL_TEST_FIXTURE";
    const FIXTURE_TEST: &str = "shell::tests::process_fixture";

    #[derive(serde::Serialize, serde::Deserialize)]
    struct FixtureConfig {
        child_delay_ms: u64,
        parent_delay_ms: u64,
        write_marker: bool,
        inherit_pipes: bool,
    }

    /// A subprocess entry point, selected with --exact by the real shell tool.
    /// The ordinary suite has no fixture mode and simply returns. Both OSes
    /// execute this same native parent/child protocol, so the descendant stays
    /// in the tool's process group / Windows job. Readiness is `ready.txt`,
    /// waited out to `READY_BOUND`, not a fixed one-second budget.
    #[test]
    fn process_fixture() {
        let Ok(mode) = std::env::var(FIXTURE_MODE) else {
            return;
        };
        let config: FixtureConfig =
            serde_json::from_slice(&std::fs::read("fixture.json").unwrap()).unwrap();
        if mode == "child" {
            std::fs::write("ready.txt", b"ready").unwrap();
            std::thread::sleep(Duration::from_millis(config.child_delay_ms));
            if config.write_marker {
                std::fs::write("marker.txt", b"survived").unwrap();
            }
            return;
        }
        assert_eq!(mode, "parent", "unknown fixture mode");
        std::fs::write("parent.pid", std::process::id().to_string()).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
            .env(FIXTURE_MODE, "child")
            .stdin(Stdio::null())
            .stdout(if config.inherit_pipes {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if config.inherit_pipes {
                Stdio::inherit()
            } else {
                Stdio::null()
            });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No breakaway flag: the child must inherit the tool's Windows job.
            command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
        }
        let mut child = command.spawn().unwrap();
        std::fs::write("child.pid", child.id().to_string()).unwrap();
        // Reap while this parent is alive, but intentionally do not join the
        // thread: the background controls require the parent to exit first.
        let _reaper = std::thread::spawn(move || child.wait().unwrap());
        // A successful parent return must witness an already-running child.
        // The deadline is the shared readiness cap, not a one-second budget.
        let ready_deadline = std::time::Instant::now() + READY_BOUND;
        while !Path::new("ready.txt").exists() {
            assert!(
                std::time::Instant::now() < ready_deadline,
                "fixture child did not acknowledge startup"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("started-bg");
        std::thread::sleep(Duration::from_millis(config.parent_delay_ms));
    }

    struct ProcessFixture {
        command: String,
        shell: &'static str,
        dir: PathBuf,
        marker: PathBuf,
        ready: PathBuf,
        child_pid: PathBuf,
        _cleanup: KillLater,
    }

    /// The same fixture serves the timeout, cancellation and positive control.
    /// The descendant acknowledges startup before its delayed write.
    /// Parent and descendant remain in the tool's process group / Windows job.
    /// `parent_delay` keeps the foreground parent alive past the tool timeout
    /// and, on the positive-control path, past the marker write.
    fn spawn_delayed_marker(dir: &Path, delay: Duration, parent_delay: Duration) -> ProcessFixture {
        spawn_fixture(dir, delay, parent_delay, true, true)
    }

    fn spawn_fixture(
        dir: &Path,
        delay: Duration,
        parent_delay: Duration,
        write_marker: bool,
        inherit_pipes: bool,
    ) -> ProcessFixture {
        let dir = dir.join("Öcel 引用's process tree");
        std::fs::create_dir(&dir).unwrap();
        // A fixed relative executable name keeps arbitrary paths out of cmd's
        // expansion rules. The actual executable and cwd still contain Unicode
        // and an apostrophe. Copy only when a cross-volume hard link cannot work.
        let executable = std::env::current_exe().unwrap();
        let fixture_executable = dir.join("shell-test-fixture.exe");
        if std::fs::hard_link(&executable, &fixture_executable).is_err() {
            std::fs::copy(&executable, &fixture_executable).unwrap();
        }
        let config = FixtureConfig {
            child_delay_ms: u64::try_from(delay.as_millis()).unwrap(),
            parent_delay_ms: u64::try_from(parent_delay.as_millis()).unwrap(),
            write_marker,
            inherit_pipes,
        };
        std::fs::write(
            dir.join("fixture.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let marker = dir.join("marker.txt");
        let ready = dir.join("ready.txt");
        let parent_pid = dir.join("parent.pid");
        let child_pid = dir.join("child.pid");
        let cleanup = KillLater {
            pidfiles: vec![child_pid.clone(), parent_pid.clone()],
            groupfile: Some(parent_pid.clone()),
        };
        // Set the mode inside the shell: production deliberately strips custom
        // variables from its inherited environment. Only the exact helper runs.
        let command = if cfg!(windows) {
            format!(
                "set \"{FIXTURE_MODE}=parent\" && .\\shell-test-fixture.exe \
                 --exact {FIXTURE_TEST} --nocapture --test-threads=1"
            )
        } else {
            format!(
                "{FIXTURE_MODE}=parent exec ./shell-test-fixture.exe \
                 --exact {FIXTURE_TEST} --nocapture --test-threads=1"
            )
        };
        ProcessFixture {
            command,
            shell: if cfg!(windows) { "cmd" } else { "sh" },
            dir,
            marker,
            ready,
            child_pid,
            _cleanup: cleanup,
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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn recorded_pid(path: &Path) -> Option<u32> {
        std::fs::read_to_string(path)
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 1)
    }

    struct KillLater {
        pidfiles: Vec<PathBuf>,
        groupfile: Option<PathBuf>,
    }

    impl Drop for KillLater {
        fn drop(&mut self) {
            // The group can outlive its parent. Kill it even when a regression
            // killed only that parent, then cover individually recorded PIDs.
            if cfg!(unix)
                && let Some(pid) = self.groupfile.as_deref().and_then(recorded_pid)
            {
                let _ = std::process::Command::new("sh")
                    .args(["-c", "kill -KILL \"-$1\"", "cleanup", &pid.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            for pidfile in &self.pidfiles {
                if let Some(pid) = recorded_pid(pidfile) {
                    kill_pid(pid);
                }
            }
        }
    }

    /// Poll until `due` (measured from `started`) and fail if the marker appears.
    /// The deadline is the descendant's scheduled write plus `MARKER_GRACE`, so
    /// a survivor cannot hide behind a short fixed sleep.
    async fn assert_marker_absent_until(
        fixture: &ProcessFixture,
        started: std::time::Instant,
        due: Duration,
    ) {
        loop {
            assert!(
                !fixture.marker.exists(),
                "descendant survived and wrote {}",
                fixture.marker.display()
            );
            if started.elapsed() >= due {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn delayed_marker_positive_control() {
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
        let work = tempfile::tempdir().unwrap();
        let delay = Duration::from_secs(TIMEOUT_DESCENDANT_DELAY_SECS);
        let fixture = spawn_delayed_marker(
            work.path(),
            delay,
            Duration::from_secs(BACKGROUND_LIFE_SECS),
        );
        let t = tool(work.path());
        let ctx = ToolContext::default();
        // The identical command, without a kill timeout, must write its marker.
        // The tool timeout only covers a slow start; it is longer than the delay.
        // Drop the still-running parent once the marker has been observed.
        {
            let call = t.call(
                &ctx,
                json!({"command": fixture.command, "shell": fixture.shell, "cwd": fixture.dir, "timeout": POSITIVE_CONTROL_TIMEOUT_SECS}),
            );
            tokio::pin!(call);
            let cap = READY_BOUND + delay + MARKER_GRACE;
            let written = tokio::time::timeout(cap, async {
                loop {
                    if std::fs::read_to_string(&fixture.marker).is_ok_and(|value| {
                        value.trim_start_matches('\u{feff}').trim() == "survived"
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            });
            tokio::select! {
                result = &mut call => panic!("positive-control parent exited early: {result:?}"),
                result = written => result.expect("positive-control marker was not written"),
            }
            assert!(
                fixture.ready.exists(),
                "descendant never acknowledged startup"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_future_kills_the_tree() {
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
        let work = tempfile::tempdir().unwrap();
        let delay = Duration::from_secs(CANCEL_DESCENDANT_DELAY_SECS);
        let fixture = spawn_delayed_marker(
            work.path(),
            delay,
            Duration::from_secs(BACKGROUND_LIFE_SECS),
        );
        let t = tool(work.path());
        let ctx = ToolContext::default();
        let started = std::time::Instant::now();
        let ready_at = {
            let call = t.call(
                &ctx,
                json!({"command": fixture.command, "shell": fixture.shell, "cwd": fixture.dir, "timeout": SHELL_CALL_TIMEOUT_SECS}),
            );
            tokio::pin!(call);
            // Cancel only once the child is running. A slow start must not
            // yield a vacuous green test, and the wait is the readiness cap.
            let ready_at = loop {
                tokio::select! {
                    biased;
                    result = &mut call => panic!("shell returned before cancellation: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(25)) => {
                        if fixture.ready.exists() {
                            break started.elapsed();
                        }
                        assert!(
                            started.elapsed() < READY_BOUND,
                            "descendant never acknowledged startup"
                        );
                    }
                }
            };
            assert!(
                tokio::time::timeout(Duration::from_millis(500), &mut call)
                    .await
                    .is_err(),
                "shell returned before its future could be cancelled"
            );
            ready_at
        }; // Drop the actual call future, not just a borrowed timeout wrapper.
        assert_marker_absent_until(&fixture, started, ready_at + delay + MARKER_GRACE).await;
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

    async fn await_recorded_pid(path: &Path) -> u32 {
        let started = std::time::Instant::now();
        loop {
            if let Some(pid) = recorded_pid(path) {
                return pid;
            }
            assert!(
                started.elapsed() < READY_BOUND,
                "pid file {} was not written",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn became_alive(pid: u32, bound: Duration) -> bool {
        let started = std::time::Instant::now();
        loop {
            if process_alive(pid) {
                return true;
            }
            if started.elapsed() >= bound {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `true` when `pid` is still alive at the end of `window`.
    /// A death at any poll is `false` (the child was killed).
    async fn stays_alive(pid: u32, window: Duration) -> bool {
        let started = std::time::Instant::now();
        loop {
            if !process_alive(pid) {
                return false;
            }
            if started.elapsed() >= window {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// A background child must still be running after a successful `shell_run`.
    ///
    /// Unix `ProcessGroupChild::wait` blocks until the group is empty, so a
    /// group wait turns this into a timeout that `killpg`s the child. Windows
    /// `KILL_ON_JOB_CLOSE` kills it when the job handle drops on return.
    #[tokio::test]
    async fn successful_shell_run_leaves_background_child_alive() {
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
        let work = tempfile::tempdir().unwrap();
        let spill = tempfile::tempdir().unwrap();
        let t = tool(spill.path());
        // Null child streams isolate successful-return job lifetime from the
        // separate inherited-pipe/grace test above. The marker is written only
        // after `BACKGROUND_LIFE_SECS`, so the signal that we did not wait the
        // child out is the missing file under a generous return bound.
        let fixture = spawn_fixture(
            work.path(),
            Duration::from_secs(BACKGROUND_LIFE_SECS),
            Duration::ZERO,
            true,
            false,
        );
        let started = std::time::Instant::now();
        let out = t
            .call(
                &ToolContext::default(),
                json!({"command": fixture.command, "shell": fixture.shell, "cwd": fixture.dir, "timeout": SHELL_CALL_TIMEOUT_SECS}),
            )
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "shell_run did not finish while the background child was still running: {err}"
                )
            });
        let s = text(&out);
        assert!(s.contains("exit_code: 0"), "{s}");
        assert!(s.contains("started-bg"), "{s}");
        assert!(
            started.elapsed() < SHELL_RETURN_BOUND,
            "shell_run waited out the background child ({:?})",
            started.elapsed()
        );
        assert!(
            !fixture.marker.exists(),
            "background child finished before shell_run returned"
        );
        assert!(fixture.ready.exists(), "background child never started");
        let pid = await_recorded_pid(&fixture.child_pid).await;
        // A job-close kill lands before `call` returns. The first wait absorbs a
        // task list that has not observed the new pid yet. The second covers a
        // kill that is only queued when the result is observed, including on a
        // slow runner where 500ms is not enough to see it.
        assert!(
            became_alive(pid, Duration::from_secs(10)).await,
            "background child {pid} was not running after a successful shell_run"
        );
        assert!(
            stays_alive(pid, Duration::from_secs(5)).await,
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
        #[cfg(windows)]
        let _serial = WINDOWS_SHELL_TESTS.lock().await;
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
