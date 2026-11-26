//! A kill-on-close Job Object around the program on Windows, so that killing the session ends
//! the whole tree it started and not only its leader.
//!
//! The program is put in the job right after it was created; a process it starts in the first
//! moments, before that, is outside it (the same window `process-wrap` has).
#![allow(unsafe_code)]

use std::io::{Error, Result};
use std::os::windows::io::RawHandle;

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
};

/// The job. Dropping it closes the handle, which kills whatever is still inside.
pub(crate) struct Job(HANDLE);

// SAFETY: the handle is owned by this value, closed once, and only used through `&self` calls
// that the kernel serialises.
unsafe impl Send for Job {}
// SAFETY: same ownership rule as `Send`.
unsafe impl Sync for Job {}

impl Job {
    /// A new kill-on-close job that holds `process`.
    pub(crate) fn around(process: RawHandle) -> Result<Self> {
        // SAFETY: an unnamed job; the handle is owned by `job` from here on.
        let job = Self(unsafe { CreateJobObjectW(None, None) }.map_err(Error::other)?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let len = u32::try_from(std::mem::size_of_val(&info))
            .map_err(|_| Error::other("job limit info does not fit in a DWORD"))?;
        // SAFETY: `info` is a live struct of the size passed; `job.0` is the job just created.
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                len,
            )
        }
        .map_err(Error::other)?;
        // SAFETY: both handles are open: the job is ours, the process handle belongs to the child
        // object that is alive while this runs.
        unsafe { AssignProcessToJobObject(job.0, HANDLE(process)) }.map_err(Error::other)?;
        Ok(job)
    }

    /// End every process in the job.
    pub(crate) fn terminate(&self) {
        // SAFETY: `self.0` is the live job handle owned by `self`.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a kernel handle this value owns, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
