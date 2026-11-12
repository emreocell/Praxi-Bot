//! The only native-API boundary. OWNER RIGHTS is essential: without it a
//! same-user owner can restore `WRITE_DAC` despite ordinary deny ACEs. This does
//! not revoke handles already opened, protect thread objects, or sandbox files.
#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle};
use windows::Win32::{
    Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, HLOCAL, LocalFree, SetHandleInformation,
    },
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_KERNEL_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    },
    System::Threading::GetCurrentProcess,
};
use windows::core::{BOOL, w};

struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the conversion API allocated this descriptor with LocalAlloc;
        // this value owns it, and SetSecurityInfo has copied its ACL already.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

pub(super) fn seal_stdin() -> io::Result<()> {
    seal_handle(HANDLE(std::io::stdin().as_handle().as_raw_handle()))
}

fn seal_handle(handle: HANDLE) -> io::Result<()> {
    // SAFETY: caller holds this live stdin/pipe handle. Clear inheritance
    // only: never close it or read from it here.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS::default()) }
        .map_err(io::Error::other)
}

pub(super) fn protect() -> io::Result<()> {
    // OWNER RIGHTS suppresses implicit owner WRITE_DAC. The owner can query,
    // synchronize and terminate; SYSTEM retains full access. Deliberately no
    // VM_READ/WRITE/OPERATION, DUP_HANDLE, CREATE_PROCESS/THREAD or WRITE_DAC.
    let mut raw = PSECURITY_DESCRIPTOR::default();
    // SAFETY: fixed valid SDDL; out pointer is writable and starts empty.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;0x00121001;;;OW)(A;;GA;;;SY)"),
            1,
            &raw mut raw,
            None,
        )
    }
    .map_err(io::Error::other)?;
    let descriptor = Descriptor(raw);
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut acl = std::ptr::null_mut();
    // SAFETY: descriptor remains live while extracting/copying its ACL; the
    // pseudohandle names this process and is never closed. No other process is
    // modified. Apply before bootstrap so bot children cannot pre-open it.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &raw mut present,
            &raw mut acl,
            &raw mut defaulted,
        )
        .map_err(io::Error::other)?;
        if !present.as_bool() || acl.is_null() {
            return Err(io::Error::other("missing process DACL"));
        }
        SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl),
            None,
        )
        .ok()
        .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::{CloseHandle, GetHandleInformation, WIN32_ERROR},
        System::Threading::{
            OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_DUP_HANDLE,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        },
    };

    #[test]
    fn bootstrap_handle_is_explicitly_non_inheritable() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let handle = HANDLE(reader.as_handle().as_raw_handle());
        // SAFETY: reader owns the pipe throughout; only metadata is changed.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .unwrap();
        seal_handle(handle).unwrap();
        let mut flags = 0;
        // SAFETY: same owned handle and a writable u32.
        unsafe { GetHandleInformation(handle, &raw mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
    }

    fn can_open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> bool {
        // SAFETY: only opens a synthetic test process; successful handles are
        // owned here and closed once. No memory is read and no credential used.
        unsafe {
            match OpenProcess(access, false, pid) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    true
                }
                Err(error) => {
                    assert_eq!(error.code(), WIN32_ERROR(5).to_hresult());
                    false
                }
            }
        }
    }

    fn probe(expect_access: bool) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows::tests::access_probe", "--nocapture"])
            .env("PRAXI_GUARD_PROBE_PID", std::process::id().to_string())
            .env(
                "PRAXI_GUARD_EXPECT_ACCESS",
                if expect_access { "yes" } else { "no" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn access_probe() {
        let Ok(pid) = std::env::var("PRAXI_GUARD_PROBE_PID") else {
            return;
        };
        let pid = pid.parse().unwrap();
        let expected = std::env::var("PRAXI_GUARD_EXPECT_ACCESS").unwrap() == "yes";
        for access in [
            PROCESS_VM_READ,
            PROCESS_DUP_HANDLE,
            PROCESS_ACCESS_RIGHTS(0x0004_0000),
            PROCESS_ACCESS_RIGHTS(0x0008_0000),
            PROCESS_ACCESS_RIGHTS(0x0020),
            PROCESS_ACCESS_RIGHTS(0x0008),
            PROCESS_ACCESS_RIGHTS(0x0080),
            PROCESS_ACCESS_RIGHTS(0x0002),
        ] {
            assert_eq!(can_open(pid, access), expected, "access={access:?}");
        }
        assert!(can_open(pid, PROCESS_QUERY_LIMITED_INFORMATION));
    }

    #[test]
    fn protection_fixture() {
        if std::env::var("PRAXI_GUARD_FIXTURE").as_deref() != Ok("yes") {
            return;
        }
        probe(true);
        protect().unwrap();
        probe(false);
    }

    #[test]
    fn same_user_child_cannot_open_guarded_parent_memory_or_restore_its_acl() {
        // Only the fixture process is modified, never the test runner/desktop.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows::tests::protection_fixture",
                "--nocapture",
            ])
            .env("PRAXI_GUARD_FIXTURE", "yes")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
