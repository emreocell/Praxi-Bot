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
        "lctrl" => 0xA2,
        "rctrl" => 0xA3,
        "shift" => 0x10,
        "lshift" => 0xA0,
        "rshift" => 0xA1,
        "alt" | "menu" | "option" => 0x12,
        "lalt" => 0xA4,
        "ralt" | "altgr" => 0xA5,
        "win" | "windows" | "cmd" | "meta" | "super" | "lwin" => 0x5B,
        "rwin" | "rsuper" => 0x5C,
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
    fn key_parsing_left_right_modifiers() {
        for (name, vk) in [
            ("lctrl", 0xA2),
            ("rctrl", 0xA3),
            ("lshift", 0xA0),
            ("rshift", 0xA1),
            ("lalt", 0xA4),
            ("ralt", 0xA5),
            ("altgr", 0xA5),
            ("lwin", 0x5B),
            ("rwin", 0x5C),
            ("rsuper", 0x5C),
        ] {
            assert_eq!(parse_key(name), Some(Key::Vk(vk)), "{name}");
        }
        // The generic names keep their virtual keys.
        assert_eq!(parse_key("shift"), Some(Key::Vk(0x10)));
        assert_eq!(parse_key("alt"), Some(Key::Vk(0x12)));
        assert_eq!(parse_key("win"), Some(Key::Vk(0x5B)));
        // Right Ctrl and right Alt are sent with the extended-key flag.
        for name in ["rctrl", "ralt"] {
            let Some(Key::Vk(vk)) = parse_key(name) else {
                panic!("{name} should parse to a virtual key");
            };
            assert!(crate::live_view_win::is_win_extended_vk(vk), "{name}");
        }
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

const BATCH_REFUSAL: &str = "batch launches with cmd.exe metacharacters are refused; use an executable or pass plain arguments";
pub(crate) const SHELL_REFUSAL: &str =
    "command interpreter arguments with shell metacharacters are refused";

/// How a resolved launch image must be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchKind {
    /// PE image parsed by `CommandLineToArgvW`.
    DirectProcess,
    /// `cmd.exe`, PowerShell, `wscript.exe`, or `cscript.exe`.
    ShellInterpreter,
    /// `.bat`, `.cmd`, `.lnk`, or an association whose template runs `cmd.exe`.
    Batch,
    /// Document whose association is parsed by `CommandLineToArgvW`.
    Document,
}

/// Quoting decision for one launch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PreparedLaunch {
    parameters: String,
    direct: bool,
}

impl PreparedLaunch {
    /// `CreateProcess` the original executable. `ShellExecute` is the other route.
    #[cfg(windows)]
    #[must_use]
    pub(crate) fn spawns_directly(&self) -> bool {
        self.direct
    }

    /// `ShellExecuteExW` parameter string. Empty when there are no arguments.
    #[must_use]
    pub(crate) fn parameters(&self) -> &str {
        &self.parameters
    }
}

/// Drop a `\\?\`, `\\?\UNC\`, or `\\.\` prefix so the image name is the DOS path.
#[must_use]
pub(crate) fn strip_device_prefix(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        format!(r"\\{}", &path[8..])
    } else if lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\") {
        path[4..].to_string()
    } else {
        path.to_string()
    }
}

