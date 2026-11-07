//! Platform-independent parts of the Linux backend: session detection (X11 vs
//! Wayland), AT-SPI role/state/action mapping, tree filtering, X11 keysym
//! parsing, coordinate denormalisation, and `ZPixmap` pixel conversion.
//!
//! Compiled on every OS so that all pure logic is unit-testable on Windows,
//! macOS, and Linux.

use desktop_api::{
    DesktopError,
    snapshot::{Rect, UiNode},
};

/// Maximum number of nodes kept in one snapshot.
pub const MAX_NODES: usize = 2500;
/// Maximum tree depth walked.
pub const MAX_DEPTH: usize = 40;

// ── Display & Wayland detection ───────────────────────────────────────────────

/// Validate the current Linux display session from environment variables.
///
/// Wayland is out of scope for v0 (compositor security policies block global
/// `XTest` input injection and root-window `XGetImage`); when a Wayland session
/// is detected, this returns [`DesktopError::Unsupported`] with a clear
/// explanation and remediation hint.
///
/// # Errors
///
/// - [`DesktopError::Unsupported`] when running under Wayland.
/// - [`DesktopError::Accessibility`] when no X11 `DISPLAY` is set.
pub fn detect_display_session(
    xdg_session_type: Option<&str>,
    wayland_display: Option<&str>,
    x11_display: Option<&str>,
) -> Result<String, DesktopError> {
    let session = xdg_session_type.map(str::trim).filter(|s| !s.is_empty());
    let wayland = wayland_display.map(str::trim).filter(|s| !s.is_empty());
    let x11 = x11_display.map(str::trim).filter(|s| !s.is_empty());

    if let Some(s) = session
        && s.eq_ignore_ascii_case("wayland")
    {
        return Err(DesktopError::Unsupported(
            "Wayland session detected (XDG_SESSION_TYPE=wayland): Wayland desktop automation \
             is out of scope for v0 because compositor security policies restrict global input \
             injection (XTest) and window-level screen capture (XGetImage). Run under an X11 \
             session or start a headless Xvfb session via scripts/linux/setup-headless.sh"
                .into(),
        ));
    }

    let explicit_x11 = session.is_some_and(|s| s.eq_ignore_ascii_case("x11"));
    if let Some(w) = wayland
        && !explicit_x11
    {
        return Err(DesktopError::Unsupported(format!(
            "Wayland display '{w}' detected (WAYLAND_DISPLAY={w}): Wayland desktop automation \
             is out of scope for v0 because compositor security policies restrict global input \
             injection and screen capture. Use an X11 session (or set XDG_SESSION_TYPE=x11 \
             when running under Xvfb; see scripts/linux/setup-headless.sh)"
        )));
    }

    match x11 {
        Some(display) => Ok(display.to_string()),
        None => Err(DesktopError::Accessibility(
            "no X11 DISPLAY environment variable is set; start Xvfb (e.g. DISPLAY=:99) or \
             run scripts/linux/setup-headless.sh"
                .into(),
        )),
    }
}

// ── AT-SPI role & action mapping ──────────────────────────────────────────────

