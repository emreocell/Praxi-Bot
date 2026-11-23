//! GUI logoff and installer shutdown, separate from the owner's close-to-tray choice.
#![allow(unsafe_code)]

use std::sync::{Arc, OnceLock};
use tauri::Manager;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::WM_ENDSESSION,
    },
};

use crate::daemon::DaemonHandle;

static DAEMON: OnceLock<Arc<DaemonHandle>> = OnceLock::new();

unsafe extern "system" fn session_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_ENDSESSION
        && wparam.0 != 0
        && let Some(daemon) = DAEMON.get()
    {
        daemon.stop();
    }
    // SAFETY: forward every message to the next subclass in the same UI thread.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub fn install(
    app: &tauri::AppHandle,
    daemon: Arc<DaemonHandle>,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = process_guard::supervision::StopEvent::new()?;
    if let Some(window) = app.get_webview_window("main") {
        let hwnd = window.hwnd()?;
        let _ = DAEMON.set(Arc::clone(&daemon));
        // SAFETY: setup runs on the UI thread; the main HWND remains owned by Tauri.
        if !unsafe { SetWindowSubclass(HWND(hwnd.0), Some(session_proc), 604, 0) }.as_bool() {
            return Err("Praxi could not register Windows sign-out handling".into());
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if event.wait() {
            daemon.stop();
            app.exit(0);
        }
    });
    Ok(())
}
