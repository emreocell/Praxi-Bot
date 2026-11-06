//! Compact one-line-per-node text renderer for LLM consumption.
//!
//! Format: `[12] button "Save" (640,480) {focused} value="..."`.
//! Coordinates are the node's centre in screen pixels. Secure (password)
//! fields never print a value; they render as `<secure>`.

use std::fmt::Write as _;

use crate::snapshot::{Snapshot, UiNode};

const MAX_VALUE_CHARS: usize = 80;

fn escape(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Render a single node as one line (no trailing newline).
#[must_use]
pub fn render_node(node: &UiNode) -> String {
    let mut line = format!("[{}] {}", node.index, node.role);
    if !node.name.is_empty() {
        let _ = write!(line, " \"{}\"", escape(&node.name, 120));
    }
    if let Some(b) = node.bounds {
        let (cx, cy) = b.center();
        // Pixel coordinates comfortably fit; rounding is intended.
        #[allow(clippy::cast_possible_truncation)]
        let _ = write!(line, " ({},{})", cx.round() as i64, cy.round() as i64);
    }
    if !node.states.is_empty() {
        let _ = write!(line, " {{{}}}", node.states.join(","));
    }
    if node.states.iter().any(|s| s == "secure") {
        line.push_str(" <secure>");
    } else if let Some(v) = node.value.as_deref().filter(|v| !v.is_empty()) {
        let _ = write!(line, " value=\"{}\"", escape(v, MAX_VALUE_CHARS));
    }
    line
}

/// Render a whole snapshot: a header line followed by one line per node.
#[must_use]
pub fn render_snapshot(snapshot: &Snapshot) -> String {
    let mut out = format!(
        "# window \"{}\" pid={} gen={} quality={:?} nodes={}\n",
        escape(&snapshot.window.title, 120),
        snapshot.window.pid,
        snapshot.generation,
        snapshot.quality,
        snapshot.nodes.len()
    );
    for n in &snapshot.nodes {
        out.push_str(&render_node(n));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Rect;

    fn rect() -> Rect {
        Rect {
            x: 600.0,
            y: 460.0,
            width: 80.0,
            height: 40.0,
        }
    }

    #[test]
    fn renders_button_line() {
        let n = UiNode::new(
            12,
            "button",
            "Save",
            None,
            Some(rect()),
            vec!["focused".into()],
        );
        assert_eq!(render_node(&n), "[12] button \"Save\" (640,480) {focused}");
    }

    #[test]
    fn secure_field_hides_value() {
        let n = UiNode::new(
            3,
            "edit",
            "Password",
            Some("hunter2".into()),
            None,
            vec!["secure".into()],
        );
        let line = render_node(&n);
        assert!(line.contains("<secure>"));
        assert!(!line.contains("hunter2"));
    }

    #[test]
    fn escapes_and_truncates_value() {
        let long = "a".repeat(200);
        let n = UiNode::new(1, "edit", "q\"x", Some(format!("l1\n{long}")), None, vec![]);
        let line = render_node(&n);
        assert!(line.contains("\"q\\\"x\""));
        assert!(line.contains("l1\\n"));
        assert!(line.ends_with("…\""));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn unnamed_node_has_no_quotes() {
        let n = UiNode::new(0, "pane", "", None, None, vec![]);
        assert_eq!(render_node(&n), "[0] pane");
    }
}