/// Map an AT-SPI role name (from `atspi::Role::name()`) to the canonical
/// lowercase role string shared with the Windows backend.
#[must_use]
pub fn role_name(atspi_role_name: &str, is_editable: bool) -> &'static str {
    let lower = atspi_role_name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "button" | "push button" | "toggle button" => "button",
        "push button menu" => "splitbutton",
        "check box" | "switch" => "checkbox",
        "combo box" => "combobox",
        "entry" | "password text" | "editbar" | "date editor" | "terminal" => "edit",
        "text" | "paragraph" if is_editable => "edit",
        "text" | "label" | "accelerator label" | "static" | "paragraph" | "heading" | "caption"
        | "comment" | "definition" | "description term" | "description value" | "footnote"
        | "block quote" | "log" => "text",
        "link" => "hyperlink",
        "list item" => "listitem",
        "list" | "list box" | "autocomplete" => "list",
        "menu" | "popup menu" => "menu",
        "menu bar" => "menubar",
        "menu item" | "check menu item" | "radio menu item" | "tearoff menu item" => "menuitem",
        "progress bar" | "level bar" => "progressbar",
        "radio button" => "radiobutton",
        "scroll bar" => "scrollbar",
        "slider" | "dial" => "slider",
        "spin button" => "spinner",
        "status bar" => "statusbar",
        "page tab list" => "tab",
        "page tab" => "tabitem",
        "tool bar" => "toolbar",
        "tool tip" => "tooltip",
        "tree" => "tree",
        "tree item" => "treeitem",
        "tree table" => "datagrid",
        "table" => "table",
        "table cell" | "table row" => "dataitem",
        "column header" | "row header" | "table column header" | "table row header" => "headeritem",
        "document frame"
        | "document web"
        | "document text"
        | "document spreadsheet"
        | "document presentation"
        | "document email"
        | "html container"
        | "article"
        | "page" => "document",
        "window" | "frame" | "dialog" | "alert" | "file chooser" | "color chooser"
        | "font chooser" | "application" | "notification" | "info bar" => "window",
        "panel" | "filler" | "scroll pane" | "split pane" | "viewport" | "layered pane"
        | "root pane" | "glass pane" | "option pane" | "directory pane" | "internal frame"
        | "desktop frame" | "embedded" => "pane",
        "grouping" | "form" | "section" | "landmark" | "description list" => "group",
        "title bar" => "titlebar",
        "header" | "footer" => "header",
        "separator" => "separator",
        "calendar" => "calendar",
        "image" | "icon" | "animation" | "desktop icon" | "image map" | "chart" => "image",
        "canvas" | "drawing area" | "extended" => "custom",
        _ => "unknown",
    }
}

/// Roles an agent can usefully act on (mirrors `desktop-windows`).
const INTERACTIVE: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "edit",
    "hyperlink",
    "listitem",
    "menuitem",
    "radiobutton",
    "tabitem",
    "treeitem",
    "slider",
    "spinner",
    "splitbutton",
    "document",
    "dataitem",
    "headeritem",
    "scrollbar",
    "thumb",
];

/// Roles kept as context only when they carry a name (mirrors `desktop-windows`).
const CONTEXT: &[&str] = &[
    "text",
    "group",
    "pane",
    "window",
    "list",
    "tab",
    "toolbar",
    "menu",
    "menubar",
    "tree",
    "datagrid",
    "table",
    "statusbar",
    "titlebar",
    "header",
    "appbar",
];

/// Action names in priority order when invoking an element via AT-SPI `Action`.
const ACTION_PRIORITY: &[&str] = &[
    "click",
    "press",
    "activate",
    "toggle",
    "jump",
    "open",
    "select",
    "expand or contract",
];

/// Pick the best 0-based AT-SPI action index to invoke for a click.
///
/// Prefers well-known activation verbs (`click`, `press`, `activate`, `toggle`,
/// `jump`, `open`, `select`) and falls back to index `0` (the AT-SPI default
/// action) when at least one action is exposed.
#[must_use]
pub fn preferred_action_index(actions: &[String]) -> Option<i32> {
    if actions.is_empty() {
        return None;
    }
    for &wanted in ACTION_PRIORITY {
        if let Some(pos) = actions
            .iter()
            .position(|a| a.trim().eq_ignore_ascii_case(wanted))
        {
            return i32::try_from(pos).ok();
        }
    }
    Some(0)
}

/// One element as read from AT-SPI, before filtering.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct RawNode {
    pub role: &'static str,
    pub name: String,
    pub value: Option<String>,
    pub bounds: Option<Rect>,
    pub offscreen: bool,
    pub enabled: bool,
    pub focused: bool,
    pub password: bool,
    /// Extra states derived from AT-SPI `StateSet` (`checked`, `selected`, ...).
    pub pattern_states: Vec<&'static str>,
    /// Action names exposed by `org.a11y.atspi.Action` (e.g. `["click"]`).
    pub actions: Vec<String>,
}

