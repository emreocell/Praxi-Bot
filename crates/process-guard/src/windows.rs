//! The only native-API boundary. OWNER RIGHTS is essential: without it a
//! same-user owner can restore `WRITE_DAC` despite ordinary deny ACEs. This does
//! not revoke handles already opened, protect thread objects, or sandbox files.
#![allow(unsafe_code)]

use std::collections::BTreeSet;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::AF_INET,
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
            SetNamedSecurityInfoW, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS,
        PROCESS_DUP_HANDLE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        QueryFullProcessImageNameW,
    },
};
use windows::core::{BOOL, PCWSTR, PWSTR, w};

/// Kernel answers for `owner::verify_with`.
pub(super) struct SystemProbe;

impl crate::owner::Probe for SystemProbe {
    fn image_path(&self, pid: u32) -> Option<PathBuf> {
        // SAFETY: query-only handle, closed once; the buffer is sized for the API maximum.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut path = vec![0u16; 32768];
            let mut len = u32::try_from(path.len()).ok()?;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &raw mut len,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            ok.then(|| PathBuf::from(String::from_utf16_lossy(&path[..len as usize])))
        }
    }

    fn same_user(&self, pid: u32) -> bool {
        // SAFETY: pseudo handle for ourselves, never closed.
        let me = token_user(unsafe { GetCurrentProcess() });
        // SAFETY: query-only handle, closed once; failure means untrusted.
        let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            return false;
        };
        let them = token_user(h);
        // SAFETY: handle opened above.
        let _ = unsafe { CloseHandle(h) };
        let (Some(a), Some(b)) = (me, them) else {
            return false;
        };
        // SAFETY: both buffers hold a TOKEN_USER written by the OS and outlive the call.
        unsafe {
            let sa = (*a.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let sb = (*b.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            EqualSid(sa, sb).is_ok()
        }
    }

    fn listener_pids(&self, port: u16) -> Result<BTreeSet<u32>, String> {
        let mut size = 0u32;
        let mut buf: Vec<u8> = Vec::new();
        for _ in 0..8 {
            let ptr = if buf.is_empty() {
                None
            } else {
                Some(buf.as_mut_ptr().cast())
            };
            // SAFETY: `ptr` is null or points to `size` writable bytes.
            let rc = unsafe {
                GetExtendedTcpTable(
                    ptr,
                    &raw mut size,
                    false,
                    u32::from(AF_INET.0),
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            match rc {
                0 => return Ok(parse_listeners(&buf, port)),
                122 => buf = vec![0u8; size as usize], // ERROR_INSUFFICIENT_BUFFER
                e => return Err(format!("GetExtendedTcpTable failed ({e})")),
            }
        }
        Err("GetExtendedTcpTable kept growing".to_owned())
    }
}

/// `TOKEN_USER` bytes in an 8-aligned buffer.
fn token_user(process: HANDLE) -> Option<Vec<u64>> {
    let mut tok = HANDLE::default();
    // SAFETY: valid process handle, out pointer to a local.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut tok) }.ok()?;
    let mut len = 0u32;
    // SAFETY: size query; the expected error is ignored.
    let _ = unsafe { GetTokenInformation(tok, TokenUser, None, 0, &raw mut len) };
    let buf = if len == 0 {
        None
    } else {
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` has at least `len` writable bytes.
        unsafe {
            GetTokenInformation(
                tok,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &raw mut len,
            )
        }
        .ok()
        .map(|()| buf)
    };
    // SAFETY: token opened above, closed once.
    let _ = unsafe { CloseHandle(tok) };
    buf
}

fn parse_listeners(buf: &[u8], port: u16) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    if buf.len() < 4 {
        return out;
    }
    let n = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let row = size_of::<MIB_TCPROW_OWNER_PID>();
    for i in 0..n {
        let off = 4 + i * row;
        if off + row > buf.len() {
            break;
        }
        // SAFETY: in bounds (checked); rows are plain integers, read unaligned.
        let r: MIB_TCPROW_OWNER_PID =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
        let local = u16::from_be((r.dwLocalPort & 0xFFFF) as u16);
        if local == port && (r.dwLocalAddr == 0x0100_007F || r.dwLocalAddr == 0) {
            out.insert(r.dwOwningPid);
        }
    }
    out
}

/// Replace the file's DACL with owner + SYSTEM only, protected from inheritance.
pub(super) fn owner_only_file(path: &Path) -> io::Result<()> {
    let mut raw = PSECURITY_DESCRIPTOR::default();
    // SAFETY: fixed valid SDDL; out pointer is writable and starts empty.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;FA;;;OW)(A;;FA;;;SY)"),
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
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: descriptor outlives the ACL pointer use; `wide` is NUL-terminated.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &raw mut present,
            &raw mut acl,
            &raw mut defaulted,
        )
        .map_err(io::Error::other)?;
        if !present.as_bool() || acl.is_null() {
            return Err(io::Error::other("missing file DACL"));
        }
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
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
    fn system_probe_sees_our_own_image_and_listener() {
        use crate::owner::{Probe, same_file};
        let me = std::process::id();
        let probe = super::SystemProbe;
        let image = probe.image_path(me).expect("own image path");
        assert!(same_file(&image, &std::env::current_exe().unwrap()));
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        assert_eq!(
            probe
                .listener_pids(port)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [me]
        );
    }

    #[test]
    fn endpoint_and_lock_files_get_an_owner_only_dacl() {
        let dir = tempfile::tempdir().unwrap();
        let lock = crate::lifecycle::acquire(dir.path()).unwrap();
        crate::lifecycle::Endpoint {
            pid: 1,
            port: 2,
            requested_port: 2,
        }
        .publish(dir.path())
        .unwrap();
        // Still readable/lockable by us, and the lock file cannot be deleted while held.
        assert!(crate::lifecycle::discover(dir.path()).is_some());
        assert!(std::fs::remove_file(dir.path().join(crate::lifecycle::LOCK_FILE)).is_err());
        drop(lock);
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
