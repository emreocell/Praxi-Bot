//! `SendInput` helpers: mouse, Unicode text and key chords.
#![allow(unsafe_code)]

use desktop_api::DesktopError;
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    },
    WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

use crate::pure::{Key, is_extended, parse_key};

fn send(inputs: &[INPUT]) -> Result<(), DesktopError> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs and
    // the size argument is the size of one element.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(DesktopError::Accessibility(format!(
            "SendInput injected {sent} of {} events (blocked by UIPI or another desktop?)",
            inputs.len()
        )))
    }
}

fn kbd(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Convert physical screen pixels to the 0..=65535 virtual-desktop range.
fn to_absolute(x: f64, y: f64) -> (i32, i32) {
    // SAFETY: GetSystemMetrics takes a constant and returns an int.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let nx = (x - f64::from(vx)) * 65535.0 / f64::from(vw - 1);
    let ny = (y - f64::from(vy)) * 65535.0 / f64::from(vh - 1);
    // Clamped to the valid range first, so the cast cannot overflow.
    #[allow(clippy::cast_possible_truncation)]
    (
        nx.round().clamp(0.0, 65535.0) as i32,
        ny.round().clamp(0.0, 65535.0) as i32,
    )
}

fn move_event(x: f64, y: f64) -> INPUT {
    let (ax, ay) = to_absolute(x, y);
    mouse(
        ax,
        ay,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )
}

/// Move the real cursor to `(x, y)` and left-click.
pub fn click_at(x: f64, y: f64) -> Result<(), DesktopError> {
    send(&[
        move_event(x, y),
        mouse(0, 0, MOUSEEVENTF_LEFTDOWN),
        mouse(0, 0, MOUSEEVENTF_LEFTUP),
    ])
}

/// Press at `from`, move to `to`, release.
pub fn drag(from: (f64, f64), to: (f64, f64)) -> Result<(), DesktopError> {
    send(&[
        move_event(from.0, from.1),
        mouse(0, 0, MOUSEEVENTF_LEFTDOWN),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let moved = send(&[move_event(to.0, to.1)]);
    std::thread::sleep(std::time::Duration::from_millis(50));
    // Always try to release the button, even if the move failed.
    let up = send(&[mouse(0, 0, MOUSEEVENTF_LEFTUP)]);
    moved.and(up)
}

/// Type text as Unicode key events (layout independent).
pub fn type_text(text: &str) -> Result<(), DesktopError> {
    let mut events = Vec::new();
    for unit in text.encode_utf16() {
        events.push(kbd(0, unit, KEYEVENTF_UNICODE));
        events.push(kbd(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    send(&events)
}

fn vk_event(vk: u16, up: bool) -> INPUT {
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    kbd(vk, 0, flags)
}

/// Press a chord: all keys down in order, then released in reverse order.
pub fn key_chord(keys: &[String]) -> Result<(), DesktopError> {
    let mut parsed = Vec::with_capacity(keys.len());
    for k in keys {
        parsed.push(
            parse_key(k).ok_or_else(|| DesktopError::Unsupported(format!("unknown key '{k}'")))?,
        );
    }
    let mut events = Vec::new();
    for k in &parsed {
        match *k {
            Key::Vk(vk) => events.push(vk_event(vk, false)),
            Key::Char(c) => {
                let mut b = [0u16; 2];
                for unit in c.encode_utf16(&mut b) {
                    events.push(kbd(0, *unit, KEYEVENTF_UNICODE));
                }
            }
        }
    }
    for k in parsed.iter().rev() {
        match *k {
            Key::Vk(vk) => events.push(vk_event(vk, true)),
            Key::Char(c) => {
                let mut b = [0u16; 2];
                for unit in c.encode_utf16(&mut b) {
                    events.push(kbd(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
        }
    }
    send(&events)
}
