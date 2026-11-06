//! Browser automation via Chrome `DevTools` Protocol: snapshot, element refs, and DOM diff.
//!
//! Connects to a running browser over CDP, captures accessibility/DOM snapshots
//! with stable element references, and computes incremental diffs between turns.
