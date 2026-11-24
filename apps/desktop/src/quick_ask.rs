//! "Quick ask": an opt-in global hotkey that opens a small always-on-top window
//! with a single question box.
//!
//! The window (label `quick-ask`) is built the first time the hotkey is used, never at
//! start-up, and only while the user turned the feature on in Settings. It may call exactly
//! four commands (see `capabilities/quick-ask.json`): get the daemon connection (URL and
//! token, never the owner key), hide itself, hand a chat over to the main window, and nothing
//! else. The clipboard is never read here: the window reads it only after the user presses
//! the explicit "Include clipboard" button.
//!
//! This file holds the pure decisions (hotkey grammar, reserved combinations, window
//! placement) so they are unit-tested without a display; the OS parts are at the bottom.

use std::fmt;

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, Window,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::control::ControlHandle;
use crate::daemon::{self, Connection, DaemonHandle};
use crate::window_gate::{MAIN_ONLY, QUICK_ASK_ONLY, require_window};

/// The window label; the capability file and the window gate use the same string.
pub const WINDOW_LABEL: &str = "quick-ask";
/// Sent to the window each time it is shown, so it can clear the old input state.
pub const EVENT_SHOWN: &str = "quick-ask:shown";
/// Default hotkey (also the UI default).
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";

/// Logical size of the window.
const WIDTH: f64 = 600.0;
const HEIGHT: f64 = 380.0;

/// Why a hotkey is refused. The UI turns [`HotkeyProblem::code`] into a plain sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyProblem {
    /// Nothing, or a key name we do not know.
    Invalid,
    /// No Ctrl / Alt / Super: a bare key would hijack normal typing.
    NeedsModifier,
    /// Only Shift plus a typing key: it would break capital letters.
    ShiftOnly,
    /// A combination every app relies on (copy, paste, Alt+Tab, ...).
    Reserved,
    /// The same keys as the Stop hotkey.
    SameAsStop,
    /// Another program already owns it (reported by the OS when registering).
    InUse,
}

impl HotkeyProblem {
    /// Stable code the UI maps to a translated message.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NeedsModifier => "needs_modifier",
            Self::ShiftOnly => "shift_only",
            Self::Reserved => "reserved",
            Self::SameAsStop => "same_as_stop",
            Self::InUse => "in_use",
        }
    }
}

impl fmt::Display for HotkeyProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const SUPER: u8 = 8;

/// A validated hotkey in canonical spelling (`Ctrl+Alt+Space`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    /// Bit set of [`CTRL`], [`ALT`], [`SHIFT`], [`SUPER`].
    mods: u8,
    key: String,
}

impl Hotkey {
    /// Canonical text, modifiers in a fixed order (`Ctrl+Alt+Shift+Super+Key`).
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for (bit, name) in [
            (CTRL, "Ctrl"),
            (ALT, "Alt"),
            (SHIFT, "Shift"),
            (SUPER, "Super"),
        ] {
            if self.mods & bit != 0 {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }
}

/// Combinations that are never allowed: the user needs them elsewhere.
const RESERVED: &[&str] = &[
    "Ctrl+A",
    "Ctrl+C",
    "Ctrl+V",
    "Ctrl+X",
    "Ctrl+Y",
    "Ctrl+Z",
    "Ctrl+S",
    "Ctrl+W",
    "Ctrl+Tab",
    "Alt+Tab",
    "Alt+F4",
    "Alt+Space",
];

fn normalize_key(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if lower == "space" {
        return Some("Space".to_owned());
    }
    if lower == "tab" {
        return Some("Tab".to_owned());
    }
    if let Some(n) = lower.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Some(format!("F{n}"));
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase().to_string()),
        _ => None,
    }
}

