//! The text clipboard (Win32). Runs on the UI Automation thread.
#![allow(unsafe_code)]

use std::time::Duration;

use desktop_api::DesktopError;
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
                OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        },
    },
    core::w,
};

/// `CF_UNICODETEXT`.
const CF_UNICODETEXT: u32 = 13;
/// Another program may hold the clipboard for a moment.
const OPEN_ATTEMPTS: u32 = 10;
const OPEN_PAUSE: Duration = Duration::from_millis(25);
/// Most UTF-16 units a read copies; the tool cuts the text shorter than this.
const MAX_READ_UNITS: usize = 1 << 20;

/// An open clipboard, closed again on drop.
struct Open;

impl Open {
    fn new() -> Result<Self, DesktopError> {
        let mut last = None;
        for _ in 0..OPEN_ATTEMPTS {
            // SAFETY: opens the clipboard for the current thread; closed by `Drop`.
            match unsafe { OpenClipboard(None) } {
                Ok(()) => return Ok(Self),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(OPEN_PAUSE);
                }
            }
        }
        Err(DesktopError::Accessibility(format!(
            "the clipboard is in use by another program: {}",
            last.map_or_else(String::new, |e| e.to_string())
        )))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: closes the clipboard this thread opened.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Whether the program that copied asked clipboard tools to leave the content
/// alone (the convention password managers follow).
fn marked_private() -> bool {
    // SAFETY: registering and querying clipboard formats by name.
    unsafe {
        let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        exclude != 0 && IsClipboardFormatAvailable(exclude).is_ok()
    }
}

/// The text on the clipboard; `None` when there is none or it is marked private.
pub fn get_text() -> Result<Option<String>, DesktopError> {
    let _open = Open::new()?;
    if marked_private() {
        return Ok(None);
    }
    // SAFETY: the clipboard is open; the data handle stays owned by the clipboard and is
    // only read while locked, within the size the system reports for it.
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return Ok(None);
        }
        let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
            return Ok(None);
        };
        let global = HGLOBAL(handle.0);
        let ptr = GlobalLock(global).cast::<u16>();
        if ptr.is_null() {
            return Err(DesktopError::Accessibility(
                "the clipboard text could not be read".into(),
            ));
        }
        let units = (GlobalSize(global) / 2).min(MAX_READ_UNITS);
        let all = std::slice::from_raw_parts(ptr, units);
        let end = all.iter().position(|u| *u == 0).unwrap_or(all.len());
        let text = String::from_utf16_lossy(&all[..end]);
        let _ = GlobalUnlock(global);
        Ok(Some(text))
    }
}

/// Replace the clipboard content with `text`.
pub fn set_text(text: &str) -> Result<(), DesktopError> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = units.len() * 2;
    let _open = Open::new()?;
    let fail = |what: &str, e: windows::core::Error| {
        DesktopError::Accessibility(format!("clipboard {what} failed: {e}"))
    };
    // SAFETY: the clipboard is open. The block is allocated here, filled while locked and
    // handed to the clipboard, which owns it after a successful `SetClipboardData`; on a
    // failure it is freed here.
    unsafe {
        EmptyClipboard().map_err(|e| fail("clear", e))?;
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| fail("allocation", e))?;
        let ptr = GlobalLock(global).cast::<u16>();
        if ptr.is_null() {
            let _ = GlobalFree(Some(global));
            return Err(DesktopError::Accessibility(
                "clipboard memory could not be locked".into(),
            ));
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
        let _ = GlobalUnlock(global);
        if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(global.0))) {
            let _ = GlobalFree(Some(global));
            return Err(fail("write", e));
        }
    }
    Ok(())
}
