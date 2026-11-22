//! `SendInput` helpers: mouse, Unicode text and key chords.
#![allow(unsafe_code)]

use desktop_api::DesktopError;
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS,
        KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
        VIRTUAL_KEY,
    },
    WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

use crate::live_view_win::is_win_extended_vk;
use crate::pure::{Key, parse_key};

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
        // A blocked send can stop midway. Do not leave a chord's keys held down.
        release_stuck_keys(&inputs[..(sent as usize).min(inputs.len())]);
        Err(DesktopError::Accessibility(format!(
            "SendInput injected {sent} of {} events (blocked by UIPI or another desktop?)",
            inputs.len()
        )))
    }
}

/// Key-ups, in reverse order, for every virtual key that `done` pressed and
/// did not release. Unicode text events carry no held state.
fn stuck_key_ups(done: &[INPUT]) -> Vec<INPUT> {
    let mut held: Vec<(u16, bool)> = Vec::new();
    for e in done {
        if e.r#type != INPUT_KEYBOARD {
            continue;
        }
        // SAFETY: `r#type` says this is the keyboard variant of the union.
        let ki = unsafe { e.Anonymous.ki };
        if ki.wVk.0 == 0 || ki.dwFlags.contains(KEYEVENTF_UNICODE) {
            continue;
        }
        if ki.dwFlags.contains(KEYEVENTF_KEYUP) {
            if let Some(pos) = held.iter().rposition(|(vk, _)| *vk == ki.wVk.0) {
                held.remove(pos);
            }
        } else {
            held.push((ki.wVk.0, ki.dwFlags.contains(KEYEVENTF_EXTENDEDKEY)));
        }
    }
    held.iter()
        .rev()
        .map(|(vk, _)| vk_event(*vk, true))
        .collect()
}

fn release_stuck_keys(done: &[INPUT]) {
    let ups = stuck_key_ups(done);
    if ups.is_empty() {
        return;
    }
    // SAFETY: a valid slice of initialised INPUT structs; best effort, the
    // result is ignored because there is nothing further to do on failure.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let _ = unsafe { SendInput(&ups, std::mem::size_of::<INPUT>() as i32) };
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
    mouse_with_data(dx, dy, flags, 0)
}

fn mouse_with_data(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Move to a screen point and scroll in both requested directions.
pub fn scroll_at((x, y): (f64, f64), dx: f64, dy: f64) -> Result<(), DesktopError> {
    if !dx.is_finite() || !dy.is_finite() {
        return Err(DesktopError::Unsupported(
            "scroll deltas must be finite".into(),
        ));
    }
    let mut events = vec![move_event(x, y)];
    for (delta, flag) in [(dx, MOUSEEVENTF_HWHEEL), (-dy, MOUSEEVENTF_WHEEL)] {
        #[allow(clippy::cast_possible_truncation)]
        let ticks = (delta.clamp(-50.0, 50.0) * 120.0).round() as i32;
        if ticks != 0 {
            events.push(mouse_with_data(
                0,
                0,
                flag,
                u32::from_ne_bytes(ticks.to_ne_bytes()),
            ));
        }
    }
    if events.len() == 1 {
        return Ok(());
    }
    send(&events)
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

/// Type UTF-16 units as Unicode key events (one `SendInput`).
pub fn type_units(units: &[u16]) -> Result<(), DesktopError> {
    let mut events = Vec::with_capacity(units.len() * 2);
    for unit in units {
        events.push(kbd(0, *unit, KEYEVENTF_UNICODE));
        events.push(kbd(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    send(&events)
}

/// Virtual keys that can change what the next key means.
const MODIFIER_VKS_ALL: [u16; 11] = [
    0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
];

fn key_is_down(vk: u16) -> bool {
    // SAFETY: plain query by value.
    unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
}

/// Ctrl, Alt, Shift or Win (either side) is down right now.
pub fn modifiers_down() -> bool {
    MODIFIER_VKS_ALL.iter().any(|vk| key_is_down(*vk))
}

/// Release every modifier and the left mouse button, then verify with the
/// async key state that no modifier is left down. `true` = confirmed clean.
/// A key the user is holding also makes this `false`: the grant then asks.
pub fn release_all() -> bool {
    let mut ups: Vec<INPUT> = MODIFIER_VKS_ALL
        .iter()
        .map(|vk| vk_event(*vk, true))
        .collect();
    ups.push(mouse(0, 0, MOUSEEVENTF_LEFTUP));
    // SAFETY: a valid slice of initialised INPUT structs. The result is not
    // trusted: the key state below is what decides.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let _ = unsafe { SendInput(&ups, std::mem::size_of::<INPUT>() as i32) };
    !modifiers_down()
}

fn vk_event(vk: u16, up: bool) -> INPUT {
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if is_win_extended_vk(vk) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_send_releases_the_keys_it_already_pressed() {
        // ctrl down, shift down, then the send stopped before the key-ups.
        let done = [
            vk_event(0x11, false),
            vk_event(0x10, false),
            kbd(0, u16::from(b'x'), KEYEVENTF_UNICODE),
        ];
        let ups = stuck_key_ups(&done);
        let vks: Vec<(u16, bool)> = ups
            .iter()
            .map(|e| {
                // SAFETY: built by `vk_event`, the keyboard variant.
                let ki = unsafe { e.Anonymous.ki };
                (ki.wVk.0, ki.dwFlags.contains(KEYEVENTF_KEYUP))
            })
            .collect();
        assert_eq!(
            vks,
            vec![(0x10, true), (0x11, true)],
            "reverse order, key-ups"
        );
    }

    #[test]
    fn nothing_is_released_when_every_press_was_released() {
        let done = [
            vk_event(0x11, false),
            vk_event(0x41, false),
            vk_event(0x41, true),
            vk_event(0x11, true),
        ];
        assert!(stuck_key_ups(&done).is_empty());
    }
}
