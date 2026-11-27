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

/// The 0-based AT-SPI action that expands (`want_expanded`) or collapses an
/// element. Toolkits differ: some name two actions, GTK trees expose one that
/// flips (`expand or contract`). The caller has already checked the live state,
/// so a flipping action is only run when a flip is what is needed.
#[must_use]
pub fn expand_action_index(actions: &[String], want_expanded: bool) -> Option<i32> {
    let wanted: &[&str] = if want_expanded {
        &["expand", "open"]
    } else {
        &["collapse", "contract", "close"]
    };
    let flipping = ["expand or contract", "toggle expand", "toggle"];
    let find = |names: &[&str]| {
        actions.iter().position(|a| {
            let a = a.trim();
            names.iter().any(|n| a.eq_ignore_ascii_case(n))
        })
    };
    find(wanted)
        .or_else(|| find(&flipping))
        .and_then(|i| i32::try_from(i).ok())
}

/// The 0-based AT-SPI action that selects an element, if it names one.
#[must_use]
pub fn select_action_index(actions: &[String]) -> Option<i32> {
    actions
        .iter()
        .position(|a| a.trim().eq_ignore_ascii_case("select"))
        .and_then(|i| i32::try_from(i).ok())
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

/// Select the source indices to keep, in document order. When more than `max`
/// nodes qualify, the root and the focused node come first, then enabled
/// interactive nodes, then the rest; the result is still sorted by position.
/// Element 0 is treated as the root.
#[must_use]
pub fn select(raw: &[RawNode], max: usize) -> Vec<usize> {
    let kept: Vec<usize> = raw
        .iter()
        .enumerate()
        .filter(|(i, n)| keep(n, *i == 0))
        .map(|(i, _)| i)
        .collect();
    if kept.len() <= max {
        return kept;
    }
    let tier = |i: usize| -> u8 {
        let n = &raw[i];
        if i == 0 || n.focused {
            0
        } else if n.enabled && n.is_interactive() {
            1
        } else {
            2
        }
    };
    let mut ranked = kept;
    // Stable sort keeps document order inside each tier.
    ranked.sort_by_key(|&i| tier(i));
    ranked.truncate(max);
    ranked.sort_unstable();
    ranked
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
        "space" | "spacebar" => 0x0020,               // XK_space
        "backspace" => 0xFF08,                        // XK_BackSpace
        "delete" | "del" => 0xFFFF,                   // XK_Delete
        "insert" | "ins" => 0xFF63,                   // XK_Insert
        "home" => 0xFF50,                             // XK_Home
        "end" => 0xFF57,                              // XK_End
        "pageup" | "pgup" | "page_up" | "page-up" => 0xFF55, // XK_Page_Up
        "pagedown" | "pgdn" | "page_down" | "page-down" => 0xFF56, // XK_Page_Down
        "left" | "arrow_left" | "arrowleft" | "arrow-left" => 0xFF51, // XK_Left
        "up" | "arrow_up" | "arrowup" | "arrow-up" => 0xFF52, // XK_Up
        "right" | "arrow_right" | "arrowright" | "arrow-right" => 0xFF53, // XK_Right
        "down" | "arrow_down" | "arrowdown" | "arrow-down" => 0xFF54, // XK_Down
        "caps" | "capslock" | "caps_lock" => 0xFFE5,  // XK_Caps_Lock
        "print" | "printscreen" | "print_screen" | "prtsc" => 0xFF61, // XK_Print
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

// ── Errors the model can act on ───────────────────────────────────────────────

/// `X server not reachable at :99: …` — the phrase tools and doctor share.
#[must_use]
pub fn x_unreachable(display: &str, cause: &str) -> String {
    format!("X server not reachable at {display}: {cause}")
}

/// Caller-side deadline for an X11 or AT-SPI operation that would otherwise hang.
#[must_use]
pub fn op_timed_out(op: &str, display: &str, limit: std::time::Duration) -> String {
    x_unreachable(
        display,
        &format!("{op} timed out after {}s", limit.as_secs().max(1)),
    )
}

// ── Capability probe that does not pin a failure for the process lifetime ───

/// How long a failed display probe is trusted before it is tried again.
pub const PROBE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(15);

/// Positive X reachability sticks; a negative result expires after [`PROBE_RETRY_AFTER`].
#[derive(Debug)]
pub struct ProbeCache {
    reachable: bool,
    failed_at: Option<std::time::Instant>,
}

impl ProbeCache {
    /// Empty cache: the next call probes.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reachable: false,
            failed_at: None,
        }
    }

    /// Return the cached reachability, probing only when the last failure is stale.
    #[must_use]
    pub fn get(&mut self, now: std::time::Instant, probe: impl FnOnce() -> bool) -> bool {
        if self.reachable {
            return true;
        }
        if self
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < PROBE_RETRY_AFTER)
        {
            return false;
        }
        if probe() {
            self.reachable = true;
        } else {
            self.failed_at = Some(now);
        }
        self.reachable
    }
}

