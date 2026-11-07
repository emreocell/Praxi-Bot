//! Policy types and preset constructors.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use protocol::{InitiatorKind, PermissionCategory};

/// Optional upper bounds on `allow-with-limits` grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum number of operations per hour (if applicable).
    pub max_per_hour: Option<u32>,
    /// Maximum monetary amount per operation in the smallest currency unit (e.g. cents).
    pub max_amount_minor: Option<u64>,
    /// Glob patterns restricting which file paths / URLs are accessible.
    pub path_patterns: Vec<String>,
}

/// The permission level for a category.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Level {
    /// The action is always denied.
    Deny,
    /// The action requires real-time human approval.
    #[default]
    Ask,
    /// The action is always allowed.
    Allow,
    /// The action is allowed within specified limits; outside them it needs approval.
    ///
    /// **Not enforced yet:** the resolver cannot evaluate limits (requests carry
    /// no path/URL/amount and there is no rate counter), so it currently
    /// resolves this level to `Ask` (fail-closed).
    AllowWithLimits(Limits),
}

impl Level {
    /// Return `true` if this level is at least as permissive as `other`.
    ///
    /// Order: `Deny` < `Ask` < `AllowWithLimits` < `Allow`.
    #[must_use]
    pub fn is_at_least_as_permissive_as(&self, other: &Level) -> bool {
        self.rank() >= other.rank()
    }

    fn rank(&self) -> u8 {
        match self {
            Level::Deny => 0,
            Level::Ask => 1,
            Level::AllowWithLimits(_) => 2,
            Level::Allow => 3,
        }
    }

    /// Clamp `self` down so it is no more permissive than `ceiling`.
    #[must_use]
    pub fn clamp_to(&self, ceiling: &Level) -> Level {
        if self.rank() <= ceiling.rank() {
            self.clone()
        } else {
            ceiling.clone()
        }
    }
}

/// A single permission rule for a category.
///
/// The base `level` applies to all initiator kinds unless `by_initiator`
/// provides an override for the specific initiator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Rule {
    /// The effective level for this rule (default for all initiator kinds).
    pub level: Level,
    /// Per-initiator-kind overrides.
    ///
    /// If the request's initiator kind has an entry here, that level is used
    /// instead of `level`.  Clamp semantics still apply.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_initiator: BTreeMap<InitiatorKind, Level>,
}

impl Rule {
    /// Construct a simple rule with no per-initiator overrides.
    #[must_use]
    pub fn new(level: Level) -> Self {
        Self {
            level,
            by_initiator: BTreeMap::new(),
        }
    }

    /// Return the effective level for the given initiator kind.
    ///
    /// Uses the per-initiator override if present, otherwise the base level.
    /// A [`InitiatorKind::Schedule`] run behaves like the user by default: with
    /// no override of its own it follows the `Human` override, if any.
    #[must_use]
    pub fn level_for(&self, initiator: InitiatorKind) -> &Level {
        self.by_initiator
            .get(&initiator)
            .or_else(|| {
                (initiator == InitiatorKind::Schedule)
                    .then(|| self.by_initiator.get(&InitiatorKind::Human))
                    .flatten()
            })
            .unwrap_or(&self.level)
    }
}

/// A set of per-category permission rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    /// Per-category rules.  Categories absent from this map inherit from the
    /// outer policy (see [`PermissionResolver`][crate::resolver::PermissionResolver]).
    pub categories: BTreeMap<PermissionCategory, Rule>,
}

impl Policy {
    /// Preset: read-only across all categories (all actions denied).
    #[must_use]
    pub fn read_only() -> Self {
        Self::uniform(&Level::Deny)
    }

    /// Preset: reads are always allowed; writes / destructive ops ask for approval.
    ///
    /// This is the default for new installations.
    #[must_use]
    pub fn approve_destructive() -> Self {
        Self::uniform(&Level::Ask)
    }

    /// Preset: everything is allowed without approval.
    #[must_use]
    pub fn full() -> Self {
        Self::uniform(&Level::Allow)
    }

    /// Preset: reads allowed, drafts (non-sending) allowed; sending requires approval.
    #[must_use]
    pub fn read_and_draft() -> Self {
        let mut p = Self::approve_destructive();
        p.categories
            .insert(PermissionCategory::Fs, Rule::new(Level::Ask));
        p.categories
            .insert(PermissionCategory::Browser, Rule::new(Level::Ask));
        p
    }

    /// Build a policy that uses `default_level` for all categories.
    fn uniform(default_level: &Level) -> Self {
        let categories = [
            PermissionCategory::Fs,
            PermissionCategory::Shell,
            PermissionCategory::Desktop,
            PermissionCategory::Browser,
            PermissionCategory::Accounts,
            PermissionCategory::Messaging,
            PermissionCategory::Money,
            PermissionCategory::System,
            PermissionCategory::Remote,
            PermissionCategory::Script,
        ]
        .into_iter()
        .map(|cat| (cat, Rule::new(default_level.clone())))
        .collect();
        Self { categories }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_clamp_respects_ceiling() {
        let allow = Level::Allow;
        let ask = Level::Ask;
        assert_eq!(allow.clamp_to(&ask), Level::Ask);
        assert_eq!(ask.clamp_to(&allow), Level::Ask);
    }

    #[test]
    fn read_only_has_deny_for_all_categories() {
        let p = Policy::read_only();
        for rule in p.categories.values() {
            assert_eq!(rule.level, Level::Deny, "read_only should deny all writes");
        }
    }

    #[test]
    fn full_policy_allows_everything() {
        let p = Policy::full();
        for rule in p.categories.values() {
            assert_eq!(rule.level, Level::Allow);
        }
    }

    #[test]
    fn rule_level_for_uses_override_when_present() {
        let mut rule = Rule::new(Level::Allow);
        rule.by_initiator.insert(InitiatorKind::Routine, Level::Ask);
        assert_eq!(rule.level_for(InitiatorKind::Routine), &Level::Ask);
        assert_eq!(rule.level_for(InitiatorKind::Human), &Level::Allow);
    }

    #[test]
    fn rule_level_for_falls_back_to_base_level() {
        let rule = Rule::new(Level::Allow);
        assert_eq!(rule.level_for(InitiatorKind::Subagent), &Level::Allow);
    }

    #[test]
    fn schedule_follows_human_unless_overridden() {
        let mut rule = Rule::new(Level::Ask);
        assert_eq!(rule.level_for(InitiatorKind::Schedule), &Level::Ask);
        rule.by_initiator.insert(InitiatorKind::Human, Level::Allow);
        assert_eq!(rule.level_for(InitiatorKind::Schedule), &Level::Allow);
        rule.by_initiator
            .insert(InitiatorKind::Schedule, Level::Deny);
        assert_eq!(rule.level_for(InitiatorKind::Schedule), &Level::Deny);
        assert_eq!(rule.level_for(InitiatorKind::Human), &Level::Allow);
    }
}
