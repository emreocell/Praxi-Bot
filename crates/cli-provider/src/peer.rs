//! Accept a loopback MCP connection only when its owning pid is this turn's
//! CLI child or a descendant of that child.
//!
//! Claude and Codex can both launch a stdio MCP server, but they spawn that
//! process with an explicit handle list, so a pipe created here is not
//! inherited by the server. The bridge stays on loopback and this check is
//! what rejects every other same-user client.

use std::net::{Ipv4Addr, SocketAddr};

/// `true` only when `peer` is an established socket owned by `child_pid` or
/// one of that process's descendants. Any lookup failure is a refusal.
#[must_use]
pub(crate) fn peer_is_allowed(peer: SocketAddr, listen: SocketAddr, child_pid: u32) -> bool {
    if child_pid == 0 {
        return false;
    }
    let Some(owner) = connection_owner(peer, listen) else {
        return false;
    };
    owner == child_pid || is_descendant(owner, child_pid)
}

fn connection_owner(peer: SocketAddr, listen: SocketAddr) -> Option<u32> {
    for attempt in 0..8 {
        if let Some(pid) = connection_owner_once(peer, listen) {
            return Some(pid);
        }
        if attempt + 1 < 8 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    None
}

fn v4(addr: SocketAddr) -> Option<(Ipv4Addr, u16)> {
    match addr {
        SocketAddr::V4(v4) => Some((*v4.ip(), v4.port())),
        SocketAddr::V6(_) => None,
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn connection_owner_once(peer: SocketAddr, listen: SocketAddr) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    let (peer_ip, peer_port) = v4(peer)?;
    let (listen_ip, listen_port) = v4(listen)?;
    let family = u32::from(AF_INET.0);
    let mut size = 0u32;
    // SAFETY: a null table pointer asks for the byte count via `size`.
    let first = unsafe {
        GetExtendedTcpTable(
            None,
            &raw mut size,
            false,
            family,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if first != 0 && first != 122 {
        return None;
    }
    for _ in 0..3 {
        let words = usize::try_from(size).ok()?.div_ceil(4).saturating_add(8);
        let mut buf = vec![0u32; words];
        let mut got = size;
        // SAFETY: `buf` is 4-byte aligned, writable, and at least `got` bytes
        // when the call succeeds. `MIB_TCPTABLE_OWNER_PID` is a u32 header
        // followed by u32 rows, so that alignment is sufficient.
        let ret = unsafe {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr().cast()),
                &raw mut got,
                false,
                family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if ret == 122 {
            size = got;
            continue;
        }
        if ret != 0 {
            return None;
        }
        let num = usize::try_from(*buf.first()?).ok()?;
        let row_words = std::mem::size_of::<MIB_TCPROW_OWNER_PID>() / std::mem::size_of::<u32>();
        let need = 1usize.saturating_add(num.saturating_mul(row_words));
        if buf.len() < need {
            return None;
        }
        // SAFETY: `buf` holds `dwNumEntries` then `num` rows, checked above.
        // The slice borrows `buf` only for this iteration.
        let rows = unsafe {
            let table = &*buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
            let count = usize::try_from(table.dwNumEntries).ok()?;
            if count != num {
                return None;
            }
            std::slice::from_raw_parts(table.table.as_ptr(), count)
        };
        for row in rows {
            if ipv4_from_tcp(row.dwLocalAddr) == peer_ip
                && tcp_port(row.dwLocalPort) == peer_port
                && ipv4_from_tcp(row.dwRemoteAddr) == listen_ip
                && tcp_port(row.dwRemotePort) == listen_port
            {
                return Some(row.dwOwningPid);
            }
        }
        return None;
    }
    None
}

#[cfg(windows)]
fn ipv4_from_tcp(addr: u32) -> Ipv4Addr {
    // The table stores IPv4 addresses in network byte order.
    Ipv4Addr::from(addr.to_be())
}

#[cfg(windows)]
fn tcp_port(raw: u32) -> u16 {
    // The port is network-order in the low 16 bits.
    u16::from_be(u16::try_from(raw & 0xFFFF).unwrap_or(0))
}

#[cfg(windows)]
struct Snapshot(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
#[allow(unsafe_code)]
impl Drop for Snapshot {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        // SAFETY: `self.0` is the snapshot from CreateToolhelp32Snapshot and
        // is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn is_descendant(pid: u32, ancestor: u32) -> bool {
    use std::collections::HashMap;

    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    if pid == 0 || ancestor == 0 {
        return false;
    }
    // SAFETY: a process snapshot. The handle is closed on every return path.
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };
    let snap = Snapshot(snap);
    let mut entry = PROCESSENTRY32W::default();
    let Ok(size) = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()) else {
        return false;
    };
    entry.dwSize = size;
    // SAFETY: `entry.dwSize` is set and `entry` outlives the call.
    if unsafe { Process32FirstW(snap.0, &raw mut entry) }.is_err() {
        return false;
    }
    let mut parents = HashMap::<u32, u32>::new();
    loop {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        // SAFETY: the same sized entry and the still-open snapshot.
        if unsafe { Process32NextW(snap.0, &raw mut entry) }.is_err() {
            break;
        }
    }
    let mut current = pid;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let Some(parent) = parents.get(&current).copied() else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent == 0 || parent == current || !seen.insert(parent) {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(not(windows))]
fn connection_owner_once(peer: SocketAddr, listen: SocketAddr) -> Option<u32> {
    let (peer_ip, peer_port) = v4(peer)?;
    let (listen_ip, listen_port) = v4(listen)?;
    let text = std::fs::read_to_string("/proc/net/tcp").ok()?;
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let Some((local_ip, local_port)) = parse_proc_addr(fields[1]) else {
            continue;
        };
        let Some((remote_ip, remote_port)) = parse_proc_addr(fields[2]) else {
            continue;
        };
        if local_ip == peer_ip
            && local_port == peer_port
            && remote_ip == listen_ip
            && remote_port == listen_port
        {
            let inode = fields[9];
            return pid_for_socket_inode(inode);
        }
    }
    None
}

#[cfg(not(windows))]
fn parse_proc_addr(text: &str) -> Option<(Ipv4Addr, u16)> {
    let (ip_hex, port_hex) = text.split_once(':')?;
    let ip = u32::from_str_radix(ip_hex, 16).ok()?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    // /proc/net/tcp prints the address bytes in host order.
    Some((Ipv4Addr::from(ip.swap_bytes()), port))
}

#[cfg(not(windows))]
fn pid_for_socket_inode(inode: &str) -> Option<u32> {
    let needle = format!("socket:[{inode}]");
    let proc = std::fs::read_dir("/proc").ok()?;
    for entry in proc.filter_map(Result::ok) {
        let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = std::fs::read_dir(fd_dir) else {
            continue;
        };
        for fd in fds.filter_map(Result::ok) {
            if std::fs::read_link(fd.path()).is_ok_and(|target| target.to_string_lossy() == needle)
            {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn is_descendant(pid: u32, ancestor: u32) -> bool {
    if pid == 0 || ancestor == 0 {
        return false;
    }
    let mut current = pid;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let parent = parent_pid(current)?;
        if parent == ancestor {
            return true;
        }
        if parent == 0 || parent == current || !seen.insert(parent) {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(not(windows))]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::peer_is_allowed;
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    #[test]
    fn connecting_process_is_the_owner_and_other_pids_are_refused() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = listener.local_addr().unwrap();
        let client = TcpStream::connect(listen).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        assert!(peer_is_allowed(peer, listen, std::process::id()));
        assert!(!peer_is_allowed(peer, listen, u32::MAX));
        assert!(!peer_is_allowed(peer, listen, 0));
    }
}