impl Default for ProbeCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── AT-SPI extents → X11 pixels (the space screenshots and XTest use) ────────

/// Maps a point from one rectangle's coordinate space into another's.
///
/// Identity (`scale` 1, origin 0) leaves coordinates unchanged. A GDK scale of
/// 2 produces `scale` 0.5 so a click lands on the screenshot pixel.
#[derive(Debug, Clone, Copy)]
pub struct AxisMap {
    /// Multiplier from source X to target X.
    pub scale_x: f64,
    /// Multiplier from source Y to target Y.
    pub scale_y: f64,
    /// Source origin (AT-SPI frame).
    pub from_x: f64,
    /// Source origin (AT-SPI frame).
    pub from_y: f64,
    /// Target origin (X11 window).
    pub to_x: f64,
    /// Target origin (X11 window).
    pub to_y: f64,
}

impl AxisMap {
    /// No scaling and no shift.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            from_x: 0.0,
            from_y: 0.0,
            to_x: 0.0,
            to_y: 0.0,
        }
    }

    /// Map one point.
    #[must_use]
    pub fn map_point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.to_x + (x - self.from_x) * self.scale_x,
            self.to_y + (y - self.from_y) * self.scale_y,
        )
    }

    /// Map a rectangle's origin and size.
    #[must_use]
    pub fn map_rect(self, rect: Rect) -> Rect {
        let (x, y) = self.map_point(rect.x, rect.y);
        Rect {
            x,
            y,
            width: rect.width * self.scale_x,
            height: rect.height * self.scale_y,
        }
    }
}

impl Default for AxisMap {
    fn default() -> Self {
        Self::identity()
    }
}

/// Build the map from an AT-SPI frame to the X11 window that owns the screenshot.
///
/// Returns [`AxisMap::identity`] when the two rects already agree, when the
/// aspect change looks like window decorations rather than DPI, or when the
/// ratio is outside the range a toolkit scale factor takes (about 0.4–2.6).
#[must_use]
pub fn align_atspi_to_x11(atspi: &Rect, x11: &Rect) -> AxisMap {
    if atspi.width < 1.0 || atspi.height < 1.0 || x11.width < 1.0 || x11.height < 1.0 {
        return AxisMap::identity();
    }
    let sx = x11.width / atspi.width;
    let sy = x11.height / atspi.height;
    if !sx.is_finite() || !sy.is_finite() || sx <= 0.0 || sy <= 0.0 {
        return AxisMap::identity();
    }
    let rel = (sx - sy).abs() / sx.max(sy);
    if rel > 0.15 || !(0.4..=2.6).contains(&sx) {
        return AxisMap::identity();
    }
    // Close to 1:1 is decoration or rounding, never a toolkit scale factor.
    // AT-SPI screen coordinates are already X pixels then; do not squash them.
    if (sx - 1.0).abs() < 0.15 && (sy - 1.0).abs() < 0.15 {
        return AxisMap::identity();
    }
    AxisMap {
        scale_x: sx,
        scale_y: sy,
        from_x: atspi.x,
        from_y: atspi.y,
        to_x: x11.x,
        to_y: x11.y,
    }
}