/// Parse and validate a hotkey; `stop_hotkey` is the Stop hotkey's text (it is never taken over).
///
/// # Errors
/// The first [`HotkeyProblem`] found.
pub fn parse_hotkey(spec: &str, stop_hotkey: &str) -> Result<Hotkey, HotkeyProblem> {
    let hotkey = parse_unchecked(spec)?;
    if hotkey.mods & (CTRL | ALT | SUPER) == 0 {
        return Err(if hotkey.mods & SHIFT != 0 {
            HotkeyProblem::ShiftOnly
        } else {
            HotkeyProblem::NeedsModifier
        });
    }
    let canonical = hotkey.canonical();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&canonical)) {
        return Err(HotkeyProblem::Reserved);
    }
    if parse_unchecked(stop_hotkey).is_ok_and(|stop| stop == hotkey) {
        return Err(HotkeyProblem::SameAsStop);
    }
    Ok(hotkey)
}

fn parse_unchecked(spec: &str) -> Result<Hotkey, HotkeyProblem> {
    let mut hotkey = Hotkey {
        mods: 0,
        key: String::new(),
    };
    for part in spec.split('+').map(str::trim) {
        let bit = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => CTRL,
            "alt" | "option" => ALT,
            "shift" => SHIFT,
            "super" | "win" | "windows" | "meta" | "cmd" | "command" => SUPER,
            _ => 0,
        };
        if bit != 0 {
            if hotkey.mods & bit != 0 {
                return Err(HotkeyProblem::Invalid); // the same modifier twice
            }
            hotkey.mods |= bit;
        } else if hotkey.key.is_empty() {
            hotkey.key = normalize_key(part).ok_or(HotkeyProblem::Invalid)?;
        } else {
            return Err(HotkeyProblem::Invalid); // two keys
        }
    }
    if hotkey.key.is_empty() {
        return Err(HotkeyProblem::Invalid);
    }
    Ok(hotkey)
}

/// A screen rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// Where the top-left corner of a `size` window goes: centred on the cursor, or in the upper
/// third of the screen when the cursor is unknown, and always fully inside `screen`.
#[must_use]
pub fn place_window(cursor: Option<(f64, f64)>, size: (f64, f64), screen: Rect) -> (f64, f64) {
    let (cx, cy) = cursor.unwrap_or((screen.x + screen.w / 2.0, screen.y + screen.h / 3.0));
    let x = (cx - size.0 / 2.0).clamp(screen.x, (screen.x + screen.w - size.0).max(screen.x));
    let y = (cy - size.1 / 3.0).clamp(screen.y, (screen.y + screen.h - size.1).max(screen.y));
    (x, y)
}

/// Whether a chat id handed to "Open in app" looks like the daemon's ids (a UUID); anything
/// else is dropped so a compromised page cannot push arbitrary text into the main window.
#[must_use]
pub fn plausible_chat_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

// ── OS side ──────────────────────────────────────────────────────────────────

/// The currently registered shortcut, if any.
#[derive(Default)]
pub struct QuickAskState {
    current: Mutex<Option<Shortcut>>,
}

/// Build the plugin. Its handler only toggles the window; no JavaScript permission of the
/// plugin is granted to any window (registration happens in Rust).
#[must_use]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = tokio::task::spawn_blocking(move || toggle(&app)).await;
                });
            }
        })
        .build()
}

/// Turn the hotkey on (registering `hotkey`) or off. Returns the canonical text.
///
/// # Errors
/// A [`HotkeyProblem`] code; the previous hotkey stays off in that case.
pub fn configure(
    app: &AppHandle,
    state: &QuickAskState,
    enabled: bool,
    hotkey: &str,
    stop_hotkey: &str,
) -> Result<Option<String>, HotkeyProblem> {
    let shortcuts = app.global_shortcut();
    if let Some(old) = state.current.lock().take() {
        let _ = shortcuts.unregister(old);
    }
    if !enabled {
        if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
            let _ = w.hide();
        }
        return Ok(None);
    }
    let parsed = parse_hotkey(hotkey, stop_hotkey)?;
    let canonical = parsed.canonical();
    let shortcut: Shortcut = canonical.parse().map_err(|_| HotkeyProblem::Invalid)?;
    shortcuts
        .register(shortcut)
        .map_err(|_| HotkeyProblem::InUse)?;
    *state.current.lock() = Some(shortcut);
    Ok(Some(canonical))
}

