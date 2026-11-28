//! Which program owns a window: image path and start time from `/proc`, and
//! whether it runs in the user's own account. The pair (pid, start time) makes a
//! recycled pid a different program, like the Windows backend's identity.

use std::os::unix::fs::MetadataExt as _;

use desktop_api::ProcessIdentity;

use crate::pure::{parse_proc_start_time, parse_proc_uids, runs_outside_own_account};

/// Image path, pid and start time of `pid`. `None` for pid 0, a process that is
/// gone, or one whose image or start time cannot be read.
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    if pid == 0 {
        return None;
    }
    let image = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let id = ProcessIdentity {
        image_path: image.to_string_lossy().into_owned(),
        pid,
        start_time: parse_proc_start_time(&stat)?,
    };
    id.is_complete().then_some(id)
}

/// The user id the daemon runs as (the owner of its own `/proc` entry).
fn own_euid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// `Some(true)` when the program runs outside the daemon's own account (root, set-uid
/// or another user), `Some(false)` when it is the user's own, `None` when it cannot be
/// read, which counts as outside.
pub fn process_elevated(pid: u32) -> Option<bool> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let uids = parse_proc_uids(&status)?;
    Some(runs_outside_own_account(uids, own_euid()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_complete_identity_in_its_own_account() {
        let pid = std::process::id();
        let id = process_identity(pid).expect("own identity");
        assert!(id.is_complete());
        assert_eq!(id.pid, pid);
        assert_eq!(
            desktop_api::guard::program_stem(&id.image_path),
            desktop_api::guard::program_stem(&std::env::current_exe().unwrap().to_string_lossy())
        );
        // A second read names the same process.
        assert_eq!(process_identity(pid), Some(id));
        assert_eq!(process_elevated(pid), Some(false));
    }

    #[test]
    fn pid_zero_and_a_missing_process_have_no_identity() {
        assert_eq!(process_identity(0), None);
        assert_eq!(process_identity(u32::MAX - 3), None);
        assert_eq!(process_elevated(u32::MAX - 3), None);
    }

    #[test]
    fn init_is_outside_the_own_account_unless_we_are_root() {
        // pid 1 belongs to root: elevated for any other user, the same account for root.
        let own = own_euid().unwrap();
        assert_eq!(process_elevated(1), Some(own != 0));
    }
}