// ── Keymap lookup (level 0, shifted level 1, or a scratch keysym bind) ───────

/// Where a keysym lives in the server keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPlace {
    /// Produced by `keycode` with no modifier.
    Level0 { keycode: u8 },
    /// Produced by `keycode` while Shift is held.
    Level1 { keycode: u8 },
    /// Not in the map. The caller binds `keysym` onto a free keycode.
    Bind { keysym: u32 },
}

/// Find `keysym` in a `GetKeyboardMapping` reply.
///
/// `keysyms` is the flat list, `per` symbols per keycode, starting at `min_kc`.
#[must_use]
pub fn find_keysym(keysym: u32, min_kc: u8, max_kc: u8, per: u8, keysyms: &[u32]) -> KeyPlace {
    let per_n = usize::from(per.max(1));
    let mut level1 = None;
    for (idx, chunk) in keysyms.chunks(per_n).enumerate() {
        let Ok(offset) = u8::try_from(idx) else {
            break;
        };
        let kc = min_kc.saturating_add(offset);
        if kc > max_kc {
            break;
        }
        if chunk.first().copied() == Some(keysym) {
            return KeyPlace::Level0 { keycode: kc };
        }
        if level1.is_none() && chunk.get(1).copied() == Some(keysym) {
            level1 = Some(kc);
        }
    }
    if let Some(keycode) = level1 {
        return KeyPlace::Level1 { keycode };
    }
    KeyPlace::Bind { keysym }
}

/// Keycodes whose symbols are all `NoSymbol` (`0`), skipping the reserved code 8.
#[must_use]
pub fn free_keycodes(min_kc: u8, max_kc: u8, per: u8, keysyms: &[u32], need: usize) -> Vec<u8> {
    if need == 0 {
        return Vec::new();
    }
    let per_n = usize::from(per.max(1));
    let mut out = Vec::new();
    for (idx, chunk) in keysyms.chunks(per_n).enumerate() {
        let Ok(offset) = u8::try_from(idx) else {
            break;
        };
        let kc = min_kc.saturating_add(offset);
        if kc > max_kc {
            break;
        }
        if kc == 8 {
            continue;
        }
        if chunk.iter().all(|&sym| sym == 0) {
            out.push(kc);
            if out.len() == need {
                break;
            }
        }
    }
    out
}

/// One step of a planned `XTest` typing sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeAction {
    /// Install `keysym` on `keycode` (both shift levels).
    Bind { keycode: u8, keysym: u32 },
    /// Key down.
    Down { keycode: u8 },
    /// Key up.
    Up { keycode: u8 },
    /// Clear a scratch keycode after the string has been sent.
    Unbind { keycode: u8 },
}

