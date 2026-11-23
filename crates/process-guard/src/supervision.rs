//! Windows native boundary: non-inheritable jobs and owner-only stop events.
#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Child;

use windows::Win32::{
    Foundation::{HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0},
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, EqualSid,
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::{
            CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, INFINITE, OpenEventW, OpenProcess,
            OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, SetEvent,
            TerminateProcess, WaitForSingleObject,
        },
    },
};
use windows::core::{PCWSTR, PWSTR, w};

fn handle(owned: &OwnedHandle) -> HANDLE {
    HANDLE(owned.as_raw_handle())
}
fn own(raw: HANDLE) -> OwnedHandle {
    // SAFETY: callers transfer a newly created, valid, non-inherited handle exactly once.
    unsafe { OwnedHandle::from_raw_handle(raw.0) }
}

fn same_user(process: HANDLE) -> bool {
    fn user(process: HANDLE) -> Option<Vec<usize>> {
        let mut token = HANDLE::default();
        let mut bytes = 0;
        // SAFETY: query-only token, aligned output buffer sized by the first query.
        unsafe {
            OpenProcessToken(process, TOKEN_QUERY, &raw mut token).ok()?;
            let token = own(token);
            let _ = GetTokenInformation(handle(&token), TokenUser, None, 0, &raw mut bytes);
            let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
            GetTokenInformation(
                handle(&token),
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                bytes,
                &raw mut bytes,
            )
            .ok()?;
            Some(buffer)
        }
    }
    // SAFETY: each buffer contains TOKEN_USER followed by its SID; both live through comparison.
    unsafe {
        let Some(ours) = user(GetCurrentProcess()) else {
            return false;
        };
        let Some(theirs) = user(process) else {
            return false;
        };
        let ours = &*ours.as_ptr().cast::<TOKEN_USER>();
        let theirs = &*theirs.as_ptr().cast::<TOKEN_USER>();
        EqualSid(ours.User.Sid, theirs.User.Sid).is_ok()
    }
}

fn job_policy() -> windows::Win32::System::JobObjects::JOB_OBJECT_LIMIT {
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
}

/// Closing the last handle terminates every process in this job. No breakaway is allowed.
pub struct Job(OwnedHandle);
impl Job {
    /// Create an empty kill-on-close job before spawning a sidecar.
    /// # Errors
    /// The kernel refuses creation or configuration.
    pub fn new() -> io::Result<Self> {
        // SAFETY: no inherited handle, unnamed job, correctly sized information buffer.
        unsafe {
            let job = Self(own(
                CreateJobObjectW(None, PCWSTR::null()).map_err(io::Error::other)?
            ));
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = job_policy();
            SetInformationJobObject(
                handle(&job.0),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of_val(&info)).map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
            Ok(job)
        }
    }

    /// Assign before releasing the child's stdin startup handshake.
    /// # Errors
    /// Assignment failed; the caller must kill and reap the uncontained child.
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: the job and Child own live handles; Child's initial handle has assignment rights.
        unsafe { AssignProcessToJobObject(handle(&self.0), HANDLE(child.as_raw_handle())) }
            .map_err(io::Error::other)
    }

    /// Contain a standalone daemon and all its future children for its entire OS lifetime.
    /// # Errors
    /// The kernel refuses a job or assignment.
    pub fn contain_current_process() -> io::Result<()> {
        let job = Self::new()?;
        // SAFETY: pseudo-handle names only this process, before any children are spawned.
        unsafe { AssignProcessToJobObject(handle(&job.0), GetCurrentProcess()) }
            .map_err(io::Error::other)?;
        // Intentional: closing this job in a Rust destructor would kill this process before
        // it can return its exit code. The OS closes it on exit/crash and kills descendants.
        std::mem::forget(job);
        Ok(())
    }
}

fn event_name(pid: u32) -> Vec<u16> {
    format!("Local\\PraxiBot.Stop.{pid}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

/// Same-user/system only, even when the process has a permissive default DACL.
pub struct StopEvent(OwnedHandle);
impl StopEvent {
    /// Install before secrets/children. The event conveys no secrets or API authority.
    /// # Errors
    /// The kernel refuses the protected event.
    pub fn new() -> io::Result<Self> {
        let name = event_name(std::process::id());
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: static SDDL and valid out pointer. Descriptor is freed after CreateEvent copies it.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;OW)(A;;GA;;;SY)"),
                1,
                &raw mut descriptor,
                None,
            )
            .map_err(io::Error::other)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                    .map_err(io::Error::other)?,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let result = CreateEventW(
                Some(&raw const attributes),
                true,
                false,
                PCWSTR(name.as_ptr()),
            );
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            result.map(|h| Self(own(h))).map_err(io::Error::other)
        }
    }

    /// Wait on a dedicated thread; never block a runtime/UI thread.
    #[must_use]
    pub fn wait(&self) -> bool {
        // SAFETY: owned event stays live for the entire wait.
        unsafe { WaitForSingleObject(handle(&self.0), INFINITE) == WAIT_OBJECT_0 }
    }
}

