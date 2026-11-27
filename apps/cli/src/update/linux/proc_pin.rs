//! Processes pinned through their `/proc/<pid>` directory.
//!
//! A pid is only a number: once its process exits, the number can name another process. A descriptor
//! opened on `/proc/<pid>` stays bound to the process it was opened for. Reads through it fail once
//! that process is gone; they never turn into reads about whoever received the number next.
use rustix::{
    fs::{Mode, OFlags},
    process::{Pid, PidfdFlags, Signal},
};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::OwnedFd,
    path::Path,
    time::{Duration, Instant},
};

/// `status`, `stat` and `cgroup` are a few hundred bytes; anything near this is not them.
const LIMIT: u64 = 64 * 1024;

pub(super) struct Proc {
    dir: OwnedFd,
    pid: u32,
}

impl Proc {
    /// Pin the process that has `pid` right now. `root` is `/proc` (a directory of the same shape in
    /// tests).
    pub(super) fn open(root: &Path, pid: u32) -> io::Result<Self> {
        let dir = rustix::fs::open(
            root.join(pid.to_string()),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self { dir, pid })
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    fn read(&self, name: &str) -> io::Result<String> {
        let fd = rustix::fs::openat(
            &self.dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut text = String::new();
        File::from(fd).take(LIMIT).read_to_string(&mut text)?;
        Ok(text)
    }

    pub(super) fn status(&self) -> io::Result<String> {
        self.read("status")
    }

    /// Control groups of the process, read through the pinned directory.
    pub(super) fn cgroup(&self) -> io::Result<String> {
        self.read("cgroup")
    }

    /// The effective user.
    pub(super) fn uid(&self) -> io::Result<u32> {
        super::status_uid(&self.status()?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no user in status"))
    }

    /// When the process started, in clock ticks since boot (comparable between processes).
    pub(super) fn start_time(&self) -> io::Result<u64> {
        parse_start_time(&self.read("stat")?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no start time in stat"))
    }
}

/// Field 22 of `/proc/<pid>/stat`. The command name (field 2) may hold spaces and brackets, so the
/// fields are counted from the last `)`.
pub(super) fn parse_start_time(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // `rest` begins at field 3 (the state), so field 22 is the 20th.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// The real, effective, saved and file-system user of a process, from `status`.
fn status_uids(status: &str) -> Option<[u32; 4]> {
    let mut ids = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .map(str::parse::<u32>);
    Some([
        ids.next()?.ok()?,
        ids.next()?.ok()?,
        ids.next()?.ok()?,
        ids.next()?.ok()?,
    ])
}

fn is_zombie(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .is_some_and(|state| state.trim_start().starts_with('Z'))
}

/// Processes that run as `uid` in any of its four roles (a zombie holds nothing and is skipped).
pub(super) fn processes_of(root: &Path, uid: u32) -> io::Result<Vec<Proc>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that is gone by now is no problem.
        let Ok(process) = Proc::open(root, pid) else {
            continue;
        };
        let Ok(status) = process.status() else {
            continue;
        };
        if !is_zombie(&status) && status_uids(&status).is_some_and(|ids| ids.contains(&uid)) {
            found.push(process);
        }
    }
    Ok(found)
}

/// `SIGKILL` the process behind `process`, provided it still runs as `uid`. Never a process that only
/// has the same number, and never one of root or of the updater's own user.
pub(super) fn kill(process: &Proc, uid: u32) -> io::Result<()> {
    kill_unless_own(process, uid, rustix::process::geteuid().as_raw())
}

fn kill_unless_own(process: &Proc, uid: u32, own: u32) -> io::Result<()> {
    if uid == 0 || uid == own {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "never signals the processes of root or of the updater's own user",
        ));
    }
    // `None`: the process is gone (the pinned directory no longer reads).
    let runs_as_user = |process: &Proc| {
        process
            .status()
            .ok()
            .map(|status| status_uids(&status).is_some_and(|ids| ids.contains(&uid)))
    };
    match runs_as_user(process) {
        None => return Ok(()),
        Some(false) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the process does not run as the service user",
            ));
        }
        Some(true) => {}
    }
    let pid = i32::try_from(process.pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let handle = rustix::process::pidfd_open(pid, PidfdFlags::empty())?;
    // The handle names whoever has the number now. The pinned directory still reading proves that
    // this is the process that was checked: a number is not reused while its process exists.
    if runs_as_user(process) != Some(true) {
        return Ok(());
    }
    Ok(rustix::process::pidfd_send_signal(&handle, Signal::KILL)?)
}

