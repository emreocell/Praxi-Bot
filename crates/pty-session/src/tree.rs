//! Killing a program's whole tree on Unix.
//!
//! The program is the leader of its own session (the PTY made it one), so its process group is
//! its own. A launcher that starts its own group or session (`setsid`, `nohup`, a daemonising
//! helper) is outside that group, and is found through the session and the parent links instead.

use rustix::process::{Pid, Signal, kill_process, kill_process_group};

/// `SIGKILL` to the group of `leader` and to every other process of its session and every
/// descendant of those. Call it only while `leader` has not been reaped, so that its pid is still
/// its own and cannot have been handed to a stranger.
pub(crate) fn kill_tree(leader: u32) {
    let Ok(root) = i32::try_from(leader) else {
        return;
    };
    // Looked up BEFORE the group dies: afterwards the descendants are re-parented.
    let victims = tree_of(root);
    if let Some(pid) = Pid::from_raw(root) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    for victim in victims.into_iter().filter(|p| *p != root) {
        if let Some(pid) = Pid::from_raw(victim) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

/// `(pid, ppid, session)` of every process (Linux `/proc`; empty elsewhere).
#[cfg(target_os = "linux")]
fn process_table() -> Vec<(i32, i32, i32)> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // `pid (comm) state ppid pgrp session ...`; comm may hold spaces and parentheses.
        let rest = &stat[stat.rfind(')')? + 1..];
        let mut fields = rest.split_whitespace().skip(1);
        let parent: i32 = fields.next()?.parse().ok()?;
        let _group = fields.next()?;
        let session: i32 = fields.next()?.parse().ok()?;
        Some((pid, parent, session))
    })
    .collect()
}

#[cfg(not(target_os = "linux"))]
fn process_table() -> Vec<(i32, i32, i32)> {
    Vec::new()
}

/// `root`, the processes in session `root`, and all their descendants.
fn tree_of(root: i32) -> Vec<i32> {
    let table = process_table();
    let mut tree: Vec<i32> = table
        .iter()
        .filter(|(pid, _, session)| *pid == root || *session == root)
        .map(|(pid, _, _)| *pid)
        .collect();
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for (candidate, its_parent, _) in &table {
            if *its_parent == parent && !tree.contains(candidate) {
                tree.push(*candidate);
            }
        }
        i += 1;
    }
    tree
}

/// Wait for `pid` to end WITHOUT reaping it: the zombie keeps the pid ours until the caller
/// reaps it, so a group kill sent in between cannot hit a stranger. `false` when the wait
/// failed (the caller then just reaps).
#[cfg(target_os = "linux")]
pub(crate) fn wait_without_reaping(pid: u32) -> bool {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(_) => return true,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
    }
}