fn screen_of(app: &AppHandle, cursor: Option<(f64, f64)>) -> Option<Rect> {
    let monitor = cursor
        .and_then(|(x, y)| app.monitor_from_point(x, y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())?;
    let pos = monitor.position();
    let size = monitor.size();
    Some(Rect {
        x: f64::from(pos.x),
        y: f64::from(pos.y),
        w: f64::from(size.width),
        h: f64::from(size.height),
    })
}

fn build_window(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    tauri::WebviewWindowBuilder::new(
        app,
        WINDOW_LABEL,
        WebviewUrl::App("index.html?window=quick-ask".into()),
    )
    .title("Praxi-Bot Quick ask")
    .inner_size(WIDTH, HEIGHT)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false)
    .build()
}

/// Show the window near the cursor, or hide it when it is already open. Runs off the main
/// thread (building a window on it can dead-lock on Windows).
pub fn toggle(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL)
        && w.is_visible().unwrap_or(false)
    {
        let _ = w.hide();
        return;
    }
    let window = match app
        .get_webview_window(WINDOW_LABEL)
        .map_or_else(|| build_window(app), Ok)
    {
        Ok(w) => w,
        Err(e) => {
            eprintln!("bot-desktop: quick ask window failed: {e}");
            return;
        }
    };
    let cursor = app.cursor_position().ok().map(|p| (p.x, p.y));
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Some(screen) = screen_of(app, cursor) {
        let size = (WIDTH * scale, HEIGHT * scale);
        let (x, y) = place_window(cursor, size, screen);
        #[allow(clippy::cast_possible_truncation)]
        let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let _ = window.set_size(PhysicalSize::new(size.0 as u32, size.1 as u32));
    }
    let _ = window.show();
    let _ = window.set_focus();
    let _ = app.emit_to(WINDOW_LABEL, EVENT_SHOWN, ());
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Turn the Quick ask hotkey on or off. Main window only (Settings).
///
/// # Errors
/// Another window called it, or the hotkey is refused; the text is a stable code (`invalid`,
/// `needs_modifier`, `shift_only`, `reserved`, `same_as_stop`, `in_use`).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window and state over by value
pub fn quick_ask_configure(
    window: Window,
    app: AppHandle,
    state: State<'_, QuickAskState>,
    control: State<'_, ControlHandle>,
    enabled: bool,
    hotkey: String,
) -> Result<Option<String>, String> {
    require_window(&window, MAIN_ONLY)?;
    let stop = control.snapshot().stop_hotkey;
    configure(&app, &state, enabled, &hotkey, &stop).map_err(|p| p.code().to_owned())
}

/// The daemon address and token for the Quick ask window. Never the owner key: chat and
/// message routes take the plain token, and owner-only routes stay out of reach.
///
/// # Errors
/// Another window called it, or the service cannot be reached.
#[tauri::command]
pub async fn quick_ask_connect(
    window: Window,
    daemon: State<'_, Arc<DaemonHandle>>,
) -> Result<Connection, String> {
    require_window(&window, QUICK_ASK_ONLY)?;
    daemon::connect(&daemon).await
}

/// Hide the Quick ask window (Esc, or after "Open in app").
///
/// # Errors
/// Another window called it.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window over by value
pub fn quick_ask_hide(window: Window) -> Result<(), String> {
    require_window(&window, QUICK_ASK_ONLY)?;
    window.hide().map_err(|e| e.to_string())
}

