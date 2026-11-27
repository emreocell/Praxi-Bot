//! A program running on a pseudo-terminal.
//!
//! Some programs only work with a terminal: installers that ask questions, `claude auth login`,
//! `vibe --setup`. [`PtySession`] starts one on a PTY (`ConPTY` on Windows, `openpty` on Unix),
//! lets the caller type into it, and reads back what it printed as bounded plain text
//! ([`transcript::Transcript`]) or as the current screen (a `vt100` terminal emulator).
//!
//! Nothing here decides what a program may run: callers (the `shell_interactive` tool, the CLI
//! sign-in flows) check their own policy first and pass a fixed program and environment. Output
//! is returned as the program printed it; callers redact before it leaves their process.

#[cfg(windows)]
mod job;
pub mod transcript;
#[cfg(unix)]
mod tree;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::Notify;

use transcript::{Taken, Transcript};

/// Smallest and largest terminal a caller may ask for.
pub const MIN_COLS: u16 = 20;
pub const MAX_COLS: u16 = 300;
pub const MIN_ROWS: u16 = 5;
pub const MAX_ROWS: u16 = 100;
/// Most bytes of input one [`PtySession::write`] accepts.
pub const MAX_INPUT_BYTES: usize = 4096;
/// After the program exits, how long its last output may still arrive.
const EXIT_DRAIN: Duration = Duration::from_millis(300);
/// Writes (typed text and terminal replies) that may wait for the program to read them.
const WRITE_QUEUE: usize = 64;

type ChildHandle = Box<dyn portable_pty::Child + Send + Sync>;

/// What to run. `env` is the WHOLE environment: nothing is inherited.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub cols: u16,
    pub rows: u16,
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    /// The exit code; `None` when it was killed by a signal.
    pub code: Option<i32>,
    /// [`PtySession::kill`] was called (or the session was dropped) before it ended by itself.
    pub killed: bool,
}

/// Where a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Output arrived recently (or the caller did not wait long enough to know).
    Running,
    /// Alive and quiet: it is most likely waiting for the person or the bot to type.
    WaitingForInput,
    Exited(ExitInfo),
}

/// What one [`PtySession::read`] returns.
#[derive(Debug)]
pub struct Snapshot {
    /// Finished lines printed since the previous read.
    pub lines: Vec<String>,
    /// The unfinished last line (typically a prompt).
    pub prompt: String,
    pub state: State,
    /// Lines that were dropped unread because the transcript is bounded.
    pub dropped: u64,
    /// More finished lines are waiting (this read hit its byte limit).
    pub more: bool,
}

struct Shared {
    transcript: Transcript,
    screen: vt100::Parser,
    /// Bytes received so far (a change means new output).
    received: u64,
    /// `received` as of the last read.
    delivered_bytes: u64,
    /// Next absolute line a read returns.
    next_line: u64,
    last_output: Instant,
    exit: Option<(ExitInfo, Instant)>,
    reader_done: bool,
    /// The leader has been waited for: its process id may be handed to another program.
    reaped: bool,
}

/// A program on a PTY. Dropping it kills the program, and what it started.
pub struct PtySession {
    shared: Arc<Mutex<Shared>>,
    notify: Arc<Notify>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Typed text goes to a writer thread through this queue: a program that does not read its
    /// input blocks that thread, never the caller or the reader.
    input: SyncSender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    /// Everything the program started (Windows; see the `job` module).
    #[cfg(windows)]
    job: Option<job::Job>,
}

type SharedState = Arc<Mutex<Shared>>;