/// Plan keystrokes so every character is either a real keymap key (with Shift
/// when it sits on level 1) or one shared scratch binding for the whole string.
/// Scratch keys are unbound only after the last character, which avoids the
/// race of clearing a keysym before the client has read the event.
///
/// # Errors
///
/// Returns an error when Shift is required but missing, or when the keymap has
/// no free keycode for a character that is not on the current layout.
pub fn plan_text_actions(
    steps: &[KeyPlace],
    shift_keycode: Option<u8>,
    free_keycodes: &[u8],
) -> Result<Vec<TypeAction>, String> {
    let mut assigned: Vec<(u32, u8)> = Vec::new();
    let mut next_free = 0usize;
    let mut binds = Vec::new();
    for step in steps {
        if let KeyPlace::Bind { keysym } = *step {
            if assigned.iter().any(|(sym, _)| *sym == keysym) {
                continue;
            }
            let Some(&keycode) = free_keycodes.get(next_free) else {
                return Err(format!(
                    "cannot type keysym {keysym:#x}: the X keymap has no free keycode for a character outside the current layout"
                ));
            };
            next_free += 1;
            assigned.push((keysym, keycode));
            binds.push(TypeAction::Bind { keycode, keysym });
        }
    }
    let mut actions = binds;
    for step in steps {
        match *step {
            KeyPlace::Level0 { keycode } => {
                actions.push(TypeAction::Down { keycode });
                actions.push(TypeAction::Up { keycode });
            }
            KeyPlace::Level1 { keycode } => {
                let Some(shift) = shift_keycode else {
                    return Err(
                        "Shift is not on the X keymap, so a shifted character cannot be typed"
                            .into(),
                    );
                };
                actions.push(TypeAction::Down { keycode: shift });
                actions.push(TypeAction::Down { keycode });
                actions.push(TypeAction::Up { keycode });
                actions.push(TypeAction::Up { keycode: shift });
            }
            KeyPlace::Bind { keysym } => {
                let keycode = assigned
                    .iter()
                    .find(|(sym, _)| *sym == keysym)
                    .map(|(_, keycode)| *keycode)
                    .ok_or_else(|| format!("scratch keysym {keysym:#x} was not assigned"))?;
                actions.push(TypeAction::Down { keycode });
                actions.push(TypeAction::Up { keycode });
            }
        }
    }
    for (_, keycode) in assigned {
        actions.push(TypeAction::Unbind { keycode });
    }
    Ok(actions)
}

/// Process start time (clock ticks since boot) from the text of `/proc/<pid>/stat`.
/// The command name sits in parentheses and may hold spaces and `)`, so the
/// fields are counted from the LAST `)`. `None` when absent or zero: a zero
/// start time is "unknown" and never names a process.
#[must_use]
pub fn parse_proc_start_time(stat: &str) -> Option<u64> {
    let (_, after_comm) = stat.rsplit_once(')')?;
    // Field 3 (the state) is the first one after the command; the start time is field 22.
    after_comm
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
        .filter(|t| *t != 0)
}

/// Real and effective user id from the text of `/proc/<pid>/status`.
#[must_use]
pub fn parse_proc_uids(status: &str) -> Option<(u32, u32)> {
    let line = status.lines().find_map(|l| l.strip_prefix("Uid:"))?;
    let mut ids = line.split_whitespace().map(str::parse::<u32>);
    let real = ids.next()?.ok()?;
    let effective = ids.next()?.ok()?;
    Some((real, effective))
}

/// Whether a program runs outside the user's own account: any of its user ids
/// differs from the daemon's effective user id (root's programs, another user's).
#[must_use]
pub fn runs_outside_own_account(uids: (u32, u32), own_euid: u32) -> bool {
    uids.0 != own_euid || uids.1 != own_euid
}