/// Lexical Win32 normalization: slash folding, `.` / `..`, and trailing dot/space
/// stripping. The `\\?\` prefix does not protect trailing dots from `GetFullPathNameW`,
/// so it is removed before those rules run.
#[must_use]
pub(crate) fn normalize_launch_path(path: &str) -> String {
    let slashed = path.replace('/', "\\");
    let rest = strip_device_prefix(&slashed);
    if rest.is_empty() {
        return String::new();
    }
    let unc = rest.starts_with("\\\\");
    let raw: Vec<&str> = if unc {
        rest[2..]
            .split('\\')
            .filter(|part| !part.is_empty())
            .collect()
    } else {
        rest.split('\\').filter(|part| !part.is_empty()).collect()
    };
    let (root, start) = if unc {
        let server = raw.first().copied().unwrap_or("");
        let share = raw.get(1).copied().unwrap_or("");
        (format!("\\\\{server}\\{share}"), 2.min(raw.len()))
    } else if raw.first().is_some_and(|first| is_drive_prefix(first)) && rest.contains('\\') {
        ((*raw.first().unwrap_or(&"")).to_string(), 1)
    } else {
        (String::new(), 0)
    };
    let mut stack = Vec::new();
    for part in raw.iter().skip(start) {
        if *part == "." {
            continue;
        }
        if *part == ".." {
            stack.pop();
            continue;
        }
        stack.push(strip_one_trailing_dot(part).to_string());
    }
    if let Some(last) = stack.last_mut() {
        let trimmed = last.trim_end_matches([' ', '.']);
        if !trimmed.is_empty() {
            *last = trimmed.to_string();
        }
    }
    join_normalized(&root, &stack)
}

/// Final path component, after [`normalize_launch_path`] when the caller has not
/// already resolved the path.
#[must_use]
pub(crate) fn file_name_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed)
}

/// `cmd.exe` is the interpreter when the association command starts with it.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn command_template_uses_cmd(template: &str) -> bool {
    let Some(token) = first_command_token(template) else {
        return false;
    };
    let expanded = expand_env_vars(&token);
    let normalized = normalize_launch_path(&expanded);
    is_cmd_image(file_name_of(&normalized))
}

/// A document (not an exe, batch file, or shortcut) may still be a `cmd.exe` association.
#[cfg(windows)]
#[must_use]
pub(crate) fn needs_association_check(resolved_path: &str) -> bool {
    classify_file_name(file_name_of(resolved_path)) == LaunchKind::Document
}

/// Choose quoting for a path that has already been resolved to its Win32 image.
///
/// # Errors
/// Rejects NUL, command-interpreter metacharacters, and batch arguments that
/// still change `cmd.exe` parsing inside quotes.
pub(crate) fn prepare_launch(
    app: &desktop_api::snapshot::AppSpec,
    resolved_path: &str,
    cmd_association: bool,
) -> Result<PreparedLaunch, desktop_api::DesktopError> {
    if app.executable.contains('\0') || app.args.iter().any(|arg| arg.contains('\0')) {
        return Err(desktop_api::DesktopError::Unsupported(
            "launch paths and arguments must not contain NUL".into(),
        ));
    }
    let mut kind = classify_file_name(file_name_of(resolved_path));
    if cmd_association && kind == LaunchKind::Document {
        kind = LaunchKind::Batch;
    }
    match kind {
        LaunchKind::ShellInterpreter => {
            if app.args.iter().any(|arg| has_shell_meta(arg)) {
                return Err(desktop_api::DesktopError::Unsupported(SHELL_REFUSAL.into()));
            }
            Ok(PreparedLaunch {
                parameters: crt_parameters(&app.args),
                direct: true,
            })
        }
        LaunchKind::DirectProcess => Ok(PreparedLaunch {
            parameters: crt_parameters(&app.args),
            direct: true,
        }),
        LaunchKind::Batch => {
            if has_batch_path_breakout(&app.executable)
                || app.args.iter().any(|arg| has_batch_breakout(arg))
            {
                return Err(desktop_api::DesktopError::Unsupported(BATCH_REFUSAL.into()));
            }
            // Quote every argument. cmd.exe expands `%VAR%` inside those
            // quotes; `%%` is not an escape on a command line (only inside a
            // batch file), and an expanded value that contains `"` breaks out.
            // `^` is not literal next to the quotes. `%`, `!`, and `^` are
            // rejected above. Parentheses and a trailing backslash stay
            // literal, so this is not CRT quoting.
            let mut quoted = Vec::with_capacity(app.args.len());
            for arg in &app.args {
                quoted.push(format!("\"{arg}\""));
            }
            Ok(PreparedLaunch {
                parameters: quoted.join(" "),
                direct: false,
            })
        }
        LaunchKind::Document => Ok(PreparedLaunch {
            parameters: crt_parameters(&app.args),
            direct: false,
        }),
    }
}