impl RawNode {
    fn is_interactive(&self) -> bool {
        INTERACTIVE.contains(&self.role)
            || (!CONTEXT.contains(&self.role)
                && self.actions.iter().any(|a| {
                    let lower = a.trim().to_ascii_lowercase();
                    ACTION_PRIORITY.contains(&lower.as_str())
                }))
    }
}

/// Decide whether a raw node is kept. Root nodes are always kept.
#[must_use]
pub fn keep(node: &RawNode, is_root: bool) -> bool {
    if is_root {
        return true;
    }
    if node.offscreen && !node.focused {
        return false;
    }
    let context = CONTEXT.contains(&node.role);
    // Containers inherit keyboard focus from their children; only the
    // focused leaf is interesting, so a focused wrapper is not kept for that.
    if node.is_interactive() || (node.focused && !context) {
        return true;
    }
    context && !node.name.trim().is_empty()
}

/// Select the source indices to keep (first `max` kept nodes, in order).
/// Element 0 is treated as the root.
#[must_use]
pub fn select(raw: &[RawNode], max: usize) -> Vec<usize> {
    raw.iter()
        .enumerate()
        .filter(|(i, n)| keep(n, *i == 0))
        .map(|(i, _)| i)
        .take(max)
        .collect()
}

/// Convert a raw node into the public [`UiNode`]. Password fields lose their
/// value and gain the `secure` state.
#[must_use]
pub fn to_ui_node(index: u32, raw: &RawNode) -> UiNode {
    let mut states: Vec<String> = Vec::new();
    if raw.focused {
        states.push("focused".into());
    }
    if !raw.enabled {
        states.push("disabled".into());
    }
    if raw.password {
        states.push("secure".into());
    }
    states.extend(raw.pattern_states.iter().map(|s| (*s).to_string()));
    // A value identical to the name (tree items, list items) is pure noise.
    let value = if raw.password {
        None
    } else {
        raw.value.clone().filter(|v| *v != raw.name)
    };
    UiNode::new(index, raw.role, raw.name.clone(), value, raw.bounds, states)
}

// ── Keys & X11 keysyms ────────────────────────────────────────────────────────

/// A parsed key token for X11 input injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// An X11 keysym (`XK_*`).
    Keysym(u32),
    /// A literal Unicode character.
    Char(char),
}

/// Convert a Unicode character into an X11 keysym.
///
/// Follows the X11 protocol specification:
/// - Control characters `\n` / `\r` → `XK_Return` (`0xFF0D`), `\t` → `XK_Tab` (`0xFF09`)
/// - Printable ASCII `0x20..=0x7E` and Latin-1 `0xA0..=0xFF` map 1:1
/// - All other Unicode code points `U+0100..=U+10FFFF` map to `0x0100_0000 | codepoint`
#[must_use]
pub fn char_to_keysym(c: char) -> u32 {
    match c {
        '\n' | '\r' => 0xFF0D,
        '\t' => 0xFF09,
        '\u{0008}' => 0xFF08,
        '\u{001B}' => 0xFF1B,
        c if ('\u{0020}'..='\u{007E}').contains(&c) || ('\u{00A0}'..='\u{00FF}').contains(&c) => {
            u32::from(c)
        }
        c => 0x0100_0000 | u32::from(c),
    }
}