/// The window class out of a `WM_CLASS` property (`instance\0Class\0`): the class
/// part, or the instance when there is no second part. Empty when unreadable.
#[must_use]
pub fn parse_wm_class(raw: &[u8]) -> String {
    let mut parts = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).trim().to_owned());
    let first = parts.next();
    parts.next().or(first).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn expand_and_collapse_pick_the_named_action_or_a_flipping_one() {
        use super::{expand_action_index, select_action_index};
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(expand_action_index(&v(&["click", "expand"]), true), Some(1));
        assert_eq!(
            expand_action_index(&v(&["click", "collapse"]), false),
            Some(1)
        );
        assert_eq!(
            expand_action_index(&v(&["Expand or contract"]), true),
            Some(0)
        );
        assert_eq!(
            expand_action_index(&v(&["Expand or contract"]), false),
            Some(0)
        );
        // A named expand never collapses: no action for the other direction.
        assert_eq!(expand_action_index(&v(&["expand"]), false), None);
        assert_eq!(expand_action_index(&v(&["click"]), true), None);
        assert_eq!(select_action_index(&v(&["click", "Select"])), Some(1));
        assert_eq!(select_action_index(&v(&["click"])), None);
    }

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
    fn select_prefers_focus_and_interactive_nodes_over_text_when_capped() {
        let mut v = vec![raw("pane", "")];
        v.extend((0..3000).map(|i| raw("text", &format!("t{i}"))));
        v.extend((0..1998).map(|i| raw("button", &format!("b{i}"))));
        let mut focused = raw("edit", "Search");
        focused.focused = true;
        v.push(focused);
        assert_eq!(v.len(), 5000);
        let sel = select(&v, MAX_NODES);
        assert_eq!(sel.len(), MAX_NODES);
        assert!(sel.contains(&0));
        assert!(sel.contains(&4999), "focused node kept");
        for i in 3001..4999 {
            assert!(sel.contains(&i), "interactive node {i} kept");
        }
        assert!(sel.windows(2).all(|w| w[0] < w[1]), "document order");
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
        assert_eq!(parse_key("page_up"), Some(Key::Keysym(0xFF55)));
        assert_eq!(parse_key("arrow_left"), Some(Key::Keysym(0xFF51)));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key("f25"), None);
    }

    #[test]
    fn turkish_letters_use_unicode_keysyms_when_they_are_outside_latin1() {
        // ö ü ç are Latin-1 and map 1:1. ğ ş ı İ do not, so they use the X11
        // unicode keysym (0x01000000 | code point) instead of the active layout.
        for c in ['ö', 'ü', 'ç'] {
            assert_eq!(char_to_keysym(c), u32::from(c), "{c}");
        }
        for c in ['ğ', 'ş', 'ı', 'İ'] {
            assert_eq!(char_to_keysym(c), 0x0100_0000 | u32::from(c), "{c}");
        }
        assert_eq!(parse_key("İ"), Some(Key::Char('İ')));
        assert_eq!(parse_key("ı"), Some(Key::Char('ı')));
    }

    #[test]
    fn keymap_lookup_shifts_ascii_and_binds_turkish_letters() {
        // One key: level 0 = 'a', level 1 = 'A'. Then an empty keycode.
        let min = 10u8;
        let per = 2u8;
        let map = vec![u32::from(b'a'), u32::from(b'A'), 0, 0];
        assert_eq!(
            find_keysym(u32::from(b'a'), min, 20, per, &map),
            KeyPlace::Level0 { keycode: 10 }
        );
        assert_eq!(
            find_keysym(u32::from(b'A'), min, 20, per, &map),
            KeyPlace::Level1 { keycode: 10 }
        );
        let g = char_to_keysym('ğ');
        assert_eq!(
            find_keysym(g, min, 20, per, &map),
            KeyPlace::Bind { keysym: g }
        );
        let free = free_keycodes(min, 20, per, &map, 4);
        assert_eq!(free, vec![11]);
        let steps = [
            KeyPlace::Level0 { keycode: 10 },
            KeyPlace::Level1 { keycode: 10 },
            KeyPlace::Bind { keysym: g },
            KeyPlace::Bind { keysym: g },
        ];
        let actions = plan_text_actions(&steps, Some(50), &free).unwrap();
        assert_eq!(
            actions.first().copied(),
            Some(TypeAction::Bind {
                keycode: 11,
                keysym: g
            })
        );
        assert!(
            actions
                .iter()
                .filter(|a| matches!(a, TypeAction::Bind { .. }))
                .count()
                == 1,
            "one scratch binding is shared by both ğ"
        );
        assert_eq!(
            actions.last().copied(),
            Some(TypeAction::Unbind { keycode: 11 })
        );
        let err = plan_text_actions(&[KeyPlace::Level1 { keycode: 10 }], None, &[]).unwrap_err();
        assert!(err.contains("Shift"), "{err}");
    }

    #[test]
    fn atspi_extents_scale_onto_the_x11_screenshot() {
        let atspi = Rect {
            x: 0.0,
            y: 0.0,
            width: 2560.0,
            height: 1600.0,
        };
        let x11 = Rect {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 800.0,
        };
        let map = align_atspi_to_x11(&atspi, &x11);
        let (x, y) = map.map_point(200.0, 100.0);
        assert!(
            (x - 100.0).abs() < 0.01 && (y - 50.0).abs() < 0.01,
            "{x},{y}"
        );
        let same = Rect {
            x: 10.0,
            y: 20.0,
            width: 1280.0,
            height: 800.0,
        };
        let id = align_atspi_to_x11(&same, &same);
        assert!((id.scale_x - 1.0).abs() < f64::EPSILON);
        let (px, py) = id.map_point(30.0, 40.0);
        assert!((px - 30.0).abs() < f64::EPSILON && (py - 40.0).abs() < f64::EPSILON);
        // A title-bar-sized disagreement (sy far from sx) is not a DPI scale.
        let decorated = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 900.0,
        };
        let client = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let near = Rect {
            x: 0.0,
            y: 40.0,
            width: 800.0,
            height: 560.0,
        };
        let (x, y) = align_atspi_to_x11(&client, &near).map_point(10.0, 20.0);
        assert!(
            (x - 10.0).abs() < f64::EPSILON && (y - 20.0).abs() < f64::EPSILON,
            "a 7% height difference is a title bar, not a scale: {x},{y}"
        );
        let (x, y) = align_atspi_to_x11(&client, &decorated).map_point(10.0, 20.0);
        assert!(
            (x - 10.0).abs() < f64::EPSILON && (y - 20.0).abs() < f64::EPSILON,
            "{x},{y}"
        );
    }

    #[test]
    fn unreachable_x_names_the_display_and_a_failure_is_probed_again() {
        assert_eq!(
            x_unreachable(":99", "connection refused"),
            "X server not reachable at :99: connection refused"
        );
        let msg = op_timed_out("screenshot", ":99", std::time::Duration::from_secs(8));
        assert!(msg.contains("X server not reachable at :99"), "{msg}");
        assert!(msg.contains("screenshot"), "{msg}");

        let mut cache = ProbeCache::new();
        let t0 = std::time::Instant::now();
        assert!(!cache.get(t0, || false));
        assert!(!cache.get(t0 + std::time::Duration::from_secs(1), || {
            panic!("a fresh failure must be reused")
        }));
        assert!(cache.get(t0 + PROBE_RETRY_AFTER, || true));
        assert!(cache.get(t0 + PROBE_RETRY_AFTER * 4, || { panic!("success sticks") }));
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

    #[test]
    fn the_start_time_is_field_22_even_when_the_command_name_has_spaces_and_parentheses() {
        let stat = "4242 (my (odd) app) S 1 4242 4242 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 7 0 987654 1000 200 18446744073709551615";
        assert_eq!(parse_proc_start_time(stat), Some(987_654));
        assert_eq!(
            parse_proc_start_time("1 (init) S 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 12 0 0"),
            Some(12)
        );
        // Unknown is not a start time.
        assert_eq!(
            parse_proc_start_time("1 (x) S 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0"),
            None
        );
        assert_eq!(parse_proc_start_time("1 (x) S 0 1"), None);
        assert_eq!(parse_proc_start_time(""), None);
    }

    #[test]
    fn user_ids_and_the_own_account_test() {
        let status = "Name:\tgedit\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(parse_proc_uids(status), Some((1000, 1000)));
        assert_eq!(parse_proc_uids("Name:\tx\n"), None);
        assert_eq!(parse_proc_uids("Uid:\tabc\t1\n"), None);
        assert!(!runs_outside_own_account((1000, 1000), 1000));
        assert!(runs_outside_own_account((0, 0), 1000), "root's program");
        assert!(runs_outside_own_account((1000, 0), 1000), "set-uid root");
        assert!(runs_outside_own_account((1001, 1001), 1000), "another user");
    }

    #[test]
    fn the_window_class_is_the_second_part_of_wm_class() {
        assert_eq!(parse_wm_class(b"gedit\0Gedit\0"), "Gedit");
        assert_eq!(parse_wm_class(b"xterm\0"), "xterm");
        assert_eq!(parse_wm_class(b""), "");
        assert_eq!(parse_wm_class(b"\0\0"), "");
    }
}