/// Feed everything the program prints into the transcript and the screen, and hand the answers
/// to its terminal queries to the writer thread. Nothing here ever waits for the program to read:
/// an answer that does not fit the queue is dropped (a program that floods queries and never
/// reads its input would otherwise stop this thread, and with it the output it is waiting on).
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    state: SharedState,
    notify: Arc<Notify>,
    replies: SyncSender<Vec<u8>>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            // The end of the previous read: a query can be cut in two between reads.
            let mut tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let mut sh = state.lock();
                        sh.transcript.feed(&buf[..count]);
                        sh.screen.process(&buf[..count]);
                        sh.received += count as u64;
                        sh.last_output = Instant::now();
                        tail.extend_from_slice(&buf[..count]);
                        let fresh = tail.len() - count.min(tail.len());
                        let answers = terminal_replies(&tail, fresh, sh.screen.screen());
                        tail.drain(..tail.len().saturating_sub(QUERY_OVERLAP));
                        drop(sh);
                        if !answers.is_empty() {
                            let _ = replies.try_send(answers.into_bytes());
                        }
                        notify.notify_one();
                    }
                }
            }
            state.lock().reader_done = true;
            notify.notify_one();
        })
        .map(|_| ())
}

/// Type what the queue delivers into the program, one piece at a time. Ends when the program no
/// longer takes input or the session is gone.
fn spawn_writer(mut writer: Box<dyn Write + Send>) -> std::io::Result<SyncSender<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
    std::thread::Builder::new()
        .name("pty-writer".into())
        .spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        })?;
    Ok(tx)
}

/// Record how the program ended. The child is taken out of `child` for the thread; when the
/// thread cannot be started it is put back, so the caller can still reap it.
fn spawn_waiter(
    child: &mut Option<ChildHandle>,
    state: SharedState,
    notify: Arc<Notify>,
) -> std::io::Result<()> {
    let slot = Arc::new(Mutex::new(child.take()));
    let theirs = Arc::clone(&slot);
    let started = std::thread::Builder::new()
        .name("pty-waiter".into())
        .spawn(move || {
            let Some(mut child) = theirs.lock().take() else {
                return;
            };
            // Linux: wait without reaping, then take down what the program left running while
            // its pid is still its own; `reaped` is set before the reap for the same reason.
            #[cfg(target_os = "linux")]
            if let Some(pid) = child.process_id()
                && tree::wait_without_reaping(pid)
            {
                tree::kill_tree(pid);
                state.lock().reaped = true;
            }
            let status = child.wait();
            let mut sh = state.lock();
            sh.reaped = true;
            let was_killed = sh.exit.is_some_and(|(e, _)| e.killed);
            let code = status
                .ok()
                .filter(|st| st.signal().is_none())
                .and_then(|st| i32::try_from(st.exit_code()).ok());
            sh.exit = Some((
                ExitInfo {
                    code,
                    killed: was_killed,
                },
                Instant::now(),
            ));
            drop(sh);
            notify.notify_one();
        });
    match started {
        Ok(_) => Ok(()),
        Err(e) => {
            *child = slot.lock().take();
            Err(e)
        }
    }
}

/// Answers to the questions a program asks its terminal. Windows' `ConPTY` does not print a thing
/// until its cursor-position query (`ESC [ 6 n`) is answered, and full-screen programs ask for
/// the cursor and the terminal type too. The reply to the first is the emulator's real cursor.
///
/// `data` is the last few bytes of the previous read followed by the new ones; `fresh` is where
/// the new ones start, so a query that ended in the previous read is not answered twice.
fn terminal_replies(data: &[u8], fresh: usize, screen: &vt100::Screen) -> String {
    let find = |needle: &[u8]| {
        data.windows(needle.len())
            .enumerate()
            .any(|(at, w)| w == needle && at + needle.len() > fresh)
    };
    let mut out = String::new();
    if find(b"[6n") {
        let (row, col) = screen.cursor_position();
        let _ = write!(out, "[{};{}R", row + 1, col + 1);
    }
    if find(b"[c") || find(b"[0c") {
        out.push_str("[?1;2c");
    }
    out
}

/// How much of a read is kept to catch a query split across two reads (one byte less than the
/// longest query, `ESC [ 0 c`, so a whole query is never seen twice).
const QUERY_OVERLAP: usize = 3;

/// The sizes are clamped to at least 1 before they get here.
fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).unwrap_or(std::num::NonZeroU16::MIN)
}

