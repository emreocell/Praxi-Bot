//! Who owns the loopback listener the app is about to send its bearer token to?
//!
//! The daemon health probe is unauthenticated, so `{"status":"ok"}` only proves
//! that *something* listens on the port.  Another local user could bind the
//! port before the daemon does and answer the probe.  Before the token leaves
//! the app, the owning process of every listener on the port is resolved and
//! must either be the child this app spawned (exact PID) or, for a daemon the
//! app did not start, run as the same OS user as the app.

use std::collections::BTreeSet;

/// What a listener must be to receive the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Exactly the child process this app spawned.
    Child(u32),
    /// Any process of the current OS user (a daemon started from a shell).
    SameUser,
}

/// Pure decision: every listener PID satisfies `expect`, and there is at least one.
pub fn trusted(pids: &BTreeSet<u32>, expect: Expect, same_user: impl Fn(u32) -> bool) -> bool {
    !pids.is_empty()
        && pids.iter().all(|&p| match expect {
            Expect::Child(c) => p == c,
            Expect::SameUser => same_user(p),
        })
}

/// Check the listeners on loopback `port`.
///
/// # Errors
/// The owner cannot be determined or is not trusted (fail closed).
pub fn verify(port: u16, expect: Expect) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return platform::verify_linux(port, expect);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let pids = platform::listener_pids(port)?;
        if trusted(&pids, expect, platform::is_same_user) {
            Ok(())
        } else {
            Err(format!(
                "the listener on port {port} (pid {pids:?}) is not the daemon this app trusts"
            ))
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::collections::BTreeSet;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::Security::{
        EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: a handle we opened and own.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn listener_pids(port: u16) -> Result<BTreeSet<u32>, String> {
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
                0 => return Ok(parse(&buf, port)),
                122 => buf = vec![0u8; size as usize], // ERROR_INSUFFICIENT_BUFFER
                e => return Err(format!("GetExtendedTcpTable failed ({e})")),
            }
        }
        Err("GetExtendedTcpTable kept growing".to_owned())
    }

    fn parse(buf: &[u8], port: u16) -> BTreeSet<u32> {
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
            // Port is big-endian in the low 16 bits; address 127.0.0.1 or 0.0.0.0.
            let lp = u16::from_be((r.dwLocalPort & 0xFFFF) as u16);
            if lp == port && (r.dwLocalAddr == 0x0100_007F || r.dwLocalAddr == 0) {
                out.insert(r.dwOwningPid);
            }
        }
        out
    }

    /// `TOKEN_USER` bytes in an 8-aligned buffer.
    fn token_user(process: HANDLE) -> Option<Vec<u64>> {
        let mut tok = HANDLE::default();
        // SAFETY: valid process handle, out pointer to a local.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut tok) }.ok()?;
        let tok = Owned(tok);
        let mut len = 0u32;
        // SAFETY: size query; the expected error is ignored.
        let _ = unsafe { GetTokenInformation(tok.0, TokenUser, None, 0, &raw mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` has at least `len` writable bytes.
        unsafe {
            GetTokenInformation(
                tok.0,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &raw mut len,
            )
        }
        .ok()?;
        Some(buf)
    }

    pub fn is_same_user(pid: u32) -> bool {
        // SAFETY: pseudo handle, never closed.
        let me = token_user(unsafe { GetCurrentProcess() });
        // SAFETY: plain API call; failure (other user, protected) means untrusted.
        let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            return false;
        };
        let h = Owned(h);
        let them = token_user(h.0);
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
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::BTreeSet;
    use std::os::unix::fs::MetadataExt;

    use super::Expect;

    struct SocketEntry {
        uid: u32,
        inode: String,
    }

    fn entries_for(table: &str, port: u16) -> Vec<SocketEntry> {
        let want = format!(":{port:04X}");
        table
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let local = f.get(1)?;
                let (addr, _) = local.split_once(':')?;
                let ok = f.get(3) == Some(&"0A")
                    && local.ends_with(&want)
                    && (addr == "0100007F" || addr == "00000000");
                if !ok {
                    return None;
                }
                let uid = f.get(7)?.parse::<u32>().ok()?;
                let inode = (*f.get(9)?).to_owned();
                Some(SocketEntry { uid, inode })
            })
            .collect()
    }

    fn current_uid() -> Result<u32, String> {
        std::fs::metadata("/proc/self")
            .map(|m| m.uid())
            .map_err(|e| format!("cannot read /proc/self uid: {e}"))
    }

    fn pid_holds_all_inodes(pid: u32, inodes: &BTreeSet<String>) -> Result<bool, String> {
        let fds = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .map_err(|_| "cannot resolve the owner of the listening socket".to_owned())?;
        let mut matched = BTreeSet::new();
        for fd in fds.flatten() {
            if let Ok(t) = std::fs::read_link(fd.path())
                && let Some(i) = t
                    .to_str()
                    .and_then(|s| s.strip_prefix("socket:["))
                    .and_then(|s| s.strip_suffix(']'))
                && inodes.contains(i)
            {
                matched.insert(i.to_owned());
            }
        }
        Ok(matched.len() == inodes.len())
    }

    /// Verify Linux loopback socket ownership without scanning `/proc/*/fd` across
    /// unrelated processes. `Expect::SameUser` reads the kernel-recorded socket
    /// UID directly from `/proc/net/tcp`, so it works even when the daemon has
    /// `PR_SET_DUMPABLE=0`. `Expect::Child(pid)` inspects only `/proc/<pid>/fd`
    /// (never unrelated forked test children).
    pub fn verify_linux(port: u16, expect: Expect) -> Result<(), String> {
        let table = std::fs::read_to_string("/proc/net/tcp").map_err(|e| e.to_string())?;
        let entries = entries_for(&table, port);
        if entries.is_empty() {
            return Err(format!("no listener found on loopback port {port}"));
        }
        let my_uid = current_uid()?;
        if !entries.iter().all(|e| e.uid == my_uid) {
            return Err(format!(
                "the listener on port {port} is owned by another OS user"
            ));
        }
        match expect {
            Expect::SameUser => Ok(()),
            Expect::Child(pid) => {
                let inodes: BTreeSet<String> = entries.into_iter().map(|e| e.inode).collect();
                if pid_holds_all_inodes(pid, &inodes)? {
                    Ok(())
                } else {
                    Err(format!(
                        "the listener on port {port} is not owned by child pid {pid}"
                    ))
                }
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod platform {
    use std::collections::BTreeSet;
    use std::process::Command;

    fn out(cmd: &mut Command) -> Option<String> {
        let o = cmd.output().ok()?;
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    pub fn listener_pids(port: u16) -> Result<BTreeSet<u32>, String> {
        let text = out(Command::new("lsof").args([
            "-nP",
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
            "-Fp",
        ]))
        .ok_or("cannot run lsof")?;
        Ok(text
            .lines()
            .filter_map(|l| l.strip_prefix('p')?.parse().ok())
            .collect())
    }

    pub fn is_same_user(pid: u32) -> bool {
        let theirs = out(Command::new("ps").args(["-o", "uid=", "-p", &pid.to_string()]));
        let mine = out(Command::new("id").arg("-u"));
        matches!((theirs, mine), (Some(a), Some(b)) if !a.trim().is_empty() && a.trim() == b.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(p: &[u32]) -> BTreeSet<u32> {
        p.iter().copied().collect()
    }

    #[test]
    fn decision_rules() {
        assert!(trusted(&set(&[5]), Expect::Child(5), |_| false));
        assert!(!trusted(&set(&[6]), Expect::Child(5), |_| true));
        assert!(!trusted(&set(&[5, 6]), Expect::Child(5), |_| true));
        assert!(trusted(&set(&[7]), Expect::SameUser, |_| true));
        assert!(!trusted(&set(&[7, 8]), Expect::SameUser, |p| p == 7));
        assert!(!trusted(&set(&[]), Expect::SameUser, |_| true));
    }

    #[test]
    fn resolves_our_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id();
        assert!(verify(port, Expect::SameUser).is_ok());
        assert!(verify(port, Expect::Child(me)).is_ok());
        // A listener that is not the expected child is refused.
        assert!(verify(port, Expect::Child(me.wrapping_add(1))).is_err());
    }
}