#[cfg(test)]
mod key_properties {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::sample::select;
    use proptest::test_runner::RngSeed;

    use super::{Key, parse_key};

    const NAMED_KEYS: &[&str] = &[
        "enter",
        "return",
        "tab",
        "esc",
        "escape",
        "space",
        "backspace",
        "delete",
        "del",
        "insert",
        "ins",
        "home",
        "end",
        "pageup",
        "pgup",
        "pagedown",
        "pgdn",
        "left",
        "up",
        "right",
        "down",
    ];
    // Modifier spellings accepted by desktop-windows/src/pure.rs::parse_key.
    const WINDOWS_MODIFIERS: &[&str] = &[
        "ctrl", "control", "lctrl", "rctrl", "shift", "lshift", "rshift", "alt", "menu", "option",
        "lalt", "ralt", "altgr", "win", "windows", "cmd", "meta", "super", "lwin", "rwin",
        "rsuper",
    ];
    const WHITESPACE: &[&str] = &["", " ", "\t", "\r\n", "\u{a0}", "\t\u{2003} "];

    fn unicode_text() -> impl Strategy<Value = String> {
        const EDGE_CHARS: &[char] = &['\0', '\t', '\n', '\u{1b}', '\u{2003}', 'İ', '\u{307}', '🦀'];
        let character = prop_oneof![4 => any::<char>(), 1 => select(EDGE_CHARS)];
        vec(character, 0..=512).prop_map(|chars| chars.into_iter().collect())
    }

