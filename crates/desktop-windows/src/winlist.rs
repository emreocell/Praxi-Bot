//! Top-level window enumeration, activation and process launch (Win32).
#![allow(unsafe_code)]

use std::ffi::c_void;

use desktop_api::{
    DesktopError,
    snapshot::{AppSpec, Pid, Rect, WindowInfo},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, RECT, SetLastError, WAIT_OBJECT_0},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        System::Threading::{
            AttachThreadInput, GetCurrentThreadId, GetProcessId, WaitForInputIdle,
            WaitForSingleObject,
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{
                BringWindowToTop, EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW,
                GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
                IsIconic, IsWindow, IsWindowVisible, SW_RESTORE, SW_SHOWNORMAL,
                SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::PCWSTR,
};

/// Make this process per-monitor DPI aware so that UIA rectangles, GDI capture
/// and `SendInput` all use the same physical-pixel coordinate space.
/// Failure (already set) is harmless.
pub fn ensure_dpi_aware() {
    // SAFETY: plain FFI call with a constant argument; no pointers involved.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn hwnd_from_id(id: u64) -> HWND {
    // Window ids are the HWND value; HWNDs fit in 32 significant bits.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    HWND(id as isize as *mut c_void)
}

pub fn id_from_hwnd(hwnd: HWND) -> u64 {
    #[allow(clippy::cast_sign_loss)]
    {
        hwnd.0 as isize as u64
    }
}

fn rect_to_api(r: &RECT) -> Rect {
    Rect {
        x: f64::from(r.left),
        y: f64::from(r.top),
        width: f64::from(r.right - r.left),
        height: f64::from(r.bottom - r.top),
    }
}

/// Screen bounds of a window, if it exists.
pub fn window_bounds(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: `r` is a valid out pointer for the duration of the call.
    unsafe { GetWindowRect(hwnd, &raw mut r) }
        .ok()
        .map(|()| rect_to_api(&r))
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: the buffer is sized from the reported length (+1 for NUL) and
    // outlives the call.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        #[allow(clippy::cast_sign_loss)]
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        #[allow(clippy::cast_sign_loss)]
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer for the duration of the call.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    pid
}

/// Make sure keyboard input goes to `target`'s application.
///
/// Keys and text are injected into whatever window has the focus.  When another
/// *process* owns the foreground (a window that opened on top, a toast, the
/// previously focused app) the input would land in the wrong app, so `target`
/// is brought forward first.  A window of the same process (a modal Save dialog)
/// keeps the focus.  Best effort: a refusal is not an error here.
pub fn ensure_input_target(target: HWND) {
    let Some(fg) = foreground_hwnd() else {
        let _ = activate(target);
        return;
    };
    if fg != target && window_pid(fg) != window_pid(target) {
        let _ = activate(target);
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: out buffer is a u32 and the size passed matches.
    let r = unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast::<c_void>(), 4)
    };
    r.is_ok() && cloaked != 0
}

pub fn foreground_hwnd() -> Option<HWND> {
    // SAFETY: no arguments; returns a possibly-null handle.
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h)
}