/// Make sure no process of `uid` is left: the service user can write the data directory, so root
/// must not work in it while one runs. Stragglers (outside the stopped units' control groups) are
/// killed; this fails if any survive `wait`.
pub(super) async fn quiet_user(
    root: &Path,
    uid: u32,
    kill: impl Fn(&Proc, u32) -> io::Result<()>,
    wait: Duration,
) -> crate::Result<()> {
    if uid == 0 {
        return Err("root cannot be the service user".into());
    }
    let deadline = Instant::now() + wait;
    loop {
        let alive = processes_of(root, uid)?;
        if alive.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} process(es) of the service user are still running after the stop; nothing was changed",
                alive.len()
            )
            .into());
        }
        for process in &alive {
            if let Err(error) = kill(process, uid) {
                eprintln!(
                    "Praxi update: cannot stop process {}: {error}",
                    process.pid()
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, fs, path::PathBuf};

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn fake(root: &Path, pid: u32, uid: u32, state: char, started: u64) {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("status"),
            format!("Name:\tx\nState:\t{state} (x)\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
        fs::write(
            dir.join("stat"),
            format!("{pid} (a b) c) {state} 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {started} 0 0\n"),
        )
        .unwrap();
        fs::write(dir.join("cgroup"), "0::/system.slice/praxi.service\n").unwrap();
    }

    #[test]
    fn the_start_time_is_counted_from_the_last_bracket() {
        assert_eq!(
            parse_start_time("42 (bot) S 1 42 42 0 -1 4194560 100 0 0 0 5 6 0 0 20 0 3 0 777 1 2"),
            Some(777)
        );
        // A name with spaces and brackets must not shift the fields.
        assert_eq!(
            parse_start_time("42 (a) b (c) S 1 42 42 0 -1 0 0 0 0 0 0 0 0 0 20 0 3 0 888 1 2"),
            Some(888)
        );
        assert_eq!(parse_start_time("42 (bot) S 1 2"), None);
        assert_eq!(parse_start_time("garbage"), None);
    }

    #[test]
    fn the_pinned_directory_keeps_describing_the_process_it_was_opened_for() {
        let proc = tempfile::tempdir().unwrap();
        fake(proc.path(), 123, 999, 'S', 500);
        let pinned = Proc::open(proc.path(), 123).unwrap();
        // The process exits and a stranger gets the same number: another user, another start.
        fs::rename(proc.path().join("123"), proc.path().join("gone")).unwrap();
        fake(proc.path(), 123, 0, 'S', 9000);
        assert_eq!(pinned.uid().unwrap(), 999);
        assert_eq!(pinned.start_time().unwrap(), 500);
        assert!(pinned.cgroup().unwrap().contains("praxi.service"));
        // A fresh pin by number sees the stranger: that is exactly what pinning early avoids.
        let by_number = Proc::open(proc.path(), 123).unwrap();
        assert_eq!(by_number.uid().unwrap(), 0);
        assert_eq!(by_number.start_time().unwrap(), 9000);
    }

    #[test]
    fn a_real_process_is_pinned_and_reads_fail_once_it_is_gone() {
        let own = Proc::open(Path::new("/proc"), std::process::id()).unwrap();
        assert_eq!(own.uid().unwrap(), me());
        assert!(own.start_time().unwrap() > 0);
        assert_ne!(own.cgroup().unwrap(), "");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pinned = Proc::open(Path::new("/proc"), child.id()).unwrap();
        assert_eq!(pinned.uid().unwrap(), me());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            pinned.status().is_err(),
            "a gone process must not be readable"
        );
        assert!(pinned.start_time().is_err());
    }

    #[test]
    fn user_processes_are_found_by_any_of_their_ids_and_zombies_are_not_stragglers() {
        let proc = tempfile::tempdir().unwrap();
        fake(proc.path(), 10, 999, 'S', 1);
        fake(proc.path(), 11, 999, 'Z', 1);
        fake(proc.path(), 12, 1000, 'S', 1);
        // Real user 1000 but effective 999 (a setuid helper): still the service user's.
        fake(proc.path(), 13, 1000, 'S', 1);
        fs::write(
            proc.path().join("13/status"),
            "State:\tS (x)\nUid:\t1000\t999\t1000\t999\n",
        )
        .unwrap();
        fs::write(proc.path().join("self"), "not a process").unwrap();
        let mut found: Vec<u32> = processes_of(proc.path(), 999)
            .unwrap()
            .iter()
            .map(Proc::pid)
            .collect();
        found.sort_unstable();
        assert_eq!(found, [10, 13]);
    }

    #[tokio::test]
    async fn a_quiet_service_user_passes_and_stragglers_are_stopped_first() {
        let proc = tempfile::tempdir().unwrap();
        let calls = RefCell::new(Vec::new());
        let root: PathBuf = proc.path().to_path_buf();
        // Nothing runs as the user: nothing is killed.
        fake(proc.path(), 12, 1000, 'S', 1);
        quiet_user(
            &root,
            999,
            |p, _| {
                calls.borrow_mut().push(p.pid());
                Ok(())
            },
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(calls.borrow().is_empty());
        // A straggler is killed (here: the "kill" removes its directory) and the check passes.
        fake(proc.path(), 10, 999, 'S', 1);
        quiet_user(
            &root,
            999,
            |p, uid| {
                assert_eq!(uid, 999);
                calls.borrow_mut().push(p.pid());
                fs::remove_dir_all(root.join(p.pid().to_string()))
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(*calls.borrow(), [10]);
        assert!(
            root.join("12").exists(),
            "other users' processes are not touched"
        );
    }

    #[tokio::test]
    async fn a_straggler_that_survives_aborts_and_root_is_never_the_service_user() {
        let proc = tempfile::tempdir().unwrap();
        fake(proc.path(), 10, 999, 'S', 1);
        let error = quiet_user(proc.path(), 999, |_, _| Ok(()), Duration::from_millis(250))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("still running"), "{error}");
        assert!(
            quiet_user(proc.path(), 0, |_, _| Ok(()), Duration::from_millis(10))
                .await
                .is_err()
        );
    }

    #[test]
    fn only_a_process_of_the_named_user_is_killed() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pinned = Proc::open(Path::new("/proc"), child.id()).unwrap();
        // The updater pretends to be somebody else, so that this test's own user is "the service user".
        let other = me() + 1;
        let error = kill_unless_own(&pinned, me() + 2, other).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            child.try_wait().unwrap().is_none(),
            "the wrong user must not kill it"
        );
        // Root's and the updater's own processes are never signalled, whatever the caller says.
        for (uid, own) in [(0, other), (other, other)] {
            let error = kill_unless_own(&pinned, uid, own).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert!(
            kill(&pinned, me()).is_err(),
            "the test's own user is the updater's user"
        );
        assert!(child.try_wait().unwrap().is_none());
        kill_unless_own(&pinned, me(), other).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        // A process that is already gone is not an error, and nothing else is signalled.
        kill_unless_own(&pinned, me(), other).unwrap();
    }
}