/// Stop only binaries in this exact installation, holding process handles against PID reuse.
/// Used by the installer helper before replacing or deleting binaries. No image-name kills.
/// # Errors
/// A matched process cannot be stopped. The installer must abort, retaining user data.
pub fn stop_installation(directory: &Path) -> io::Result<()> {
    let expected = [directory.join("bot-desktop.exe"), directory.join("bot.exe")];
    let current = std::process::id();
    // SAFETY: snapshot enumeration writes a sized entry; each opened handle is owned once.
    unsafe {
        let snapshot =
            own(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(io::Error::other)?);
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).map_err(io::Error::other)?,
            ..Default::default()
        };
        let mut processes = Vec::new();
        let mut more = Process32FirstW(handle(&snapshot), &raw mut entry).is_ok();
        while more {
            if entry.th32ProcessID != current
                && let Ok(raw) = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                    false,
                    entry.th32ProcessID,
                )
            {
                let process = own(raw);
                let mut path = vec![0u16; 32768];
                let mut len = u32::try_from(path.len()).map_err(io::Error::other)?;
                if QueryFullProcessImageNameW(
                    handle(&process),
                    PROCESS_NAME_WIN32,
                    PWSTR(path.as_mut_ptr()),
                    &raw mut len,
                )
                .is_ok()
                {
                    let path = String::from_utf16_lossy(&path[..len as usize]);
                    if expected
                        .iter()
                        .any(|p| p.to_string_lossy().eq_ignore_ascii_case(&path))
                        && same_user(handle(&process))
                    {
                        let name = event_name(entry.th32ProcessID);
                        // A matching bot.exe may be a backup or another CLI command.
                        // Only daemon/shell instances install this event; never force
                        // terminate an unrecognised command that could be writing data.
                        let raw = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr()))
                            .map_err(|_| {
                                io::Error::other(
                                    "Close Praxi and any Praxi command windows, then try again.",
                                )
                            })?;
                        let event = own(raw);
                        SetEvent(handle(&event)).map_err(io::Error::other)?;
                        processes.push(process);
                    }
                }
            }
            more = Process32NextW(handle(&snapshot), &raw mut entry).is_ok();
        }
        // All matched processes were signalled before waiting. Desktop jobs and daemon jobs
        // clean the entire descendant tree on forced termination as well as ordinary exit.
        for process in processes {
            if WaitForSingleObject(handle(&process), 8000) != WAIT_OBJECT_0 {
                TerminateProcess(handle(&process), 1).map_err(io::Error::other)?;
                if WaitForSingleObject(handle(&process), 2000) != WAIT_OBJECT_0 {
                    return Err(io::Error::other(
                        "Praxi could not stop. Close Praxi and try again.",
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tree_policy_kills_on_close_and_does_not_allow_breakaway() {
        use windows::Win32::System::JobObjects::{
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        };
        let flags = job_policy();
        assert_eq!(
            flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
        assert_eq!(
            (flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)).0,
            0
        );
    }

    #[test]
    #[ignore = "Windows process-tree integration; run explicitly on an interactive Windows host"]
    fn closing_job_terminates_a_child() {
        let job = Job::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild-pid");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "supervision::tests::job_tree_fixture",
                "--nocapture",
            ])
            .env("PRAXI_JOB_FIXTURE", &pid_file)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        job.assign(&child).unwrap();
        child.stdin.take().unwrap().write_all(b"start\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // SAFETY: query/synchronize only; owned handle pins the exact grandchild process.
        let descendant = own(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.unwrap());
        assert!(
            child.try_wait().unwrap().is_none(),
            "fixture exited before job close"
        );
        drop(job);
        // SAFETY: Child owns a valid process handle.
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(child.as_raw_handle()), 2000) },
            WAIT_OBJECT_0
        );
        // SAFETY: descendant owns a valid process handle.
        assert_eq!(
            unsafe { WaitForSingleObject(handle(&descendant), 2000) },
            WAIT_OBJECT_0
        );
        // Kill-on-close may report exit code 0 on Windows. The live-before /
        // signalled-after handles prove tree termination; always reap the child.
        let _ = child.wait().unwrap();
    }

    #[test]
    fn job_tree_fixture() {
        let Some(path) = std::env::var_os("PRAXI_JOB_FIXTURE") else {
            return;
        };
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        if line.trim() != "start" {
            return;
        }
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        std::fs::write(path, child.id().to_string()).unwrap();
        let _ = child.wait();
    }
}
