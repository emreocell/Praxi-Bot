//! What changed between two observations of one window, for the compact
//! state an action returns instead of the whole tree.
//!
//! Indexes are renumbered by every snapshot, so nodes are matched by their
//! identity token (role + name) and, among equals, by order of appearance.
//! The result carries the NEW indexes: they are the ones the model may act on.

use std::collections::HashMap;

use crate::snapshot::{Fingerprint, UiNode};

/// A node that is in both observations with a different value or state.
#[derive(Debug, Clone)]
pub struct NodeChange<'a> {
    pub before: &'a UiNode,
    pub after: &'a UiNode,
}

/// The difference between two node lists of the same window.
#[derive(Debug, Clone, Default)]
pub struct NodeDiff<'a> {
    /// Only in the new observation.
    pub added: Vec<&'a UiNode>,
    /// Only in the old observation.
    pub removed: Vec<&'a UiNode>,
    /// In both, value or states differ.
    pub changed: Vec<NodeChange<'a>>,
}

impl NodeDiff<'_> {
    /// Nothing differs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compare `before` with `after`.
#[must_use]
pub fn diff_nodes<'a>(before: &'a [UiNode], after: &'a [UiNode]) -> NodeDiff<'a> {
    let mut pool: HashMap<Fingerprint, Vec<&'a UiNode>> = HashMap::new();
    for n in before {
        pool.entry(n.fingerprint).or_default().push(n);
    }
    // Equal nodes pair up in order: the first old "OK" button with the first new one.
    for v in pool.values_mut() {
        v.reverse();
    }
    let mut out = NodeDiff::default();
    for n in after {
        match pool.get_mut(&n.fingerprint).and_then(Vec::pop) {
            Some(old) => {
                if old.value != n.value || old.states != n.states {
                    out.changed.push(NodeChange {
                        before: old,
                        after: n,
                    });
                }
            }
            None => out.added.push(n),
        }
    }
    let mut left: Vec<&UiNode> = pool.into_values().flatten().collect();
    left.sort_by_key(|n| n.index);
    out.removed = left;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(i: u32, role: &str, name: &str, value: Option<&str>, states: &[&str]) -> UiNode {
        UiNode::new(
            i,
            role,
            name,
            value.map(str::to_owned),
            None,
            states.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    #[test]
    fn identical_trees_have_no_diff_even_when_indexes_shift() {
        let a = vec![
            node(0, "pane", "w", None, &[]),
            node(1, "button", "OK", None, &[]),
        ];
        let b = vec![
            node(5, "pane", "w", None, &[]),
            node(6, "button", "OK", None, &[]),
        ];
        assert!(diff_nodes(&a, &b).is_empty());
    }

    #[test]
    fn a_new_dialog_button_a_changed_value_and_a_gone_node_are_told_apart() {
        let a = vec![
            node(0, "edit", "Name", Some("a"), &[]),
            node(1, "button", "Save", None, &[]),
            node(2, "text", "Saving...", None, &[]),
        ];
        let b = vec![
            node(0, "edit", "Name", Some("ab"), &["focused"]),
            node(1, "button", "Save", None, &[]),
            node(2, "button", "Cancel", None, &[]),
        ];
        let d = diff_nodes(&a, &b);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].name, "Cancel");
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].name, "Saving...");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].after.value.as_deref(), Some("ab"));
        assert_eq!(d.changed[0].before.value.as_deref(), Some("a"));
    }

    #[test]
    fn equal_nodes_pair_by_order() {
        let a = vec![
            node(0, "button", "OK", None, &[]),
            node(1, "button", "OK", None, &[]),
        ];
        let b = vec![
            node(0, "button", "OK", None, &[]),
            node(1, "button", "OK", None, &["disabled"]),
            node(2, "button", "OK", None, &[]),
        ];
        let d = diff_nodes(&a, &b);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].after.index, 1);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].index, 2);
        assert!(d.removed.is_empty());
    }
}
