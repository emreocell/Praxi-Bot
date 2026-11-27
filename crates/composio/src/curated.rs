//! The small Praxi-shipped table around the open Composio catalog.
//!
//! It never decides what exists (Composio's toolkit list and per-toolkit tool
//! list do) and never waives an approval.  It only
//! - names the toolkits Praxi connects natively (`native_first`),
//! - pins the "popular" row of the catalog,
//! - marks well-known read-only actions, as read evidence for the risk class
//!   (see `risk`) and for picking which actions get a direct tool when a
//!   toolkit has more than the cap.
//!
//! UNVERIFIED: the popular slugs other than `twitter`, `slack`, `hubspot`,
//! `salesforce`, `airtable` and the read-only action slugs come from memory,
//! not from the live catalog; entries that the live catalog does not know are
//! simply not shown / not matched.

use std::collections::BTreeSet;

use serde::Deserialize;

/// A pinned popular toolkit.
#[derive(Debug, Clone, Deserialize)]
pub struct Popular {
    /// Composio toolkit slug.
    pub slug: String,
    /// Display name (used until the live catalog gives one).
    pub name: String,
    /// One-line description shown before the live catalog is loaded.
    pub description: String,
    /// Chip group (see [`GROUPS`]).
    pub group: String,
}

/// Chip groups of the Apps grid, in display order (UI strings are keyed by these).
pub const GROUPS: [&str; 6] = [
    "communication",
    "social",
    "productivity",
    "dev",
    "finance",
    "business",
];

/// The chip group of a toolkit: the shipped table first, then keywords of
/// Composio's own categories.  `other` when nothing matches.
#[must_use]
pub fn group_of(curated: &Curated, slug: &str, categories: &[String]) -> &'static str {
    if let Some(g) = curated
        .popular
        .iter()
        .find(|p| p.slug == slug)
        .and_then(|p| GROUPS.iter().find(|g| **g == p.group))
    {
        return g;
    }
    let text = categories.join(" ").to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&[
        "communication",
        "email",
        "messag",
        "chat",
        "video conf",
        "phone",
        "sms",
    ]) {
        "communication"
    } else if has(&["social", "media", "video", "music", "podcast", "community"]) {
        "social"
    } else if has(&[
        "payment",
        "financ",
        "accounting",
        "bank",
        "crypto",
        "invoice",
        "billing",
    ]) {
        "finance"
    } else if has(&[
        "developer",
        "devops",
        "code",
        "database",
        "ci/cd",
        "monitoring",
        "cloud",
        "version control",
    ]) {
        "dev"
    } else if has(&[
        "crm",
        "sales",
        "marketing",
        "commerce",
        "customer",
        "support",
        "hr",
        "advertis",
    ]) {
        "business"
    } else if has(&[
        "productivity",
        "project",
        "task",
        "document",
        "note",
        "calendar",
        "storage",
        "spreadsheet",
        "file",
        "collaboration",
        "scheduling",
    ]) {
        "productivity"
    } else {
        "other"
    }
}

#[derive(Deserialize)]
struct File {
    cap_per_toolkit: usize,
    native_first: Vec<String>,
    popular: Vec<Popular>,
    read_only_actions: Vec<String>,
}

/// The shipped table.
#[derive(Debug, Clone)]
pub struct Curated {
    /// Most direct tools exposed to the model per connected toolkit.
    /// Selection rule: known read-only actions first, then the order of
    /// Composio's tool list, up to this number.  The rest is reachable through
    /// `apps_find_action` and `apps_run_action`.
    pub cap_per_toolkit: usize,
    native_first: BTreeSet<String>,
    /// Popular row, in display order.
    pub popular: Vec<Popular>,
    read_only: BTreeSet<String>,
}

impl Curated {
    /// The table compiled into the binary.
    ///
    /// # Panics
    /// Never in a build that passed the tests.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_json(include_str!("curated.json")).expect("curated.json")
    }

    /// Parse a table (tests use it to exercise the `native_first` mechanism).
    ///
    /// # Errors
    /// Malformed JSON.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let f: File = serde_json::from_str(text)?;
        Ok(Self {
            cap_per_toolkit: f.cap_per_toolkit,
            native_first: f.native_first.into_iter().collect(),
            popular: f.popular,
            read_only: f.read_only_actions.into_iter().collect(),
        })
    }

    /// Whether Praxi connects (or will connect) `slug` itself.
    #[must_use]
    pub fn is_native_first(&self, slug: &str) -> bool {
        self.native_first.contains(slug)
    }

    /// Whether `tool_slug` is a well-known read-only action.
    #[must_use]
    pub fn is_known_read(&self, tool_slug: &str) -> bool {
        self.read_only.contains(tool_slug)
    }

    /// Whether `slug` is one of the apps Praxi pins in its popular row.
    #[must_use]
    pub fn is_popular(&self, slug: &str) -> bool {
        self.popular.iter().any(|p| p.slug == slug)
    }

    /// Display name of a popular slug.
    #[must_use]
    pub fn popular_name(&self, slug: &str) -> Option<&str> {
        self.popular
            .iter()
            .find(|p| p.slug == slug)
            .map(|p| p.name.as_str())
    }
}

/// Short action name of a Composio tool slug inside its toolkit:
/// `GITHUB_CREATE_AN_ISSUE` of `github` is `create_an_issue`.
#[must_use]
pub fn short_name(toolkit: &str, tool_slug: &str) -> String {
    let lower = tool_slug.to_ascii_lowercase();
    let prefix = format!("{}_", toolkit.to_ascii_lowercase());
    lower.strip_prefix(&prefix).unwrap_or(&lower).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_loads_and_classifies_for_display() {
        let c = Curated::builtin();
        assert!(c.cap_per_toolkit > 0);
        // Empty until a zero-setup direct path (remote MCP + OAuth) exists.
        assert!(!c.is_native_first("github") && !c.is_native_first("instagram"));
        let moved = Curated::from_json(
            r#"{"cap_per_toolkit":5,"native_first":["github"],"popular":[],"read_only_actions":[]}"#,
        )
        .unwrap();
        assert!(moved.is_native_first("github"));
        assert_eq!(c.popular_name("twitter"), Some("X"));
        assert!(c.is_known_read("SLACK_LIST_ALL_CHANNELS"));
        assert!(!c.is_known_read("SLACK_SEND_MESSAGE"));
        assert_eq!(
            short_name("github", "GITHUB_CREATE_AN_ISSUE"),
            "create_an_issue"
        );
        assert_eq!(short_name("x", "OTHER_THING"), "other_thing");
    }
}
