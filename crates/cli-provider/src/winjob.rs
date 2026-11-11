//! Kill-on-close job for one Windows turn. The handle is the same object
//! `isolate` assigns the child to, and the bridge accepts a socket only when
//! its owner is inside this job.
#![allow(unsafe_code)]

use std::io::{Error, Result};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::{Arc, Mutex};

use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper};
use windows::Win32::{
    Foundation::{CloseHandle, DUPLICATE_HANDLE_OPTIONS, DuplicateHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE},
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetCurrentProcess, OpenProcess, OpenThread,
            PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, SYNCHRONIZATION_SYNCHRONIZE,
            THREAD_SUSPEND_RESUME,
        },
    },
};

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a kernel handle this value owns, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: the handle is owned by `OwnedHandle` and only used while that owner
// is alive. The mutex serializes publication; copies of the raw `HANDLE`
// value are not closed.
unsafe impl Send for OwnedHandle {}
// SAFETY: same ownership rule as `Send`. Readers copy the handle value and do
// not close it.
unsafe impl Sync for OwnedHandle {}

/// The job `isolate` fills when the turn's child is spawned.
#[derive(Clone, Debug)]
pub(crate) struct SharedJob {
    handle: Arc<Mutex<Option<OwnedHandle>>>,
}

impl std::fmt::Debug for OwnedHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OwnedHandle").field(&self.0).finish()
    }
}

impl SharedJob {
    pub(crate) fn new() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<OwnedHandle>> {
        self.handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn raw(&self) -> Option<HANDLE> {
        self.lock().as_ref().map(OwnedHandle::raw)
    }

    /// Assign exactly one suspended root, then discard assignment/set rights.
    /// Descendants inherit membership; a later unrelated root cannot join.
    fn assign(&self, process: HANDLE) -> Result<()> {
        let mut guard = self.lock();
        if guard.is_some() {
            return Err(Error::other("turn job already has a root process"));
        }
        let full = create_kill_on_close_job()?;
        // SAFETY: both handles are open; the caller has not resumed the child.
        unsafe { AssignProcessToJobObject(full.raw(), process) }.map_err(Error::other)?;
        let reduced = reduce_job_handle(&full)?;
        // Keep the duplicate alive before closing the last all-access handle:
        // kill-on-close must not fire between assignment and publication.
        drop(full);
        *guard = Some(reduced);
        Ok(())
    }

    pub(crate) fn contains(&self, pid: u32) -> bool {
        let Some(job) = self.raw() else {
            return false;
        };
        process_in_job(pid, job)
    }

    fn terminate(&self) -> Result<()> {
        let Some(job) = self.raw() else {
            return Err(Error::other("turn job is not open"));
        };
        // SAFETY: `job` is the live kill-on-close job owned by `self`.
        unsafe { TerminateJobObject(job, 1) }.map_err(Error::other)
    }
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob {
    pub(crate) job: SharedJob,
}

impl CommandWrapper for KillOnCloseJob {
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        core: &CommandWrap,
    ) -> Result<()> {
        let mut flags = CREATE_SUSPENDED;
        if let Some(existing) = core.get_wrap::<process_wrap::tokio::CreationFlags>() {
            flags |= existing.0;
        } else {
            flags |= CREATE_NO_WINDOW;
        }
        command.creation_flags(flags.0);
        Ok(())
    }

    fn wrap_child(
        &mut self,
        mut inner: Box<dyn ChildWrapper>,
        _core: &CommandWrap,
    ) -> Result<Box<dyn ChildWrapper>> {
        let Some(borrowed) = inner.process_handle() else {
            let _ = inner.start_kill();
            return Err(Error::other("child has no process handle"));
        };
        let process = HANDLE(borrowed.as_raw_handle());
        if let Err(err) = self.job.assign(process) {
            let _ = inner.start_kill();
            return Err(err);
        }
        let Some(pid) = inner.id() else {
            let _ = self.job.terminate();
            let _ = inner.start_kill();
            return Err(Error::other("child has no pid"));
        };
        if let Err(err) = resume_process(pid) {
            let _ = self.job.terminate();
            let _ = inner.start_kill();
            return Err(err);
        }
        Ok(Box::new(JobChild {
            inner,
            job: self.job.clone(),
        }))
    }
}

#[derive(Debug)]
struct JobChild {
    inner: Box<dyn ChildWrapper>,
    job: SharedJob,
}

impl ChildWrapper for JobChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self.inner.as_ref()
    }

    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner.as_mut()
    }

    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.inner
    }

    fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        self.inner.process_handle()
    }

    fn start_kill(&mut self) -> Result<()> {
        let terminated = self.job.terminate();
        let _ = self.inner.start_kill();
        terminated
    }
}

