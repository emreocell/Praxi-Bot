// Adapted from vercel-labs/agent-browser (Apache-2.0): line diff of snapshot text.
//! Snapshot diffing: emit only added/removed lines so agents spend fewer tokens per turn.

use similar::{ChangeTag, TextDiff};

use crate::snapshot::Snapshot;

/// Difference between two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotDiff {
    /// Lines present only in the new snapshot.
    pub added: Vec<String>,
    /// Lines present only in the old snapshot.
    pub removed: Vec<String>,
    /// Number of identical lines.
    pub unchanged: usize,
    /// New URL when the page URL changed.
    pub url_changed: Option<String>,
    /// New title when the page title changed.
    pub title_changed: Option<String>,
}

impl SnapshotDiff {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.url_changed.is_none()
            && self.title_changed.is_none()
    }

    /// Render as text: `~` header lines, `-` removed, `+` added.
    pub fn render(&self) -> String {
        if self.is_empty() {
            return format!("(no changes, {} lines unchanged)", self.unchanged);
        }
        let mut out = Vec::new();
        if let Some(u) = &self.url_changed {
            out.push(format!("~ url: {u}"));
        }
        if let Some(t) = &self.title_changed {
            out.push(format!("~ title: {t}"));
        }
        out.extend(self.removed.iter().map(|l| format!("- {l}")));
        out.extend(self.added.iter().map(|l| format!("+ {l}")));
        out.push(format!("({} lines unchanged)", self.unchanged));
        out.join("\n")
    }
}

/// Diff two snapshot texts line by line.
pub fn diff_text(prev: &str, next: &str) -> SnapshotDiff {
    let mut d = SnapshotDiff::default();
    // Normalise the trailing newline so a final line compares equal on both sides.
    let (prev, next) = (
        format!(
            "{}
",
            prev.trim_end()
        ),
        format!(
            "{}
",
            next.trim_end()
        ),
    );
    let diff = TextDiff::from_lines(&prev, &next);
    for change in diff.iter_all_changes() {
        let line = change.value().trim_end_matches(['\n', '\r']).to_string();
        match change.tag() {
            ChangeTag::Equal => d.unchanged += 1,
            ChangeTag::Insert => d.added.push(line),
            ChangeTag::Delete => d.removed.push(line),
        }
    }
    d
}

/// Diff two snapshots (tree lines plus URL/title).
pub fn diff(prev: &Snapshot, next: &Snapshot) -> SnapshotDiff {
    let mut d = diff_text(&prev.to_text(), &next.to_text());
    if prev.url != next.url {
        d.url_changed = Some(next.url.clone());
    }
    if prev.title != next.title {
        d.title_changed = Some(next.title.clone());
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::SnapNode;

    fn node(depth: usize, r: Option<&str>, role: &str, name: &str) -> SnapNode {
        SnapNode {
            depth,
            ref_id: r.map(str::to_string),
            role: role.into(),
            name: name.into(),
            value: None,
            states: vec![],
        }
    }

    #[test]
    fn identical_is_empty() {
        let d = diff_text("a\nb\n", "a\nb\n");
        assert!(d.is_empty());
        assert_eq!(d.unchanged, 2);
        assert!(d.render().starts_with("(no changes"));
    }

    #[test]
    fn added_removed_changed() {
        let d = diff_text("a\nb\nc", "a\nB\nc\nd");
        assert_eq!(d.removed, vec!["b"]);
        assert_eq!(d.added, vec!["B", "d"]);
        assert_eq!(d.unchanged, 2);
        let r = d.render();
        assert!(r.contains("- b") && r.contains("+ B") && r.contains("+ d"));
    }

    #[test]
    fn snapshot_diff_tracks_url_and_title() {
        let a = Snapshot {
            url: "https://a".into(),
            title: "A".into(),
            nodes: vec![
                node(0, Some("e1"), "link", "Go"),
                node(0, None, "text", "hi"),
            ],
        };
        let b = Snapshot {
            url: "https://b".into(),
            title: "B".into(),
            nodes: vec![
                node(0, None, "text", "hi"),
                node(0, Some("e2"), "button", "Buy"),
            ],
        };
        let d = diff(&a, &b);
        assert_eq!(d.url_changed.as_deref(), Some("https://b"));
        assert_eq!(d.title_changed.as_deref(), Some("B"));
        assert_eq!(d.removed, vec!["[e1] link \"Go\""]);
        assert_eq!(d.added, vec!["[e2] button \"Buy\""]);
        assert_eq!(d.unchanged, 1);
        assert!(d.render().contains("~ url: https://b"));
    }
}