    fn vary_ascii_case(name: &str, uppercase: &[bool; 16]) -> String {
        name.chars()
            .zip(uppercase.iter().cycle())
            .map(|(c, &upper)| if upper { c.to_ascii_uppercase() } else { c })
            .collect()
    }

    fn config() -> ProptestConfig {
        // Keep proptest's 256-case default and its environment overrides.
        let mut config = ProptestConfig::default();
        if config.rng_seed == RngSeed::Random {
            config.rng_seed = RngSeed::Fixed(461);
        }
        config
    }

    proptest! {
        #![proptest_config(config())]

        /// Arbitrary valid UTF-8, including controls and unusual Unicode, never panics.
        #[test]
        fn arbitrary_utf8_does_not_panic(input in unicode_text()) {
            let _ = parse_key(&input);
        }

        /// Recognized ASCII key names keep the same keysym across case and surrounding whitespace.
        #[test]
        fn named_keys_ignore_ascii_case_and_whitespace(
            uppercase in any::<[bool; 16]>(),
            before in select(WHITESPACE),
            after in select(WHITESPACE),
        ) {
            for &name in NAMED_KEYS.iter().chain(WINDOWS_MODIFIERS) {
                let expected = parse_key(name);
                let varied = vary_ascii_case(name, &uppercase);
                let input = format!("{before}{varied}{after}");
                prop_assert!(matches!(expected, Some(Key::Keysym(_))), "{name:?}");
                prop_assert_eq!(parse_key(&input), expected, "input: {:?}", input);
            }
        }

        /// Linux recognizes every Windows modifier spelling as a named key.
        #[test]
        fn windows_modifier_names_are_recognized(uppercase in any::<[bool; 16]>()) {
            for &name in WINDOWS_MODIFIERS {
                let input = vary_ascii_case(name, &uppercase);
                prop_assert!(matches!(parse_key(&input), Some(Key::Keysym(_))), "{input:?}");
            }
        }

        /// F1 through F24 map to the X11 keysyms in keysymdef.h, and out-of-range numbers are rejected.
        #[test]
        fn function_key_range(
            prefix in select(&['f', 'F'][..]),
            leading_zeroes in 0..=3_usize,
            invalid in prop_oneof![Just(0), 25..=64_u64, 65..=u64::MAX],
        ) {
            let zeroes = "0".repeat(leading_zeroes);
            for (number, sym) in (1..=24).zip(0xFFBE..=0xFFD5) {
                let input = format!("{prefix}{zeroes}{number}");
                prop_assert_eq!(parse_key(&input), Some(Key::Keysym(sym)), "input: {:?}", input);
            }
            let input = format!("{prefix}{zeroes}{invalid}");
            prop_assert_eq!(parse_key(&input), None, "input: {:?}", input);
        }

        /// ASCII letters use their lowercase X11 keysyms, and digits use their ASCII values.
        #[test]
        fn ascii_letters_and_digits(letter in b'a'..=b'z', digit in b'0'..=b'9') {
            for byte in [letter, letter.to_ascii_uppercase(), digit] {
                let input = char::from(byte).to_string();
                let sym = u32::from(byte.to_ascii_lowercase());
                prop_assert_eq!(parse_key(&input), Some(Key::Keysym(sym)), "input: {:?}", input);
            }
        }

        /// A single untrimmed non-ASCII literal with no named mapping retains its original scalar.
        #[test]
        fn unicode_literals_keep_the_original_character(
            c in any::<char>().prop_filter("non-ASCII literal", |c| !c.is_ascii() && !c.is_whitespace()),
            before in select(WHITESPACE),
            after in select(WHITESPACE),
        ) {
            // Literal characters come from the original spelling after the named-key lookup.
            let input = format!("{before}{c}{after}");
            prop_assert_eq!(parse_key(&input), Some(Key::Char(c)), "input: {:?}", input);
        }

        /// Unknown tokens with at least two characters after trimming are rejected.
        #[test]
        fn unknown_multi_character_tokens_are_rejected(text in unicode_text()) {
            let input = format!(" \t?{text}!\u{2003}");
            prop_assert_eq!(parse_key(&input), None, "input: {:?}", input);
        }
    }