fn clamp_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(MIN_ROWS, MAX_ROWS),
        cols: cols.clamp(MIN_COLS, MAX_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Take down a program that was started but could not be wired up: kill what it started and
/// reap it, so that nothing is left running and no zombie remains.
fn abandon(child: Option<ChildHandle>, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        tree::kill_tree(pid);
    }
    #[cfg(not(unix))]
    let _ = pid;
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl PtySession {
    /// Start the program on a new PTY.
    ///
    /// # Errors
    /// The PTY could not be opened or the program could not be started.
    pub fn spawn(spec: &SpawnSpec) -> std::io::Result<Self> {
        let size = clamp_size(spec.cols, spec.rows);
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.env_clear();
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        if let Some(dir) = &spec.cwd {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // The child owns the slave side now; keeping ours open would stop EOF from ever arriving.
        drop(pair.slave);
        let pid = child.process_id();
        #[cfg(windows)]
        let job = child.as_raw_handle().and_then(|h| job::Job::around(h).ok());
        let killer = child.clone_killer();
        // From here a failure must not leave the program running or unreaped.
        let mut child = Some(child);
        Self::attach(pair.master, &mut child, killer, pid, size)
            .inspect_err(|_| abandon(child.take(), pid))
            .map(|mut session| {
                #[cfg(windows)]
                {
                    session.job = job;
                }
                #[cfg(not(windows))]
                let _ = &mut session;
                session
            })
    }

    /// Start the threads that read, type and wait for the program.
    fn attach(
        master: Box<dyn MasterPty + Send>,
        child: &mut Option<ChildHandle>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        pid: Option<u32>,
        size: PtySize,
    ) -> std::io::Result<Self> {
        let reader = master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let writer = master
            .take_writer()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let shared = Arc::new(Mutex::new(Shared {
            transcript: Transcript::new(),
            screen: vt100::Parser::new(nz(size.rows), nz(size.cols), 0),
            received: 0,
            delivered_bytes: 0,
            next_line: 0,
            last_output: Instant::now(),
            exit: None,
            reader_done: false,
            reaped: false,
        }));
        let notify = Arc::new(Notify::new());

        let input = spawn_writer(writer)?;
        spawn_reader(
            reader,
            Arc::clone(&shared),
            Arc::clone(&notify),
            input.clone(),
        )?;
        spawn_waiter(child, Arc::clone(&shared), Arc::clone(&notify))?;

        Ok(Self {
            shared,
            notify,
            master: Mutex::new(master),
            input,
            killer: Mutex::new(killer),
            pid,
            #[cfg(windows)]
            job: None,
        })
    }

    /// The operating-system process id, when the platform reports one.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// How it ended, once it has.
    #[must_use]
    pub fn exit(&self) -> Option<ExitInfo> {
        self.shared.lock().exit.map(|(e, _)| e)
    }

    /// Where the session stands right now, without reading (or consuming) any output: alive and
    /// quiet for `settle` is `WaitingForInput`, alive and recently printing is `Running`.
    #[must_use]
    pub fn state(&self, settle: Duration) -> State {
        let sh = self.shared.lock();
        match sh.exit {
            Some((info, _)) => State::Exited(info),
            None if sh.last_output.elapsed() >= settle => State::WaitingForInput,
            None => State::Running,
        }
    }

    /// Type bytes into the program (what a keyboard would send). The bytes are queued for a
    /// writer thread, so this never waits for the program to read them.
    ///
    /// # Errors
    /// The input is too long, the program is gone, or it is not reading its input and the queue
    /// of what it has not read yet is full.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "input too long",
            ));
        }
        if self.exit().is_some() {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        self.input.try_send(bytes.to_vec()).map_err(|e| match e {
            TrySendError::Full(_) => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "the program is not reading its input",
            ),
            TrySendError::Disconnected(_) => std::io::Error::from(std::io::ErrorKind::BrokenPipe),
        })
    }

    /// Change the terminal size.
    ///
    /// # Errors
    /// The PTY refused the new size.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let size = clamp_size(cols, rows);
        self.master
            .lock()
            .resize(size)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.shared
            .lock()
            .screen
            .screen_mut()
            .set_size(nz(size.rows), nz(size.cols));
        Ok(())
    }

    /// The current terminal size as `(cols, rows)`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.shared.lock().screen.screen().size();
        (cols.get(), rows.get())
    }

    /// Kill the program and everything it started (its session, its descendants; on Windows its
    /// job). Safe to call twice.
    pub fn kill(&self) {
        let mut sh = self.shared.lock();
        if sh.exit.is_none() {
            // Recorded first so the waiter thread reports `killed`.
            sh.exit = Some((
                ExitInfo {
                    code: None,
                    killed: true,
                },
                Instant::now(),
            ));
        }
        // Signals go out under the lock: the waiter marks the program reaped under it BEFORE it
        // reaps, so a pid that may already belong to somebody else is never signalled.
        #[cfg(unix)]
        if !sh.reaped {
            if let Some(pid) = self.pid {
                tree::kill_tree(pid);
            }
            let _ = self.killer.lock().kill();
        }
        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            if let Some(job) = &self.job {
                job.terminate();
            }
            let _ = self.killer.lock().kill();
        }
        drop(sh);
        self.notify.notify_one();
    }

    /// What is on the screen right now (the terminal emulator's view, blank rows trimmed).
    #[must_use]
    pub fn screen(&self) -> String {
        let sh = self.shared.lock();
        let text = sh.screen.screen().contents();
        text.trim_end().to_owned()
    }

    /// Everything the transcript still holds: every finished line followed by a newline, then
    /// the unfinished line (no newline), WITHOUT marking any of it as read. For callers that
    /// look for a link or a code in the output.
    #[must_use]
    pub fn text(&self) -> String {
        let sh = self.shared.lock();
        let mut out = String::new();
        for line in sh.transcript.take_lines(0, usize::MAX).lines {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str(&sh.transcript.prompt());
        out
    }

    /// Bytes received so far; a change means new output.
    #[must_use]
    pub fn received(&self) -> u64 {
        self.shared.lock().received
    }

    /// Wait until new output arrives, the program ends, or `timeout` passes.
    pub async fn changed(&self, since: u64, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let sh = self.shared.lock();
                if sh.received != since || sh.exit.is_some() {
                    return;
                }
            }
            if tokio::time::timeout_at(deadline, self.notify.notified())
                .await
                .is_err()
            {
                return;
            }
        }
    }

    /// Wait for output and return what the program printed since the previous read.
    ///
    /// Returns as soon as the program ended, or output arrived and then stayed quiet for
    /// `settle`, or `wait` passed. Output that arrived between two reads counts as new.
    /// At most `max_bytes` of text come back; `more` says there is further output waiting.
    pub async fn read(&self, wait: Duration, settle: Duration, max_bytes: usize) -> Snapshot {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let (undelivered, quiet, done) = {
                let sh = self.shared.lock();
                let drained = sh.exit.is_some_and(|(_, at)| at.elapsed() >= EXIT_DRAIN)
                    || (sh.exit.is_some() && sh.reader_done);
                (
                    sh.received != sh.delivered_bytes,
                    sh.last_output.elapsed(),
                    drained,
                )
            };
            if done || (undelivered && quiet >= settle) {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            // Wake for new output, or when the quiet period would be over.
            let nap = if undelivered {
                settle.saturating_sub(quiet).max(Duration::from_millis(10))
            } else {
                deadline - now
            };
            let _ = tokio::time::timeout(nap.min(deadline - now), self.notify.notified()).await;
        }
        self.snapshot(settle, max_bytes)
    }

    fn snapshot(&self, settle: Duration, max_bytes: usize) -> Snapshot {
        let mut sh = self.shared.lock();
        let Taken {
            lines,
            next,
            dropped,
            more,
        } = sh.transcript.take_lines(sh.next_line, max_bytes);
        sh.next_line = next;
        if !more {
            sh.delivered_bytes = sh.received;
        }
        let state = match sh.exit {
            Some((info, _)) => State::Exited(info),
            None if sh.last_output.elapsed() >= settle => State::WaitingForInput,
            None => State::Running,
        };
        Snapshot {
            lines,
            prompt: sh.transcript.prompt(),
            state,
            dropped,
            more,
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny interactive program: asks a question, reads one line, answers.
    fn asker() -> SpawnSpec {
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            (
                "powershell.exe",
                vec![
                    "-NoLogo",
                    "-NoProfile",
                    "-Command",
                    "$n = Read-Host 'Your name'; Write-Host \"hello $n\"; exit 7",
                ],
            )
        } else {
            (
                "sh",
                vec![
                    "-c",
                    "printf 'Your name: '; read n; echo \"hello $n\"; exit 7",
                ],
            )
        };
        SpawnSpec {
            program: program.into(),
            args: args.into_iter().map(OsString::from).collect(),
            cwd: None,
            env: ["PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "HOME", "TERM"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| ((*k).into(), v)))
                .collect(),
            cols: 100,
            rows: 30,
        }
    }

    /// How many one-second reads `read_until` may spend. Starting `powershell.exe` took 25-37 s on
    /// the GitHub `windows-latest` runner when it was loaded (main run 46010420448; 0.3 s on a
    /// developer PC), so 40 reads were not enough there. 180 keeps a margin of about 5x over the
    /// worst measured start. A program that never prints still fails, only later.
    const READ_BUDGET: usize = if cfg!(windows) { 180 } else { 40 };

    async fn read_until(session: &PtySession, needle: &str) -> String {
        let mut seen = String::new();
        for _ in 0..READ_BUDGET {
            let snap = session
                .read(
                    Duration::from_secs(1),
                    Duration::from_millis(200),
                    64 * 1024,
                )
                .await;
            seen.push_str(&snap.lines.join("\n"));
            seen.push('\n');
            seen.push_str(&snap.prompt);
            if seen.contains(needle) {
                return seen;
            }
            if matches!(snap.state, State::Exited(_)) {
                break;
            }
        }
        panic!(
            "never saw {needle:?}; got {seen:?} / screen {:?}",
            session.screen()
        );
    }

    #[tokio::test]
    async fn answers_a_question_and_reports_the_exit_code() {
        let session = PtySession::spawn(&asker()).expect("spawn");
        let first = read_until(&session, "Your name").await;
        assert!(first.contains("Your name"), "{first}");
        session.write(b"Ada\r").expect("write");
        let rest = read_until(&session, "hello Ada").await;
        assert!(rest.contains("hello Ada"), "{rest}");
        let mut exit = None;
        for _ in 0..20 {
            let snap = session
                .read(Duration::from_secs(1), Duration::from_millis(100), 1024)
                .await;
            if let State::Exited(info) = snap.state {
                exit = Some(info);
                break;
            }
        }
        let info = exit.expect("the program ended");
        assert_eq!(info.code, Some(7));
        assert!(!info.killed);
        // Typing into a finished program is refused, not silently dropped.
        assert!(session.write(b"x").is_err());
    }

    #[tokio::test]
    async fn kill_ends_a_waiting_program() {
        let session = PtySession::spawn(&asker()).expect("spawn");
        read_until(&session, "Your name").await;
        session.kill();
        session.kill();
        let snap = session
            .read(Duration::from_secs(2), Duration::from_millis(100), 1024)
            .await;
        match snap.state {
            State::Exited(info) => assert!(info.killed),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_quiet_program_is_waiting_for_input() {
        let session = PtySession::spawn(&asker()).expect("spawn");
        read_until(&session, "Your name").await;
        let snap = session
            .read(Duration::from_millis(600), Duration::from_millis(200), 1024)
            .await;
        assert_eq!(snap.state, State::WaitingForInput, "{snap:?}");
    }

    /// Is the process still around (a zombie that was never reaped counts as around)?
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    #[cfg(unix)]
    async fn gone(pid: u32) -> bool {
        for _ in 0..50 {
            if !alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[cfg(unix)]
    fn sh_spec(script: &str) -> SpawnSpec {
        let mut spec = asker();
        spec.program = "sh".into();
        spec.args = vec!["-c".into(), script.into()];
        spec
    }

    /// The leader and a helper it started, one of which shrugs off a hang-up (as `nohup` does).
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_takes_the_whole_tree_and_leaves_no_zombie() {
        let dir = std::env::temp_dir().join(format!("pty-session-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pids = dir.join("pids");
        let script = format!(
            "sleep 300 & echo $! > {p}; (trap '' HUP; exec sleep 301) & echo $! >> {p}; \
             echo ready; wait",
            p = pids.display()
        );
        let session = PtySession::spawn(&sh_spec(&script)).expect("spawn");
        read_until(&session, "ready").await;
        let leader = session.pid().expect("pid");
        let helpers: Vec<u32> = std::fs::read_to_string(&pids)
            .unwrap()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect();
        assert_eq!(helpers.len(), 2);
        assert!(alive(leader) && helpers.iter().all(|p| alive(*p)));
        session.kill();
        assert!(gone(leader).await, "the leader is reaped, not a zombie");
        for pid in &helpers {
            assert!(gone(*pid).await, "helper {pid} outlived the session");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A program that exits by itself is reaped too, and dropping the session kills what is left.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_finished_program_is_reaped_and_a_dropped_session_kills_the_rest() {
        let session = PtySession::spawn(&asker()).expect("spawn");
        read_until(&session, "Your name").await;
        let pid = session.pid().expect("pid");
        session.write(b"x\r").unwrap();
        assert!(gone(pid).await, "an exited program is reaped");
        let waiting = PtySession::spawn(&asker()).expect("spawn");
        read_until(&waiting, "Your name").await;
        let pid = waiting.pid().expect("pid");
        drop(waiting);
        assert!(gone(pid).await, "a dropped session leaves nothing running");
    }

    /// A terminal query can arrive in two reads; a full-screen program waits for the answer.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cursor_query_split_across_reads_is_still_answered() {
        let script = "stty raw -echo; printf '\\033['; sleep 0.4; printf '6n'; \
                      dd bs=1 count=6 2>/dev/null | od -c | head -1; echo done";
        let session = PtySession::spawn(&sh_spec(script)).expect("spawn");
        let seen = read_until(&session, "done").await;
        assert!(seen.contains("033") && seen.contains('R'), "{seen:?}");
    }

    /// A character that is cut in two by the read size is not turned into replacement signs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_multibyte_character_split_across_reads_stays_whole() {
        let script = "printf 'caf\\303'; sleep 0.4; printf '\\251 \\342\\202'; sleep 0.4; \
                      printf '\\254 ok\\n'; sleep 0.5";
        let session = PtySession::spawn(&sh_spec(script)).expect("spawn");
        let seen = read_until(&session, "ok").await;
        assert!(seen.contains("café €"), "{seen:?}");
    }

    /// A program that floods terminal queries and never reads its input must not stop the reader:
    /// the answers pile up unread (here: nobody drains the queue at all), and the output the
    /// program is waiting on has to keep being drained.
    #[test]
    fn the_reader_never_waits_for_the_program_to_take_an_answer() {
        struct Flood(usize);
        impl Read for Flood {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                self.0 -= 1;
                let query = b"\x1b[6n\x1b[c";
                buf[..query.len()].copy_from_slice(query);
                Ok(query.len())
            }
        }
        let shared = Arc::new(Mutex::new(Shared {
            transcript: Transcript::new(),
            screen: vt100::Parser::new(nz(24), nz(80), 0),
            received: 0,
            delivered_bytes: 0,
            next_line: 0,
            last_output: Instant::now(),
            exit: None,
            reader_done: false,
            reaped: false,
        }));
        // A queue of one that is never read: the second answer already has nowhere to go.
        let (answers, _unread) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        spawn_reader(
            Box::new(Flood(5_000)),
            Arc::clone(&shared),
            Arc::new(Notify::new()),
            answers,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.lock().reader_done {
            assert!(
                Instant::now() < deadline,
                "the reader is stuck on an answer"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.lock().received, 5_000 * 7);
    }

    /// A helper that started a session of its own is outside the leader's process group.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_also_takes_a_helper_that_started_its_own_session() {
        let dir = std::env::temp_dir().join(format!("pty-session-setsid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pids = dir.join("pids");
        let script = format!(
            "setsid sleep 302 & echo $! > {p}; echo ready; wait",
            p = pids.display()
        );
        let session = PtySession::spawn(&sh_spec(&script)).expect("spawn");
        read_until(&session, "ready").await;
        let helper: u32 = std::fs::read_to_string(&pids)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(helper));
        session.kill();
        assert!(gone(helper).await, "the helper outlived the session");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// When the program ends by itself, what it left running in its session goes too.
    #[cfg(unix)]
    #[tokio::test]
    async fn what_a_program_leaves_running_ends_with_it() {
        let dir = std::env::temp_dir().join(format!("pty-session-left-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pids = dir.join("pids");
        let script = format!(
            "(trap '' HUP; exec sleep 303) & echo $! > {p}; echo ready; exit 0",
            p = pids.display()
        );
        let session = PtySession::spawn(&sh_spec(&script)).expect("spawn");
        read_until(&session, "ready").await;
        let helper: u32 = std::fs::read_to_string(&pids)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let leader = session.pid().expect("pid");
        assert!(gone(leader).await, "the leader is reaped");
        assert!(gone(helper).await, "the helper outlived the program");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A program that was started but could not be wired up is killed and reaped.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_abandoned_program_is_killed_and_reaped() {
        let pair = native_pty_system().openpty(clamp_size(80, 24)).unwrap();
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("304");
        let child = pair.slave.spawn_command(cmd).unwrap();
        let pid = child.process_id();
        abandon(Some(child), pid);
        assert!(gone(pid.unwrap()).await, "killed and reaped, not a zombie");
    }

    /// A program that does not read its input fills the queue; the caller gets an error and is
    /// never blocked, and the session can still be killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn typing_into_a_program_that_never_reads_fails_fast_instead_of_blocking() {
        let session = PtySession::spawn(&sh_spec("echo up; sleep 30")).expect("spawn");
        read_until(&session, "up").await;
        let started = Instant::now();
        let mut refused = None;
        for _ in 0..400 {
            if let Err(e) = session.write(&[b'a'; MAX_INPUT_BYTES]) {
                refused = Some(e);
                break;
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2), "never waits");
        let refused = refused.expect("the queue fills up");
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
        let pid = session.pid().expect("pid");
        session.kill();
        assert!(gone(pid).await);
    }

    #[tokio::test]
    async fn input_and_size_are_bounded() {
        let session = PtySession::spawn(&asker()).expect("spawn");
        assert!(session.write(&vec![b'a'; MAX_INPUT_BYTES + 1]).is_err());
        session.resize(5000, 1).expect("resize clamps");
        assert_eq!(session.size(), (MAX_COLS, MIN_ROWS));
    }

    #[tokio::test]
    async fn a_program_that_does_not_exist_fails_to_start() {
        let mut spec = asker();
        spec.program = "definitely-not-a-program-praxi".into();
        spec.args.clear();
        // Some platforms refuse at spawn, others start and exit at once: both are "not running".
        match PtySession::spawn(&spec) {
            Err(_) => {}
            Ok(session) => {
                let snap = session
                    .read(Duration::from_secs(3), Duration::from_millis(100), 1024)
                    .await;
                assert!(matches!(snap.state, State::Exited(_)), "{snap:?}");
            }
        }
    }
}
