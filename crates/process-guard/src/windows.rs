//! The only native-API boundary. OWNER RIGHTS is essential: without it a
//! same-user owner can restore `WRITE_DAC` despite ordinary deny ACEs. This does
//! not revoke handles already opened, protect thread objects, or sandbox files.
#![allow(unsafe_code)]

use std::io;
use windows::Win32::{
    Foundation::{CloseHandle, HLOCAL, LocalFree},
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_KERNEL_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_DUP_HANDLE, PROCESS_VM_READ,
    },
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

pub(super) fn protect() -> io::Result<()> {
    // OWNER RIGHTS suppresses implicit owner WRITE_DAC. The owner can query,
    // synchronize and terminate; SYSTEM retains full access. Deliberately no
    // VM_READ/WRITE/OPERATION, DUP_HANDLE, CREATE_PROCESS/THREAD or WRITE_DAC.
    // 0x00121001 = SYNCHRONIZE | READ_CONTROL | QUERY_LIMITED_INFORMATION | TERMINATE.
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
    // modified, and children do not inherit a process security descriptor.
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
        .map_err(io::Error::other)?;
    }
    verify_denied()
}

/// Rights that expose memory, handles or execution of a process.
pub(super) const DANGEROUS: [PROCESS_ACCESS_RIGHTS; 8] = [
    PROCESS_VM_READ,
    PROCESS_DUP_HANDLE,
    PROCESS_ACCESS_RIGHTS(0x0004_0000), // WRITE_DAC
    PROCESS_ACCESS_RIGHTS(0x0008_0000), // WRITE_OWNER
    PROCESS_ACCESS_RIGHTS(0x0008),      // VM_OPERATION
    PROCESS_ACCESS_RIGHTS(0x0020),      // VM_WRITE
    PROCESS_ACCESS_RIGHTS(0x0080),      // CREATE_PROCESS
    PROCESS_ACCESS_RIGHTS(0x0002),      // CREATE_THREAD
];

/// Whether this process can open itself with `access` (the DACL is bypassed by
/// an enabled `SeDebugPrivilege`, which elevated administrators may hold).
pub(super) fn can_open_self(access: PROCESS_ACCESS_RIGHTS) -> bool {
    // SAFETY: opens this process id only; a returned handle is closed once.
    unsafe {
        OpenProcess(access, false, std::process::id()).is_ok_and(|handle| {
            let _ = CloseHandle(handle);
            true
        })
    }
}

/// The ACL is only a promise until the kernel enforces it for this caller.
fn verify_denied() -> io::Result<()> {
    if DANGEROUS.iter().any(|a| can_open_self(*a)) {
        return Err(io::Error::other(
            "the access list was set but this token can still open the process for memory access (elevated administrator or SeDebugPrivilege bypasses it)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DANGEROUS, can_open_self};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE, WIN32_ERROR},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    fn can_open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> bool {
        // SAFETY: only opens a process id; a successful handle is closed once.
        // No memory is read.
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

    fn is_elevated() -> bool {
        let mut token = HANDLE::default();
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        // SAFETY: queries this process's own token into a correctly sized
        // buffer; the token handle is closed once.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token).unwrap();
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&raw mut elevation).cast()),
                u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap(),
                &raw mut len,
            );
            let _ = CloseHandle(token);
            ok.unwrap();
        }
        elevation.TokenIsElevated != 0
    }

    #[test]
    fn protection_fixture() {
        if std::env::var("PRAXI_GUARD_FIXTURE").as_deref() != Ok("yes") {
            return;
        }
        let pid = std::process::id();
        for access in DANGEROUS {
            assert!(can_open(pid, access), "before: {access:?}");
        }
        let report = crate::protect();
        let denied = DANGEROUS.iter().all(|a| !can_open_self(*a));
        match report.outcome {
            // The guard may only claim protection the kernel really enforces.
            crate::Outcome::Applied => {
                assert!(denied, "reported Applied but access is open: {report:?}");
                for access in DANGEROUS {
                    assert!(!can_open(pid, access), "after: {access:?}");
                }
            }
            // A DACL cannot stop an elevated administrator (SeDebugPrivilege):
            // that is the only case where an honest DEGRADED is acceptable.
            crate::Outcome::Failed => {
                assert!(
                    is_elevated(),
                    "unprivileged token must be protected: {report:?}"
                );
                assert!(!denied, "reported Failed but access is denied: {report:?}");
            }
            crate::Outcome::NotSupported | crate::Outcome::Skipped => panic!("{report:?}"),
        }
        assert!(can_open(pid, PROCESS_QUERY_LIMITED_INFORMATION));

        // Children must be unaffected: they start, run, and stay inspectable.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 3 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        for access in DANGEROUS {
            assert!(can_open(child.id(), access), "child must keep {access:?}");
        }
        assert!(child.wait().unwrap().success());
    }
}