/// Build a [`WindowInfo`] for a window handle.
pub fn window_info(hwnd: HWND) -> Option<WindowInfo> {
    let bounds = window_bounds(hwnd)?;
    Some(WindowInfo {
        id: id_from_hwnd(hwnd),
        title: window_title(hwnd),
        pid: window_pid(hwnd),
        bounds,
        focused: foreground_hwnd() == Some(hwnd),
    })
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` passed by `list_windows`, alive
    // for the whole synchronous EnumWindows call.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    // SAFETY: plain queries on a handle supplied by EnumWindows.
    let (visible, ex) = unsafe {
        (
            IsWindowVisible(hwnd).as_bool(),
            #[allow(clippy::cast_sign_loss)]
            {
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32
            },
        )
    };
    if visible && ex & WS_EX_TOOLWINDOW.0 == 0 {
        out.push(hwnd);
    }
    true.into()
}

/// All visible, titled, non-cloaked top-level windows.
pub fn list_windows() -> Result<Vec<WindowInfo>, DesktopError> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only dereferences the pointer we pass here, and
    // EnumWindows is synchronous so `handles` outlives it. When no top-level
    // windows exist, EnumWindows can return FALSE with ERROR_SUCCESS (0).
    unsafe {
        SetLastError(ERROR_SUCCESS);
        if let Err(e) = EnumWindows(Some(enum_proc), LPARAM((&raw mut handles) as isize))
            && !e.code().is_ok()
        {
            return Err(DesktopError::Internal(format!("EnumWindows: {e}")));
        }
    }
    Ok(handles
        .into_iter()
        .filter(|h| !is_cloaked(*h))
        .filter_map(window_info)
        .filter(|w| !w.title.is_empty())
        .collect())
}

/// Bring a window to the foreground.
///
/// Windows blocks `SetForegroundWindow` from background processes. We first
/// try it directly; if the foreground did not change, we temporarily attach
/// our input queue to the current foreground thread (which lifts the lock)
/// and retry. No synthetic keys are sent, so no access-key badges appear.
pub fn activate(hwnd: HWND) -> Result<(), DesktopError> {
    // SAFETY: handle validity is checked by the API; all calls are plain FFI.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return Err(DesktopError::Accessibility(
                "window no longer exists".into(),
            ));
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        if foreground_hwnd() != Some(hwnd)
            && let Some(fg) = foreground_hwnd()
        {
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let attached = fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        }
        if foreground_hwnd() == Some(hwnd) {
            Ok(())
        } else {
            Err(DesktopError::Accessibility(
                "SetForegroundWindow was refused".into(),
            ))
        }
    }
}

/// Quote one argument following the `CommandLineToArgvW` / MSVCRT rules:
/// backslashes are literal unless they precede a double quote, in which case
/// they must be doubled (and the quote escaped); trailing backslashes inside
/// the surrounding quotes are doubled so they do not escape the closing quote.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '\n', '\x0b', '"']) {
        return a.to_string();
    }
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const LAUNCH_IDLE_CAP_MS: u32 = 200;

/// Launch an executable (or document / app alias) via `ShellExecuteExW`.
/// No shell or PowerShell is involved. Returns the new process id, or 0 when the
/// shell handled the request without a new process (URI handlers, reused instance).
pub fn launch(app: &AppSpec) -> Result<Pid, DesktopError> {
    let file = wide(&app.executable);
    let params = wide(
        &app.args
            .iter()
            .map(|a| quote_arg(a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let verb = wide("open");
    let mut info = SHELLEXECUTEINFOW {
        #[allow(clippy::cast_possible_truncation)]
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if app.args.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(params.as_ptr())
        },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the wide strings it points to live across the call;
    // the returned process handle (if any) is closed below.
    unsafe {
        ShellExecuteExW(&raw mut info)
            .map_err(|e| DesktopError::Internal(format!("ShellExecuteEx: {e}")))?;
        if info.hProcess.is_invalid() {
            // URI / protocol launches (`ms-settings:display`, `calculator:`) and
            // apps that reuse a running instance succeed without a process handle.
            // That is a success, not an error: pid 0 means "not known".
            return Ok(0);
        }
        // Let GUI apps initialise their message loop (capped). Stub launchers
        // (`calc.exe` activating `CalculatorApp.exe`) exit within a few ms
        // without creating a window under their own pid; returning 0 lets the
        // caller match any new window.
        let idle = WaitForInputIdle(info.hProcess, LAUNCH_IDLE_CAP_MS);
        let stub_wait = if idle == 0 { 0 } else { 100 };
        let pid = if WaitForSingleObject(info.hProcess, stub_wait) == WAIT_OBJECT_0 {
            0
        } else {
            GetProcessId(info.hProcess)
        };
        let _ = CloseHandle(info.hProcess);
        Ok(pid)
    }
}

#[cfg(test)]
mod quote_tests {
    use super::quote_arg;

    #[test]
    fn plain_and_empty() {
        assert_eq!(quote_arg("abc"), "abc");
        assert_eq!(quote_arg(""), r#""""#);
        assert_eq!(quote_arg(r"C:\dir\file"), r"C:\dir\file");
    }

    #[test]
    fn trailing_backslash_is_doubled() {
        assert_eq!(quote_arg(r"C:\My Dir\"), r#""C:\My Dir\\""#);
    }

    #[test]
    fn embedded_quote_and_backslashes() {
        assert_eq!(quote_arg(r#"x" /s"#), r#""x\" /s""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }
}