/// Build the argument string used for a Windows application launch.
///
/// # Errors
/// Rejects launch arguments that cannot safely be passed to their interpreter.
pub fn launch_parameters(
    app: &desktop_api::snapshot::AppSpec,
) -> Result<String, desktop_api::DesktopError> {
    let resolved = normalize_launch_path(&app.executable);
    Ok(prepare_launch(app, &resolved, false)?
        .parameters()
        .to_string())
}

fn is_drive_prefix(part: &str) -> bool {
    let mut chars = part.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic()
    )
}

/// Remove one trailing dot when it is only an empty extension (`foo.` → `foo`).
/// `..`, `...`, and `foo..` stay intact so later `.` / `..` resolution still works.
fn strip_one_trailing_dot(component: &str) -> &str {
    match component.strip_suffix('.') {
        Some(stripped) if !stripped.is_empty() && !stripped.ends_with('.') => stripped,
        _ => component,
    }
}

fn join_normalized(root: &str, stack: &[String]) -> String {
    if root.is_empty() {
        return stack.join("\\");
    }
    if stack.is_empty() {
        return if root.ends_with(':') {
            format!("{root}\\")
        } else {
            root.to_string()
        };
    }
    format!("{root}\\{}", stack.join("\\"))
}

fn classify_file_name(name: &str) -> LaunchKind {
    let lower = name.to_ascii_lowercase();
    if is_shell_interpreter(&lower) {
        LaunchKind::ShellInterpreter
    } else if has_extension(&lower, "exe") {
        LaunchKind::DirectProcess
    } else if has_extension(&lower, "bat")
        || has_extension(&lower, "cmd")
        || has_extension(&lower, "lnk")
    {
        LaunchKind::Batch
    } else {
        LaunchKind::Document
    }
}

fn has_extension(lower_name: &str, extension: &str) -> bool {
    lower_name
        .rsplit_once('.')
        .is_some_and(|(_, found)| found == extension)
}

fn is_shell_interpreter(file_name: &str) -> bool {
    matches!(
        file_name,
        "cmd"
            | "cmd.exe"
            | "powershell"
            | "powershell.exe"
            | "pwsh"
            | "pwsh.exe"
            | "wscript"
            | "wscript.exe"
            | "cscript"
            | "cscript.exe"
    )
}

/// Final path component is `cmd`, PowerShell, `pwsh`, `wscript`, or `cscript`,
/// with or without `.exe`.
#[cfg(windows)]
#[must_use]
pub(crate) fn file_name_is_shell_interpreter(path: &str) -> bool {
    is_shell_interpreter(&file_name_of(path).to_ascii_lowercase())
}

#[cfg(any(windows, test))]
fn is_cmd_image(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower == "cmd.exe" || lower == "cmd"
}

fn has_shell_meta(value: &str) -> bool {
    value.chars().any(|c| "&|<>^%!()\"';`${}\r\n".contains(c))
}

fn has_batch_breakout(value: &str) -> bool {
    // `%` expands inside the quotes this launcher adds. `^` escapes the next
    // character even next to those quotes, so neither is literal.
    value.chars().any(|c| "&|<>\"!%^\r\n".contains(c))
}

/// `%` cannot be doubled inside `lpFile`; the association quotes it as `"%1"`.
fn has_batch_path_breakout(value: &str) -> bool {
    value.chars().any(|c| "&|<>\"!%\r\n".contains(c))
}

fn crt_parameters(args: &[String]) -> String {
    args.iter()
        .map(|argument| quote_arg(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(windows, test))]
fn first_command_token(template: &str) -> Option<String> {
    let trimmed = template.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let end = rest.find('"').unwrap_or(rest.len());
        let token = &rest[..end];
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    } else {
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        Some(trimmed[..end].to_string())
    }
}

