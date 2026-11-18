//! Process identity, elevation and window class for the run-scoped control
//! grant. Every function returns `None` / an empty value on any
//! failure; the callers treat that as "unknown" and refuse.
#![allow(unsafe_code)]

use desktop_api::{InputTarget, ProcessIdentity};
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, POINT},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{
            GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            GA_ROOT, GW_OWNER, GetAncestor, GetClassNameW, GetWindow, WindowFromPoint,
        },
    },
    core::PWSTR,
};

use crate::winlist;

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: a handle this module opened and owns.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(pid: u32) -> Option<Owned> {
    if pid == 0 {
        return None;
    }
    // SAFETY: plain API call; a failure (protected process, gone) is `None`.
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .ok()
        .map(Owned)
}

fn filetime_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Image path, pid and creation time of a process; `None` when any is unknown.
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let h = open(pid)?;
    let mut buf = vec![0u16; 1024];
    let mut len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is writable for `len` UTF-16 units; `len` is an in/out size.
    unsafe {
        QueryFullProcessImageNameW(
            h.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        )
    }
    .ok()?;
    let image_path = String::from_utf16_lossy(buf.get(..len as usize)?);
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: four valid out pointers to locals, and a live process handle.
    unsafe {
        GetProcessTimes(
            h.0,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .ok()?;
    let id = ProcessIdentity {
        image_path,
        pid,
        start_time: filetime_u64(created),
    };
    id.is_complete().then_some(id)
}

/// Whether the process runs elevated. `None` when the token cannot be read,
/// which is the normal result for an elevated process seen from a normal one.
pub fn process_elevated(pid: u32) -> Option<bool> {
    let h = open(pid)?;
    let mut tok = HANDLE::default();
    // SAFETY: valid process handle and an out pointer to a local.
    unsafe { OpenProcessToken(h.0, TOKEN_QUERY, &raw mut tok) }.ok()?;
    let tok = Owned(tok);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: the buffer is a `TOKEN_ELEVATION` and the size passed matches it.
    unsafe {
        GetTokenInformation(
            tok.0,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).ok()?,
            &raw mut len,
        )
    }
    .ok()?;
    Some(elevation.TokenIsElevated != 0)
}

/// Window class name. `None` when the lookup fails or returns nothing: a failed
/// lookup must never look like an ordinary class.
pub fn window_class(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    // SAFETY: the buffer is writable for its whole length.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    let n = usize::try_from(n).ok().filter(|n| *n > 0)?;
    Some(String::from_utf16_lossy(buf.get(..n)?))
}

/// Top-level window of `hwnd`. `None` when the OS returns none (no fallback to
/// the child: the ancestry is part of what is being checked).
pub fn root_of(hwnd: HWND) -> Option<HWND> {
    // SAFETY: plain query on a handle.
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    (!root.0.is_null()).then_some(root)
}

/// The window under a screen point and its top-level window.
pub fn point_hit(x: f64, y: f64) -> Option<(HWND, HWND)> {
    #[allow(clippy::cast_possible_truncation)]
    let pt = POINT {
        x: x.round() as i32,
        y: y.round() as i32,
    };
    // SAFETY: plain query by value.
    let hit = unsafe { WindowFromPoint(pt) };
    if hit.0.is_null() {
        return None;
    }
    Some((hit, root_of(hit)?))
}

/// Whether a top-level window has an owner (an owned popup: modal and custom
/// dialogs included). `None`-free: a window without an owner is `false`.
fn is_owned(top: HWND) -> bool {
    // SAFETY: plain query on a handle.
    unsafe { GetWindow(top, GW_OWNER) }.is_ok_and(|o| !o.0.is_null())
}

fn facts(hwnd: HWND, owned: bool) -> Option<InputTarget> {
    let pid = winlist::window_pid(hwnd);
    let identity = process_identity(pid)?;
    let info = winlist::window_info(hwnd)?;
    Some(InputTarget {
        window_id: info.id,
        title: info.title,
        class: window_class(hwnd)?,
        identity,
        elevated: process_elevated(pid),
        password_focus: false,
        owned,
    })
}

/// Facts about a top-level window and its program. `None` when the program or
/// the class cannot be read. `password_focus` is filled in by the caller.
pub fn top_level_facts(hwnd: HWND) -> Option<InputTarget> {
    facts(hwnd, is_owned(hwnd))
}

/// Facts about a child window (the receiver under a click): its own program
/// and class; ownership belongs to the top-level window.
pub fn child_facts(hwnd: HWND) -> Option<InputTarget> {
    facts(hwnd, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_process_has_a_complete_identity_and_a_readable_token() {
        let pid = std::process::id();
        let id = process_identity(pid).expect("own identity");
        assert!(id.is_complete());
        assert_eq!(id.pid, pid);
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            desktop_api::guard::program_stem(&id.image_path),
            desktop_api::guard::program_stem(&exe.to_string_lossy())
        );
        assert!(process_elevated(pid).is_some());
        // A second read gives the same start time: the pair names one process.
        assert_eq!(process_identity(pid), Some(id));
    }

    #[test]
    fn pid_zero_and_a_missing_process_have_no_identity() {
        assert_eq!(process_identity(0), None);
        assert_eq!(process_elevated(0), None);
        assert_eq!(process_identity(u32::MAX - 3), None);
    }
}