/// Parse a key name such as `"ctrl"`, `"Return"`, `"F5"` or `"a"`.
#[must_use]
pub fn parse_key(name: &str) -> Option<Key> {
    let lower = name.trim().to_lowercase();
    let sym: u32 = match lower.as_str() {
        "ctrl" | "control" | "lctrl" => 0xFFE3,       // XK_Control_L
        "rctrl" => 0xFFE4,                            // XK_Control_R
        "shift" | "lshift" => 0xFFE1,                 // XK_Shift_L
        "rshift" => 0xFFE2,                           // XK_Shift_R
        "alt" | "menu" | "option" | "lalt" => 0xFFE9, // XK_Alt_L
        "ralt" | "altgr" => 0xFFEA,                   // XK_Alt_R
        "win" | "windows" | "cmd" | "meta" | "super" | "lwin" => 0xFFEB, // XK_Super_L
        "rwin" | "rsuper" => 0xFFEC,                  // XK_Super_R
        "enter" | "return" => 0xFF0D,                 // XK_Return
        "tab" => 0xFF09,                              // XK_Tab
        "esc" | "escape" => 0xFF1B,                   // XK_Escape
        "space" => 0x0020,                            // XK_space
        "backspace" => 0xFF08,                        // XK_BackSpace
        "delete" | "del" => 0xFFFF,                   // XK_Delete
        "insert" | "ins" => 0xFF63,                   // XK_Insert
        "home" => 0xFF50,                             // XK_Home
        "end" => 0xFF57,                              // XK_End
        "pageup" | "pgup" => 0xFF55,                  // XK_Page_Up
        "pagedown" | "pgdn" => 0xFF56,                // XK_Page_Down
        "left" => 0xFF51,                             // XK_Left
        "up" => 0xFF52,                               // XK_Up
        "right" => 0xFF53,                            // XK_Right
        "down" => 0xFF54,                             // XK_Down
        _ => 0,
    };
    if sym != 0 {
        return Some(Key::Keysym(sym));
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some(Key::Keysym(0xFFBE + n - 1)); // XK_F1..=XK_F24
    }
    let trimmed = name.trim();
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => {
            Some(Key::Keysym(u32::from(c.to_ascii_lowercase() as u8)))
        }
        (Some(c), None) if c.is_ascii_digit() => Some(Key::Keysym(u32::from(c as u8))),
        (Some(c), None) => Some(Key::Char(c)),
        _ => None,
    }
}

/// Convert a normalised 0-999 coordinate over `rect` to absolute pixels.
#[must_use]
pub fn denormalize(x: f64, y: f64, rect: &Rect) -> (f64, f64) {
    (
        rect.x + x.clamp(0.0, 999.0) / 999.0 * rect.width,
        rect.y + y.clamp(0.0, 999.0) / 999.0 * rect.height,
    )
}

// ── X11 capture helpers ───────────────────────────────────────────────────────

/// Clamp an arbitrary capture rectangle to the root screen dimensions so
/// `GetImage` never fails with `BadMatch` on partially off-screen windows.
///
/// # Errors
///
/// Returns [`DesktopError::Screenshot`] if the clamped region is empty.
pub fn clamp_capture_rect(
    rect: &Rect,
    screen_w: u32,
    screen_h: u32,
) -> Result<(i16, i16, u16, u16), DesktopError> {
    #[allow(clippy::cast_possible_truncation)]
    let x0 = rect.x.round() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let y0 = rect.y.round() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let x1 = (rect.x + rect.width).round() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let y1 = (rect.y + rect.height).round() as i64;

    let sw = i64::from(screen_w);
    let sh = i64::from(screen_h);

    let cx0 = x0.clamp(0, sw);
    let cy0 = y0.clamp(0, sh);
    let cx1 = x1.clamp(0, sw);
    let cy1 = y1.clamp(0, sh);

    let w = cx1 - cx0;
    let h = cy1 - cy0;
    if w <= 0 || h <= 0 {
        return Err(DesktopError::Screenshot("empty capture region".into()));
    }

    let left = i16::try_from(cx0)
        .map_err(|_| DesktopError::Screenshot("capture X coordinate out of range".into()))?;
    let top = i16::try_from(cy0)
        .map_err(|_| DesktopError::Screenshot("capture Y coordinate out of range".into()))?;
    let width = u16::try_from(w)
        .map_err(|_| DesktopError::Screenshot("capture width out of range".into()))?;
    let height = u16::try_from(h)
        .map_err(|_| DesktopError::Screenshot("capture height out of range".into()))?;

    Ok((left, top, width, height))
}