/// Continue a chat in the main window: show it and open that chat (the same event a clicked
/// notification uses), then hide the Quick ask window.
///
/// # Errors
/// Another window called it.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window and handle over by value
pub fn quick_ask_open_in_app(
    window: Window,
    app: AppHandle,
    chat_id: Option<String>,
) -> Result<(), String> {
    require_window(&window, QUICK_ASK_ONLY)?;
    let chat_id = chat_id.filter(|id| plausible_chat_id(id));
    crate::shell::focus_main(&app);
    let _ = app.emit_to(
        "main",
        crate::shell::EVENT_NOTIFICATION_ACTIVATED,
        crate::shell::ActivatedPayload { chat_id },
    );
    window.hide().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOP: &str = "Ctrl+Alt+Esc";

    #[test]
    fn the_default_is_valid_and_spelled_canonically() {
        let h = parse_hotkey(" control + ALT + space ", STOP).expect("default");
        assert_eq!(h.canonical(), DEFAULT_HOTKEY);
    }

    #[test]
    fn unsafe_hotkeys_are_refused_with_a_reason() {
        let code = |s: &str| parse_hotkey(s, STOP).unwrap_err().code();
        assert_eq!(code(""), "invalid");
        assert_eq!(code("Ctrl+Alt"), "invalid");
        assert_eq!(code("Ctrl+A+B"), "invalid");
        assert_eq!(code("Ctrl+Ctrl+K"), "invalid");
        assert_eq!(code("Space"), "needs_modifier");
        assert_eq!(code("F8"), "needs_modifier");
        assert_eq!(code("Shift+K"), "shift_only");
        assert_eq!(code("ctrl+c"), "reserved");
        assert_eq!(code("Alt+Tab"), "reserved");
    }

    #[test]
    fn the_stop_hotkey_is_never_taken_over() {
        assert_eq!(
            parse_hotkey("alt+ctrl+f9", "Ctrl+Alt+F9").unwrap_err(),
            HotkeyProblem::SameAsStop
        );
        assert!(parse_hotkey("Ctrl+Alt+F9", "not a hotkey").is_ok());
    }

    #[test]
    fn every_canonical_hotkey_is_understood_by_the_shortcut_plugin() {
        for spec in ["Ctrl+Alt+Space", "Ctrl+Shift+K", "Alt+F12", "Ctrl+Alt+7"] {
            let canonical = parse_hotkey(spec, STOP).expect(spec).canonical();
            assert!(canonical.parse::<Shortcut>().is_ok(), "{canonical}");
        }
    }

    #[test]
    fn the_window_stays_on_the_screen_near_the_cursor() {
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let size = (600.0, 380.0);
        // In the middle: centred on the cursor.
        assert_eq!(place_window(Some((960.0, 500.0)), size, screen).0, 660.0);
        // In a corner: pushed back inside.
        assert_eq!(
            place_window(Some((1919.0, 1079.0)), size, screen),
            (1320.0, 700.0)
        );
        assert_eq!(place_window(Some((0.0, 0.0)), size, screen), (0.0, 0.0));
        // Unknown cursor: horizontally centred in the upper part.
        let (x, y) = place_window(None, size, screen);
        assert_eq!(x, 660.0);
        assert!(y < 540.0);
        // A second monitor to the right keeps the window on that monitor.
        let right = Rect {
            x: 1920.0,
            ..screen
        };
        assert!(place_window(Some((2000.0, 100.0)), size, right).0 >= 1920.0);
    }

    #[test]
    fn only_uuid_shaped_chat_ids_are_handed_to_the_main_window() {
        assert!(plausible_chat_id("0b6c1f0e-2f4b-4c52-9a0e-1a2b3c4d5e6f"));
        assert!(!plausible_chat_id(""));
        assert!(!plausible_chat_id("<script>alert(1)</script>"));
        assert!(!plausible_chat_id("0b6c1f0e-2f4b-4c52-9a0e-1a2b3c4d5e6"));
    }
}
