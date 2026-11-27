//! The bot cursor as a layered Win32 window.
//!
//! A small square popup (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE |
//! WS_EX_TOOLWINDOW`) that follows the bot's point: click-through (a transparent layered window
//! is skipped by hit testing), always on top, never activated (shown with `SW_SHOWNOACTIVATE`
//! and positioned with `SWP_NOACTIVATE`) and not in the task bar. Its pixels come from
//! [`desktop_api::cursor::sprite`] and are put on screen with `UpdateLayeredWindow` from a
//! premultiplied-alpha DIB. It is excluded from screen capture where Windows allows it
//! (`WDA_EXCLUDEFROMCAPTURE`), so the live viewer's frames do not contain it twice: the viewer
//! draws its own from the frame metadata.
//!
//! The window lives on the worker thread of [`desktop_api::cursor::ThreadedSurface`], which also
//! calls [`OverlayWindow::pump`] so the window never stops answering messages.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::Arc;

use desktop_api::cursor::geometry::dpi_scale;
use desktop_api::cursor::sprite::{Bitmap, canvas_size};
use desktop_api::cursor::{CursorSurface, OverlayWindow, ThreadedSurface};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
            HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForSystem,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_TOPMOST,
                MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowDisplayAffinity, SetWindowPos,
                ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE, WNDCLASSW,
                WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::w,
};

/// A cursor surface over the interactive desktop.
///
/// # Errors
/// The window could not be created.
pub fn open() -> Result<Arc<dyn CursorSurface>, String> {
    // SAFETY: a plain query with no arguments.
    let dpi = unsafe { GetDpiForSystem() };
    let scale = dpi_scale(dpi);
    let surface = ThreadedSurface::spawn("win-cursor", scale, move || {
        Ok(Box::new(Win32Overlay::new(canvas_size(scale))?) as Box<dyn OverlayWindow>)
    })?;
    Ok(Arc::new(surface))
}

struct Win32Overlay {
    hwnd: HWND,
    mem_dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    side: u32,
    visible: bool,
}

// SAFETY: the overlay is created and used on one thread only (ThreadedSurface's worker); the raw
// pointer is the DIB's pixel memory, owned by `dib`, and the handles are plain integers.
unsafe impl Send for Win32Overlay {}

/// The window has no behaviour of its own: every message gets the default handling.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwards the arguments Windows gave this procedure, unchanged.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn err(what: &str, e: impl std::fmt::Display) -> String {
    format!("cursor overlay: {what}: {e}")
}

impl Win32Overlay {
    fn new(side: u32) -> Result<Self, String> {
        // SAFETY: every call below is a plain Win32 call on handles created here; failures are
        // returned as errors and nothing created before a failure is leaked past `new`.
        unsafe {
            let hinst = GetModuleHandleW(None).map_err(|e| err("module handle", e))?;
            let class = w!("PraxiBotCursor");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            // Registering twice (a second surface) fails harmlessly with "class exists".
            let _ = RegisterClassW(&raw const wc);
            let side_i = i32::try_from(side).unwrap_or(72);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                side_i,
                side_i,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .map_err(|e| err("create window", e))?;
            // Best effort (Windows 10 2004+): keep it out of screen captures.
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            let screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen));
            let _ = ReleaseDC(None, screen);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
                    biWidth: side_i,
                    biHeight: -side_i, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = match CreateDIBSection(
                Some(mem_dc),
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            ) {
                Ok(d) if !bits.is_null() => d,
                other => {
                    let _ = DeleteDC(mem_dc);
                    let _ = DestroyWindow(hwnd);
                    return Err(err(
                        "create bitmap",
                        other
                            .err()
                            .map_or_else(|| "no pixels".to_owned(), |e| e.to_string()),
                    ));
                }
            };
            let old = SelectObject(mem_dc, dib.into());
            Ok(Self {
                hwnd,
                mem_dc,
                dib,
                old,
                bits: bits.cast(),
                side,
                visible: false,
            })
        }
    }
}

impl OverlayWindow for Win32Overlay {
    fn place(&mut self, origin: (i32, i32), bitmap: &Bitmap) -> Result<(), String> {
        let want = (self.side as usize) * (self.side as usize) * 4;
        if bitmap.width != self.side || bitmap.height != self.side || bitmap.bgra.len() != want {
            return Err(err("bitmap", "size does not match the window"));
        }
        // SAFETY: `bits` points at `want` writable bytes owned by `dib` (a side x side 32-bit
        // DIB section), which lives as long as `self`; the source slice has exactly `want` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bitmap.bgra.as_ptr(), self.bits, want);
        }
        let dst = POINT {
            x: origin.0,
            y: origin.1,
        };
        let src = POINT { x: 0, y: 0 };
        let size = SIZE {
            cx: i32::try_from(self.side).unwrap_or(0),
            cy: i32::try_from(self.side).unwrap_or(0),
        };
        let blend = BLENDFUNCTION {
            #[allow(clippy::cast_possible_truncation)] // the flag is 0 or 1
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            #[allow(clippy::cast_possible_truncation)] // the flag is 0 or 1
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: all pointers are to locals that outlive the call; `hwnd` and `mem_dc` are ours.
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&raw const dst),
                Some(&raw const size),
                Some(self.mem_dc),
                Some(&raw const src),
                COLORREF(0),
                Some(&raw const blend),
                ULW_ALPHA,
            )
            .map_err(|e| err("update", e))?;
            if self.visible {
                // Stay above windows that were raised since, without ever taking focus.
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.visible = true;
            }
        }
        Ok(())
    }

    fn hide(&mut self) -> Result<(), String> {
        if self.visible {
            // SAFETY: `hwnd` is our window.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.visible = false;
        }
        Ok(())
    }

    fn pump(&mut self) {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out parameter; only this thread's messages are taken.
        unsafe {
            while PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = DispatchMessageW(&raw const msg);
            }
        }
    }
}

impl Drop for Win32Overlay {
    fn drop(&mut self) {
        // SAFETY: destroying the objects this value created, once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.mem_dc);
        }
    }
}