/// Volume serial and file index of `path`. A replaced file gets a new index.
pub(crate) fn file_id(path: &Path) -> Option<(u64, u64)> {
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = std::fs::File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` is open and `info` is a live struct of the size the API writes.
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &raw mut info).ok()?;
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some((u64::from(info.dwVolumeSerialNumber), index))
}

fn create_kill_on_close_job() -> Result<OwnedHandle> {
    // SAFETY: an unnamed job. The handle is stored in `OwnedHandle`.
    let job = unsafe { CreateJobObjectW(None, None) }.map_err(Error::other)?;
    let owned = OwnedHandle(job);
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let len = u32::try_from(std::mem::size_of_val(&info))
        .map_err(|_| Error::other("job limit info does not fit in a DWORD"))?;
    // SAFETY: `info` is a live `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` and
    // `len` is its size. `owned` is the job just created.
    unsafe {
        SetInformationJobObject(
            owned.raw(),
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            len,
        )
    }
    .map_err(Error::other)?;
    Ok(owned)
}

/// Retain only membership queries, whole-tree termination, and waiting.
fn reduce_job_handle(full: &OwnedHandle) -> Result<OwnedHandle> {
    let access = JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE | SYNCHRONIZATION_SYNCHRONIZE.0;
    let mut reduced = HANDLE::default();
    // SAFETY: the pseudo process handle is valid in this process, `full` is
    // owned, and the output is initialized. No inheritance or SAME_ACCESS.
    unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            full.raw(),
            process,
            &raw mut reduced,
            access,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
    }
    .map_err(Error::other)?;
    Ok(OwnedHandle(reduced))
}

fn process_in_job(pid: u32, job: HANDLE) -> bool {
    if pid == 0 || job.is_invalid() {
        return false;
    }
    // SAFETY: opening the socket owner for a query. The handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let process = OwnedHandle(process);
    let mut result = windows::core::BOOL::default();
    // SAFETY: `process` and `job` are open handles. `result` is a live BOOL.
    let ok = unsafe {
        windows::Win32::System::JobObjects::IsProcessInJob(
            process.raw(),
            Some(job),
            &raw mut result,
        )
    };
    ok.is_ok() && result.as_bool()
}

fn resume_process(pid: u32) -> Result<()> {
    // SAFETY: a thread snapshot for `pid`. Closed when `snap` drops.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(Error::other)?;
    let snap = OwnedHandle(snap);
    let dw_size = u32::try_from(std::mem::size_of::<THREADENTRY32>())
        .map_err(|_| Error::other("THREADENTRY32 does not fit in a DWORD"))?;
    let mut entry = THREADENTRY32 {
        dwSize: dw_size,
        ..THREADENTRY32::default()
    };
    // SAFETY: `entry.dwSize` is set and `snap` is open.
    unsafe { Thread32First(snap.raw(), &raw mut entry) }.map_err(Error::other)?;
    let mut resumed = false;
    loop {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: the thread id came from the snapshot. Closed on drop.
            if let Ok(thread) =
                unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
            {
                let thread = OwnedHandle(thread);
                // SAFETY: `thread` is open for `THREAD_SUSPEND_RESUME`.
                let previous = unsafe { ResumeThread(thread.raw()) };
                if previous == u32::MAX {
                    return Err(Error::last_os_error());
                }
                if previous > 0 {
                    resumed = true;
                }
            }
        }
        // SAFETY: the same entry and the still-open snapshot.
        if unsafe { Thread32Next(snap.raw(), &raw mut entry) }.is_err() {
            break;
        }
    }
    if resumed {
        Ok(())
    } else {
        Err(Error::other("no suspended thread of the child was resumed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows::Win32::System::JobObjects::QueryInformationJobObject;

    /// An inert suspended test harness: it cannot run tests or outlive this scope.
    struct SuspendedChild(std::process::Child);
    impl SuspendedChild {
        fn new() -> Self {
            Self(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--list")
                    .creation_flags((CREATE_SUSPENDED | CREATE_NO_WINDOW).0)
                    .spawn()
                    .unwrap(),
            )
        }
        fn handle(&self) -> HANDLE {
            HANDLE(self.0.as_raw_handle())
        }
    }
    impl Drop for SuspendedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[tokio::test]
    async fn retained_job_cannot_assign_or_change_limits_but_can_query_and_terminate() {
        let job = SharedJob::new();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.arg("--list").kill_on_drop(true);
        let mut wrapped = crate::detect::isolate(command, job.clone());
        let mut root = wrapped.spawn().unwrap();
        let handle = job.raw().expect("published turn job");
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let len = u32::try_from(std::mem::size_of_val(&info)).unwrap();
        // SAFETY: live job and output with the documented size.
        unsafe {
            QueryInformationJobObject(
                Some(handle),
                JobObjectExtendedLimitInformation,
                (&raw mut info).cast(),
                len,
                None,
            )
        }
        .expect("query access retained");
        assert_eq!(
            info.BasicLimitInformation.LimitFlags,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
        // SAFETY: the same valid structure; this operation must be denied.
        let set = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                len,
            )
        };
        assert_eq!(set.unwrap_err().code(), ERROR_ACCESS_DENIED.to_hresult());
        let other = SuspendedChild::new();
        // SAFETY: a disposable suspended child, never the test runner itself.
        let assigned = unsafe { AssignProcessToJobObject(handle, other.handle()) };
        assert_eq!(
            assigned.unwrap_err().code(),
            ERROR_ACCESS_DENIED.to_hresult()
        );
        job.terminate().expect("terminate access retained");
        tokio::time::timeout(std::time::Duration::from_secs(10), root.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn assignment_is_single_use_and_does_not_kill_the_existing_root() {
        let first = SuspendedChild::new();
        let second = SuspendedChild::new();
        let job = SharedJob::new();
        job.assign(first.handle()).unwrap();
        assert!(job.contains(first.0.id()));
        assert!(job.assign(second.handle()).is_err());
        assert!(!job.contains(second.0.id()));
        assert!(job.contains(first.0.id()));
        job.terminate().unwrap();
    }
}
