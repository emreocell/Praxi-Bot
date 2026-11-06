//! Platform-independent parts of the Windows backend: control-type naming,
//! tree filtering and key-name parsing. Compiled on every OS so it is unit
//! testable anywhere.

use desktop_api::snapshot::{Rect, UiNode};

/// Maximum number of nodes kept in one snapshot.
pub const MAX_NODES: usize = 2500;
/// Maximum tree depth walked.
pub const MAX_DEPTH: usize = 40;

/// Map a UIA `ControlType` id (50000..=50040) to a lowercase role string.
#[must_use]
pub fn role_name(control_type: i32) -> &'static str {
    match control_type {
        50000 => "button",
        50001 => "calendar",
        50002 => "checkbox",
        50003 => "combobox",
        50004 => "edit",
        50005 => "hyperlink",
        50006 => "image",
        50007 => "listitem",
        50008 => "list",
        50009 => "menu",
        50010 => "menubar",
        50011 => "menuitem",
        50012 => "progressbar",
        50013 => "radiobutton",
        50014 => "scrollbar",
        50015 => "slider",
        50016 => "spinner",
        50017 => "statusbar",
        50018 => "tab",
        50019 => "tabitem",
        50020 => "text",
        50021 => "toolbar",
        50022 => "tooltip",
        50023 => "tree",
        50024 => "treeitem",
        50025 => "custom",
        50026 => "group",
        50027 => "thumb",
        50028 => "datagrid",
        50029 => "dataitem",
        50030 => "document",
        50031 => "splitbutton",
        50032 => "window",
        50033 => "pane",
        50034 => "header",
        50035 => "headeritem",
        50036 => "table",
        50037 => "titlebar",
        50038 => "separator",
        50039 => "semanticzoom",
        50040 => "appbar",
        _ => "unknown",
    }
}

/// Roles an agent can usefully act on.
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

/// Roles kept as context only when they carry a name.
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

/// One element as read from UIA, before filtering.
// The flags mirror independent UIA properties.
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
    /// Extra states already derived from patterns (`checked`, `selected`, ...).
    pub pattern_states: Vec<&'static str>,
}

impl RawNode {
    fn is_interactive(&self) -> bool {
        INTERACTIVE.contains(&self.role)
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

// ── Keys ──────────────────────────────────────────────────────────────────────

/// A parsed key token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A Windows virtual-key code.
    Vk(u16),
    /// A literal character to be typed as Unicode.
    Char(char),
}

/// Parse a key name such as `"ctrl"`, `"Return"`, `"F5"` or `"a"`.
#[must_use]
pub fn parse_key(name: &str) -> Option<Key> {
    let lower = name.trim().to_lowercase();
    let vk: u16 = match lower.as_str() {
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" | "menu" | "option" => 0x12,
        "win" | "windows" | "cmd" | "meta" | "super" => 0x5B,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "space" => 0x20,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        _ => 0,
    };
    if vk != 0 {
        return Some(Key::Vk(vk));
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(Key::Vk(0x70 + n - 1));
    }
    let trimmed = name.trim();
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(Key::Vk(
            0x41 + u16::from(c.to_ascii_lowercase() as u8 - b'a'),
        )),
        (Some(c), None) if c.is_ascii_digit() => Some(Key::Vk(0x30 + u16::from(c as u8 - b'0'))),
        (Some(c), None) => Some(Key::Char(c)),
        _ => None,
    }
}

/// Whether a virtual key needs `KEYEVENTF_EXTENDEDKEY`.
#[must_use]
pub fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C)
}

/// Convert a normalised 0-999 coordinate over `rect` to absolute pixels.
#[must_use]
pub fn denormalize(x: f64, y: f64, rect: &Rect) -> (f64, f64) {
    (
        rect.x + x.clamp(0.0, 999.0) / 999.0 * rect.width,
        rect.y + y.clamp(0.0, 999.0) / 999.0 * rect.height,
    )
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
        }
    }

    #[test]
    fn role_names() {
        assert_eq!(role_name(50000), "button");
        assert_eq!(role_name(50004), "edit");
        assert_eq!(role_name(1), "unknown");
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
    fn key_parsing() {
        assert_eq!(parse_key("Ctrl"), Some(Key::Vk(0x11)));
        assert_eq!(parse_key("Return"), Some(Key::Vk(0x0D)));
        assert_eq!(parse_key("F5"), Some(Key::Vk(0x74)));
        assert_eq!(parse_key("A"), Some(Key::Vk(0x41)));
        assert_eq!(parse_key("7"), Some(Key::Vk(0x37)));
        assert_eq!(parse_key("\u{fc}"), Some(Key::Char('\u{fc}')));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key("f25"), None);
    }

    #[test]
    fn extended_keys() {
        assert!(is_extended(0x25));
        assert!(!is_extended(0x41));
    }

    #[test]
    fn normalized_coords() {
        let r = Rect {
            x: 100.0,
            y: 200.0,
            width: 999.0,
            height: 999.0,
        };
        assert_eq!(denormalize(0.0, 999.0, &r), (100.0, 1199.0));
        assert_eq!(denormalize(5000.0, 0.0, &r), (1099.0, 200.0));
    }
}