    /// Function key boundaries and oversized numeric suffixes are handled safely.
    #[test]
    fn function_key_boundaries_and_overflow() {
        assert_eq!(parse_key("F0"), None);
        assert_eq!(parse_key("F1"), Some(Key::Keysym(0xFFBE)));
        assert_eq!(parse_key("F24"), Some(Key::Keysym(0xFFD5)));
        assert_eq!(parse_key("F25"), None);
        assert_eq!(parse_key("F4294967296"), None);
        assert_eq!(parse_key(&format!("F{}", "9".repeat(8192))), None);
        assert_eq!(
            parse_key(&format!("F{}1", "0".repeat(8192))),
            Some(Key::Keysym(0xFFBE))
        );
    }

    /// The named space token yields a keysym, while literal fallback requires one non-whitespace scalar.
    #[test]
    fn whitespace_and_literal_regressions() {
        assert_eq!(parse_key(" \tSpAcE\u{2003}"), Some(Key::Keysym(0x0020)));
        for input in ["", " ", "\t\r\n\u{2003}", "e\u{301}", "ctrl\0", "a\0b"] {
            assert_eq!(parse_key(input), None, "{input:?}");
        }
        for c in ['İ', 'ı', 'ş', '\u{212a}', '🦀', '\u{301}', '\u{1b}', '\0'] {
            assert_eq!(parse_key(&c.to_string()), Some(Key::Char(c)), "{c:?}");
        }
        assert_eq!(parse_key(&"İ\u{301}🦀\0".repeat(4096)), None);
    }

    /// Left and right modifier aliases retain the corresponding X11 keysyms.
    #[test]
    fn sided_modifier_aliases() {
        for (name, sym) in [
            ("lctrl", 0xFFE3),
            ("rctrl", 0xFFE4),
            ("lshift", 0xFFE1),
            ("rshift", 0xFFE2),
            ("lalt", 0xFFE9),
            ("ralt", 0xFFEA),
            ("altgr", 0xFFEA),
            ("lwin", 0xFFEB),
            ("rwin", 0xFFEC),
            ("rsuper", 0xFFEC),
        ] {
            assert_eq!(parse_key(name), Some(Key::Keysym(sym)), "{name}");
        }
    }
}