/// Convert raw X11 `ZPixmap` image bytes into tightly-packed 32-bit `BGRA`
/// with opaque alpha (`255`).
///
/// Supports 24/32-bit (`4` bytes/pixel `BGRX`/`BGRA`), packed 24-bit (`3`
/// bytes/pixel `BGR`), and 16-bit `RGB565` (`2` bytes/pixel).
///
/// # Errors
///
/// Returns [`DesktopError::Screenshot`] if the buffer is smaller than expected
/// for the given dimensions and depth.
pub fn zpixmap_to_bgra(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u8,
) -> Result<Vec<u8>, DesktopError> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| DesktopError::Screenshot("image dimensions overflow".into()))?;
    if pixels == 0 {
        return Err(DesktopError::Screenshot("empty image".into()));
    }

    if data.len() >= pixels * 4 {
        let mut out = data[..pixels * 4].to_vec();
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        return Ok(out);
    }

    if data.len() >= pixels * 3 && (depth == 24 || depth == 32) {
        let mut out = Vec::with_capacity(pixels * 4);
        for px in data[..pixels * 3].as_chunks::<3>().0 {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        return Ok(out);
    }

    if data.len() >= pixels * 2 && depth == 16 {
        let mut out = Vec::with_capacity(pixels * 4);
        for px in data[..pixels * 2].as_chunks::<2>().0 {
            let v = u16::from_le_bytes([px[0], px[1]]);
            let r5 = ((v >> 11) & 0x1F) as u8;
            let g6 = ((v >> 5) & 0x3F) as u8;
            let b5 = (v & 0x1F) as u8;
            let r = (r5 << 3) | (r5 >> 2);
            let g = (g6 << 2) | (g6 >> 4);
            let b = (b5 << 3) | (b5 >> 2);
            out.extend_from_slice(&[b, g, r, 255]);
        }
        return Ok(out);
    }

    Err(DesktopError::Screenshot(format!(
        "unsupported ZPixmap buffer length {} for {}x{} depth {}",
        data.len(),
        width,
        height,
        depth
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(role: &'static str, name: &str) -> RawNode {
        RawNode {
            role,
            name: name.into(),
            value: None,
            bounds: None,
            offscreen: false,
            enabled: true,
            focused: false,
            password: false,
            pattern_states: vec![],
            actions: vec![],
        }
    }

    #[test]
    fn wayland_session_is_rejected_with_unsupported_error() {
        let err = detect_display_session(Some("wayland"), Some("wayland-0"), Some(":0"))
            .expect_err("wayland must be rejected");
        assert!(matches!(err, DesktopError::Unsupported(_)));
        assert!(err.to_string().contains("Wayland"));

        let err2 = detect_display_session(None, Some("wayland-0"), Some(":0"))
            .expect_err("WAYLAND_DISPLAY without explicit x11 must be rejected");
        assert!(matches!(err2, DesktopError::Unsupported(_)));
    }

    #[test]
    fn x11_and_xvfb_sessions_are_accepted() {
        assert_eq!(
            detect_display_session(Some("x11"), None, Some(":99")).unwrap(),
            ":99"
        );
        assert_eq!(
            detect_display_session(None, None, Some(":0")).unwrap(),
            ":0"
        );
        // Explicit XDG_SESSION_TYPE=x11 overrides a leaked WAYLAND_DISPLAY under xvfb-run.
        assert_eq!(
            detect_display_session(Some("x11"), Some("wayland-0"), Some(":99")).unwrap(),
            ":99"
        );
    }

    #[test]
    fn missing_display_returns_accessibility_error() {
        let err = detect_display_session(None, None, None).expect_err("missing DISPLAY");
        assert!(matches!(err, DesktopError::Accessibility(_)));
    }

    #[test]
    fn role_names_match_windows_conventions() {
        assert_eq!(role_name("push button", false), "button");
        assert_eq!(role_name("button", false), "button");
        assert_eq!(role_name("check box", false), "checkbox");
        assert_eq!(role_name("switch", false), "checkbox");
        assert_eq!(role_name("entry", false), "edit");
        assert_eq!(role_name("password text", false), "edit");
        assert_eq!(role_name("text", false), "text");
        assert_eq!(role_name("text", true), "edit");
        assert_eq!(role_name("frame", false), "window");
        assert_eq!(role_name("page tab", false), "tabitem");
        assert_eq!(role_name("nonexistent", false), "unknown");
    }

    #[test]
    fn drops_empty_wrappers_and_offscreen() {
        assert!(!keep(&raw("pane", ""), false));
        assert!(!keep(&raw("group", "  "), false));
        assert!(keep(&raw("group", "Toolbar"), false));
        assert!(keep(&raw("button", ""), false));
        let mut off = raw("button", "Hidden");
        off.offscreen = true;
        assert!(!keep(&off, false));
        off.focused = true;
        assert!(keep(&off, false));
    }

    #[test]
    fn custom_node_with_click_action_is_interactive() {
        let mut custom = raw("custom", "Canvas Widget");
        assert!(!keep(&custom, false));
        custom.actions = vec!["click".into()];
        assert!(keep(&custom, false));
    }

    #[test]
    fn preferred_action_index_picks_click_or_default() {
        assert_eq!(preferred_action_index(&[]), None);
        assert_eq!(
            preferred_action_index(&["menu".into(), "click".into()]),
            Some(1)
        );
        assert_eq!(preferred_action_index(&["custom-action".into()]), Some(0));
    }

    #[test]
    fn root_always_kept_and_cap_applies() {
        let mut v = vec![raw("pane", "")];
        v.extend((0..10).map(|i| raw("button", &format!("b{i}"))));
        let sel = select(&v, 4);
        assert_eq!(sel, vec![0, 1, 2, 3]);
    }

    #[test]
    fn password_is_secure_without_value() {
        let mut r = raw("edit", "Password");
        r.password = true;
        r.value = Some("secret".into());
        let n = to_ui_node(1, &r);
        assert!(n.value.is_none());
        assert!(n.states.contains(&"secure".to_string()));
    }

    #[test]
    fn value_equal_to_name_is_dropped() {
        let mut r = raw("treeitem", "Desktop");
        r.value = Some("Desktop".into());
        assert!(to_ui_node(1, &r).value.is_none());
    }

    #[test]
    fn states_collected() {
        let mut r = raw("checkbox", "Wrap");
        r.focused = true;
        r.enabled = false;
        r.pattern_states = vec!["checked"];
        let n = to_ui_node(2, &r);
        assert_eq!(n.states, vec!["focused", "disabled", "checked"]);
    }

    #[test]
    fn key_and_keysym_parsing() {
        assert_eq!(parse_key("Ctrl"), Some(Key::Keysym(0xFFE3)));
        assert_eq!(parse_key("Return"), Some(Key::Keysym(0xFF0D)));
        assert_eq!(parse_key("F5"), Some(Key::Keysym(0xFFC2)));
        assert_eq!(parse_key("A"), Some(Key::Keysym(u32::from(b'a'))));
        assert_eq!(parse_key("7"), Some(Key::Keysym(u32::from(b'7'))));
        assert_eq!(parse_key("ü"), Some(Key::Char('ü')));
        assert_eq!(char_to_keysym('a'), u32::from(b'a'));
        assert_eq!(char_to_keysym('ş'), 0x0100_0000 | u32::from('ş'));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key("f25"), None);
    }

    #[test]
    fn normalized_coords_and_capture_clamp() {
        let r = Rect {
            x: 100.0,
            y: 200.0,
            width: 999.0,
            height: 999.0,
        };
        assert_eq!(denormalize(0.0, 999.0, &r), (100.0, 1199.0));
        assert_eq!(denormalize(5000.0, 0.0, &r), (1099.0, 200.0));

        let overhanging = Rect {
            x: -20.0,
            y: 10.0,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(
            clamp_capture_rect(&overhanging, 1920, 1080).unwrap(),
            (0, 10, 180, 100)
        );
    }

    #[test]
    fn zpixmap_conversions() {
        let raw32 = vec![10, 20, 30, 0, 40, 50, 60, 128];
        let bgra32 = zpixmap_to_bgra(&raw32, 2, 1, 24).unwrap();
        assert_eq!(bgra32, vec![10, 20, 30, 255, 40, 50, 60, 255]);

        let raw24 = vec![10, 20, 30, 40, 50, 60];
        let bgra24 = zpixmap_to_bgra(&raw24, 2, 1, 24).unwrap();
        assert_eq!(bgra24, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }
}
