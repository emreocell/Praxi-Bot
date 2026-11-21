//! Bounded waits for child processes the Linux desktop backend starts.
//!
//! The real `std::process::Child` adapter is Linux-only. Tests drive
//! [`wait_child`] with a fake child, so a hang is proven to kill and reap
//! without an X server or a live subprocess.

use std::io;
use std::thread;
use std::time::Duration;

/// What a wait ended with. `Killed` means the timeout path ran `kill` and then
/// `wait`, so the process is not left as a zombie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitEnd {
    /// The process exited on its own.
    Exited(i32),
    /// The deadline fired; the child was killed and reaped.
    Killed(i32),
    /// `try_wait`, `kill`, or `wait` failed.
    Io(String),
}

/// The three calls [`wait_child`] needs. Object-safe so tests can fake a hang.
pub trait ChildControl {
    /// `Ok(None)` while the child is still running.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the status cannot be read.
    fn try_wait(&mut self) -> io::Result<Option<i32>>;
    /// Ask the OS to stop the child.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the signal cannot be sent.
    fn kill(&mut self) -> io::Result<()>;
    /// Block until the child has been reaped.
    ///
    /// # Errors
    ///
    /// Returns the OS error when waiting fails.
    fn wait(&mut self) -> io::Result<i32>;
}

/// Poll `child` until it exits or `timeout` elapses. On timeout the child is
/// killed and waited on before this returns.
#[must_use]
pub fn wait_child(child: &mut dyn ChildControl, timeout: Duration, tick: Duration) -> WaitEnd {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(code)) => return WaitEnd::Exited(code),
            Ok(None) => {}
            Err(e) => return WaitEnd::Io(e.to_string()),
        }
        if start.elapsed() >= timeout {
            if let Err(e) = child.kill() {
                return WaitEnd::Io(e.to_string());
            }
            return match child.wait() {
                Ok(code) => WaitEnd::Killed(code),
                Err(e) => WaitEnd::Io(e.to_string()),
            };
        }
        let rest = timeout.saturating_sub(start.elapsed());
        thread::sleep(tick.min(rest.max(Duration::from_millis(1))));
    }
}

/// Text for a helper that never started or did not finish in time.
#[must_use]
pub fn explain_helper(program: &str, end: &WaitEnd, timeout: Duration) -> Option<String> {
    let secs = timeout.as_secs().max(1);
    match end {
        WaitEnd::Exited(0) => None,
        WaitEnd::Exited(code) => Some(format!(
            "helper `{program}` failed (exit {code}); the X server or the tool printed an error"
        )),
        WaitEnd::Killed(_) => Some(format!(
            "helper `{program}` timed out after {secs}s and was stopped (the child was killed and reaped, so it is not left as a zombie)"
        )),
        WaitEnd::Io(msg) => Some(format!("helper `{program}`: {msg}")),
    }
}

/// Why `Command::spawn` of a desktop program failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFail<'a> {
    /// The executable is not on `PATH` and not a file.
    NotFound,
    /// `spawn` did not return before the deadline.
    TimedOut { secs: u64 },
    /// Any other spawn error, already rendered.
    Other(&'a str),
}

/// Model-facing launch error. Names the program and the cause.
#[must_use]
pub fn explain_launch(program: &str, kind: LaunchFail<'_>) -> String {
    match kind {
        LaunchFail::NotFound => {
            format!("launch '{program}': program not found (the executable is missing from PATH)")
        }
        LaunchFail::TimedOut { secs } => {
            format!("launch '{program}': timed out after {secs}s waiting for the process to start")
        }
        LaunchFail::Other(msg) => format!("launch '{program}': {msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeChild {
        polls_left: u32,
        killed: bool,
        waited: bool,
    }

    impl ChildControl for FakeChild {
        fn try_wait(&mut self) -> io::Result<Option<i32>> {
            if self.polls_left == 0 {
                return Ok(Some(0));
            }
            self.polls_left -= 1;
            Ok(None)
        }

        fn kill(&mut self) -> io::Result<()> {
            self.killed = true;
            Ok(())
        }

        fn wait(&mut self) -> io::Result<i32> {
            self.waited = true;
            Ok(9)
        }
    }

    #[test]
    fn timeout_kills_and_reaps_so_the_child_is_not_a_zombie() {
        let mut child = FakeChild {
            polls_left: 100,
            killed: false,
            waited: false,
        };
        let end = wait_child(
            &mut child,
            Duration::from_millis(15),
            Duration::from_millis(5),
        );
        assert_eq!(end, WaitEnd::Killed(9));
        assert!(child.killed && child.waited);
        let msg = explain_helper("xdpyinfo", &end, Duration::from_secs(2)).unwrap();
        assert!(msg.contains("xdpyinfo"), "{msg}");
        assert!(msg.contains("timed out"), "{msg}");
        assert!(msg.contains("reaped"), "{msg}");
    }

    #[test]
    fn a_clean_exit_is_not_an_error_and_a_missing_program_is_named() {
        let mut child = FakeChild {
            polls_left: 0,
            killed: false,
            waited: false,
        };
        assert_eq!(
            wait_child(&mut child, Duration::from_secs(1), Duration::from_millis(1)),
            WaitEnd::Exited(0)
        );
        assert!(!child.killed);
        assert!(explain_helper("xdpyinfo", &WaitEnd::Exited(0), Duration::from_secs(1)).is_none());
        assert!(
            explain_launch("gedit", LaunchFail::NotFound).contains("program not found"),
            "{}",
            explain_launch("gedit", LaunchFail::NotFound)
        );
    }
}