#[cfg(any(windows, test))]
fn expand_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if name.is_empty() {
                out.push('%');
            } else if let Ok(value) = std::env::var(name) {
                out.push_str(&value);
            } else {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
            rest = &after[end + 1..];
        } else {
            out.push('%');
            out.push_str(after);
            return out;
        }
    }
    out.push_str(rest);
    out
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

#[cfg(test)]
mod launch_tests {
    use super::*;
    use desktop_api::{DesktopError, snapshot::AppSpec};

    #[test]
    fn batch_launch_rejects_cmd_metacharacters() {
        for extension in ["cmd", "CMD", "bat", "BAT", "cmd. ", "lnk", "LNK", "lnk. "] {
            for unsafe_argument in [
                "a&b",
                "a|b",
                "<in",
                ">out",
                "!VAR!",
                "a\nb",
                "a\rb",
                "a\"b",
                "%COMSPEC%",
                "100%",
                "^quoted",
                "foo^",
                "foo^^",
            ] {
                let app = AppSpec {
                    executable: format!("C:\\Users\\Emre\\Masaüstü\\run.{extension}"),
                    args: vec![unsafe_argument.into()],
                };
                assert!(
                    matches!(launch_parameters(&app), Err(DesktopError::Unsupported(_))),
                    "accepted {unsafe_argument:?} for {extension}"
                );
            }
        }
    }

    #[test]
    fn batch_quotes_parentheses_and_trailing_backslash() {
        let app = AppSpec {
            executable: r"C:\Program Files (x86)\Masaüstü\helper.cmd".into(),
            args: vec![
                r"C:\Program Files (x86)\data".into(),
                r"C:\Program Files (x86)\".into(),
                "(x86)".into(),
            ],
        };
        assert_eq!(
            launch_parameters(&app).unwrap(),
            [
                r#""C:\Program Files (x86)\data""#,
                r#""C:\Program Files (x86)\""#,
                r#""(x86)""#,
            ]
            .join(" ")
        );
        for unsafe_argument in ["%COMSPEC%", "100%", "^quoted", "foo^", "a\"b"] {
            let rejected = AppSpec {
                executable: r"C:\Program Files (x86)\Masaüstü\helper.cmd".into(),
                args: vec![unsafe_argument.into()],
            };
            assert!(
                matches!(
                    launch_parameters(&rejected),
                    Err(DesktopError::Unsupported(_))
                ),
                "accepted {unsafe_argument:?}"
            );
        }
        let caret_path = AppSpec {
            executable: r"C:\a^b\run.cmd".into(),
            args: vec!["ok".into()],
        };
        assert_eq!(launch_parameters(&caret_path).unwrap(), r#""ok""#);
        let percent_path = AppSpec {
            executable: r"C:\100%\run.cmd".into(),
            args: vec!["plain".into()],
        };
        assert!(matches!(
            launch_parameters(&percent_path),
            Err(DesktopError::Unsupported(_))
        ));
    }

    #[test]
    fn shell_interpreter_paths_reject_metacharacters() {
        for executable in [
            "cmd.exe",
            "cmd",
            r"C:\Windows\System32\cmd",
            "powershell",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell",
            "pwsh",
            "wscript",
            "cscript",
            "cmd.exe.",
            "cmd.exe. .",
            r"C:\Windows\System32\cmd.exe.",
            r"C:\Windows\System32\cmd.exe\.",
            r"C:\Windows\System32\cmd.exe\..\cmd.exe.",
            r"\\?\C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe.",
            "pwsh.exe",
            "wscript.exe",
            "cscript.exe",
            "CMD.EXE",
        ] {
            let app = AppSpec {
                executable: executable.into(),
                args: vec!["safe&calc".into()],
            };
            assert!(
                matches!(launch_parameters(&app), Err(DesktopError::Unsupported(_))),
                "{executable}"
            );
        }
        let plain = AppSpec {
            executable: r"C:\Windows\System32\cmd.exe.".into(),
            args: vec!["/c".into(), "exit".into(), "0".into()],
        };
        assert!(launch_parameters(&plain).is_ok());
        let direct = AppSpec {
            executable: "cmd.exe".into(),
            args: vec!["(x86)".into()],
        };
        assert!(matches!(
            launch_parameters(&direct),
            Err(DesktopError::Unsupported(_))
        ));
        let notepad = AppSpec {
            executable: r"C:\Windows\notepad.exe.".into(),
            args: vec!["a&b".into()],
        };
        assert_eq!(launch_parameters(&notepad).unwrap(), "a&b");
    }

    #[test]
    fn shortcuts_reject_cmd_metacharacters() {
        for executable in [
            r"C:\temp\app.lnk",
            r"C:\temp\app.lnk.",
            r"C:\temp\app.LNK. .",
        ] {
            let app = AppSpec {
                executable: executable.into(),
                args: vec!["plain&second".into()],
            };
            assert!(
                matches!(launch_parameters(&app), Err(DesktopError::Unsupported(_))),
                "{executable}"
            );
        }
        let safe = AppSpec {
            executable: r"C:\temp\app.lnk".into(),
            args: vec!["plain".into()],
        };
        assert_eq!(launch_parameters(&safe).unwrap(), r#""plain""#);
    }

    #[test]
    fn normalize_strips_win32_trailing_dots_and_dot_segments() {
        assert_eq!(
            normalize_launch_path(r"C:\Windows\System32\cmd.exe."),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(
            normalize_launch_path(r"C:\Windows\System32\cmd.exe\."),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(
            normalize_launch_path(r"C:\Windows\System32\cmd.exe\..\cmd.exe."),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(normalize_launch_path(r"C:\foo.\..\cmd.exe."), r"C:\cmd.exe");
        assert_eq!(normalize_launch_path(r"C:\foo...\bar"), r"C:\foo...\bar");
        assert_eq!(
            normalize_launch_path(r"\\?\C:\Windows\System32\cmd.exe."),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(
            normalize_launch_path(r"C:\Windows.\System32.\cmd.EXE. "),
            r"C:\Windows\System32\cmd.EXE"
        );
        assert_eq!(file_name_of("cmd.exe."), "cmd.exe.");
        assert_eq!(file_name_of(&normalize_launch_path("cmd.exe.")), "cmd.exe");
    }

    #[test]
    fn command_templates_expanded_by_cmd() {
        assert!(command_template_uses_cmd(
            r#"C:\Windows\System32\cmd.exe /c "%1" %*"#
        ));
        assert!(command_template_uses_cmd(
            r#""C:\Windows\System32\cmd.exe" /c "%1" %*"#
        ));
        assert!(!command_template_uses_cmd(r#""%1" %*"#));
        assert!(!command_template_uses_cmd(
            r#""C:\Windows\System32\WScript.exe" "%1" %*"#
        ));
        assert!(!command_template_uses_cmd(
            r"C:\Windows\system32\NOTEPAD.EXE %1"
        ));
        if std::env::var("SystemRoot").is_ok() {
            assert!(command_template_uses_cmd(
                r"%SystemRoot%\system32\cmd.exe /c %1"
            ));
        }
    }

    #[test]
    fn unicode_executable_arguments_keep_crt_quoting() {
        assert_eq!(
            quote_arg(r"C:\Users\Emre\Masaüstü\dosya.txt"),
            r"C:\Users\Emre\Masaüstü\dosya.txt"
        );
        assert_eq!(
            quote_arg(r"C:\Masaüstü\Yeni klasör\"),
            r#""C:\Masaüstü\Yeni klasör\\""#
        );
        assert_eq!(quote_arg(""), r#""""#);
        let app = AppSpec {
            executable: "editor.exe".into(),
            args: vec!["a&b".into(), "100%".into()],
        };
        assert_eq!(launch_parameters(&app).unwrap(), "a&b 100%");
    }

    #[test]
    fn plain_batch_paths_preserve_backslashes_and_empty_arguments() {
        let app = AppSpec {
            executable: r"C:\Masaüstü\run.cmd".into(),
            args: vec![r"C:\Masaüstü\Yeni klasör\".into(), String::new()],
        };
        assert_eq!(
            launch_parameters(&app).unwrap(),
            "\"C:\\Masaüstü\\Yeni klasör\\\" \"\""
        );
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
    // Modifier spellings accepted by both desktop-windows and desktop-linux.
    // Note: desktop-linux additionally accepts X11-specific non-modifier aliases
    // (`spacebar`, `page_up`/`page-up`, `page_down`/`page-down`,
    // `arrow_left`/`arrow-left` etc., `caps`/`capslock`/`caps_lock`, and
    // `print`/`printscreen`/`print_screen`/`prtsc`), while every modifier name
    // accepted on Windows is also accepted on Linux.
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

        /// Recognized ASCII key names and modifiers keep the same virtual-key code across case and surrounding whitespace.
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
                prop_assert!(matches!(expected, Some(Key::Vk(_))), "{name:?}");
                prop_assert_eq!(parse_key(&input), expected, "input: {:?}", input);
            }
        }

        /// F1 through F24 map to Win32 VK_F1..=VK_F24 (0x70..=0x87), and out-of-range numbers are rejected.
        #[test]
        fn function_key_range(
            prefix in select(&['f', 'F'][..]),
            leading_zeroes in 0..=3_usize,
            invalid in prop_oneof![Just(0_u64), 25..=64_u64, 65..=u64::MAX],
        ) {
            let zeroes = "0".repeat(leading_zeroes);
            for (number, vk) in (1_u16..=24).zip(0x70_u16..=0x87) {
                let input = format!("{prefix}{zeroes}{number}");
                prop_assert_eq!(parse_key(&input), Some(Key::Vk(vk)), "input: {:?}", input);
            }
            let input = format!("{prefix}{zeroes}{invalid}");
            prop_assert_eq!(parse_key(&input), None, "input: {:?}", input);
        }

        /// ASCII letters map to VK_A..=VK_Z (0x41..=0x5A) and digits map to VK_0..=VK_9 (0x30..=0x39).
        #[test]
        fn ascii_letters_and_digits(letter in b'a'..=b'z', digit in b'0'..=b'9') {
            let vk_letter = 0x41 + u16::from(letter - b'a');
            for byte in [letter, letter.to_ascii_uppercase()] {
                let input = char::from(byte).to_string();
                prop_assert_eq!(parse_key(&input), Some(Key::Vk(vk_letter)), "input: {:?}", input);
            }
            let vk_digit = 0x30 + u16::from(digit - b'0');
            let input = char::from(digit).to_string();
            prop_assert_eq!(parse_key(&input), Some(Key::Vk(vk_digit)), "input: {:?}", input);
        }

        /// A single non-ASCII non-whitespace literal retains its original Unicode scalar.
        #[test]
        fn unicode_literals_keep_the_original_character(
            c in any::<char>().prop_filter("non-ASCII literal", |c| !c.is_ascii() && !c.is_whitespace()),
            before in select(WHITESPACE),
            after in select(WHITESPACE),
        ) {
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
}
/// Packaged application IDs have a package family and an application name.
/// Keep shell paths, protocols and inline arguments out of this route.
#[must_use]
pub fn valid_packaged_app_id(id: &str) -> bool {
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    };
    id.len() <= 128
        && id.split_once('!').is_some_and(|(package, app)| {
            package
                .rsplit_once('_')
                .is_some_and(|(name, publisher)| valid(name) && valid(publisher))
                && valid(app)
        })
}

#[cfg(test)]
#[test]
fn packaged_app_id_is_not_a_shell_path_or_command() {
    assert!(valid_packaged_app_id(
        "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
    ));
    assert!(!valid_packaged_app_id(
        "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App /arg"
    ));
    assert!(!valid_packaged_app_id(
        "shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
    ));
}
