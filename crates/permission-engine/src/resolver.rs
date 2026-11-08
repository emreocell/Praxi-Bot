//! Permission resolution logic.
//!
//! [`PermissionResolver::resolve`] implements the layered resolution rules from
//! ADR-0012:
//!
//! 1. Category-level override: bot rule for a category wins over global; missing
//!    category falls back to global.
//! 2. Sub-agent can never exceed its parent bot's effective level (clamp).
//! 3. Absolute floors regardless of policy: `Money` → always at least `Ask`;
//!    never-list match → always `Ask`.
//! 4. Per-initiator overrides are data in [`Rule::by_initiator`] — no hardcoded
//!    rules exist here.

use protocol::{InitiatorKind, PermissionCategory};

use crate::{
    never_list::NeverList,
    policy::{Level, Limits, Policy, Rule},
};

/// A request to perform a potentially restricted action.
#[derive(Debug, Clone)]
pub struct ActionRequest {
    /// The permission category of the requested action.
    pub category: PermissionCategory,
    /// The raw command string (used for never-list matching).
    ///
    /// For non-shell actions this may be empty.
    pub raw_command: String,
    /// Who initiated the run.
    pub initiator: InitiatorKind,
}

/// The resolved permission decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// The action is allowed without further approval.
    Allow,
    /// The action requires real-time human approval before proceeding.
    ///
    /// `unsupported_limits` is set when this ask exists because
    /// [`Level::AllowWithLimits`] cannot be evaluated yet: [`ActionRequest`]
    /// carries no path, URL, or amount and there is no rate counter. Those
    /// constraints are preserved here so callers can refuse to waive the ask
    /// through read shortcuts or existing grants. `None` is an ordinary
    /// policy-level ask, which those shortcuts may still waive.
    Ask { unsupported_limits: Option<Limits> },
    /// The action is unconditionally denied.
    Deny,
}

impl Decision {
    /// A policy-level ask that is not caused by unevaluated limits.
    #[must_use]
    pub const fn ask() -> Self {
        Self::Ask {
            unsupported_limits: None,
        }
    }

    /// Whether this decision requires approval (plain or unsupported-limit).
    #[must_use]
    pub const fn is_ask(&self) -> bool {
        matches!(self, Self::Ask { .. })
    }

    /// Limits that were configured but could not be enforced, when this ask
    /// exists for that reason.
    #[must_use]
    pub fn unsupported_limits(&self) -> Option<&Limits> {
        match self {
            Self::Ask { unsupported_limits } => unsupported_limits.as_ref(),
            _ => None,
        }
    }

    /// Return the stricter (least permissive) of two decisions.
    ///
    /// An unsupported-limit ask is preserved when combined with a plain ask:
    /// the flag must not disappear just because another rule also asks.
    #[must_use]
    pub fn stricter(self, other: Decision) -> Decision {
        match (self, other) {
            (Decision::Deny, _) | (_, Decision::Deny) => Decision::Deny,
            (
                Decision::Ask {
                    unsupported_limits: a,
                },
                Decision::Ask {
                    unsupported_limits: b,
                },
            ) => Decision::Ask {
                unsupported_limits: a.or(b),
            },
            (ask @ Decision::Ask { .. }, _) | (_, ask @ Decision::Ask { .. }) => ask,
            (Decision::Allow, Decision::Allow) => Decision::Allow,
        }
    }
}

/// Stateless permission resolver.
///
/// Create one instance per process and call [`resolve`][Self::resolve] for each
/// action.
pub struct PermissionResolver {
    never_list: NeverList,
}

impl PermissionResolver {
    /// Construct a resolver with the default never-list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            never_list: NeverList::default_list(),
        }
    }

    /// Construct a resolver with a custom never-list.
    #[must_use]
    pub fn with_never_list(never_list: NeverList) -> Self {
        Self { never_list }
    }

    /// Resolve the effective permission decision.
    ///
    /// Parameters
    /// ----------
    /// - `global`: the workspace-wide policy.
    /// - `bot`: the bot-profile policy (if any).
    /// - `subagent`: the sub-agent policy (if any; must be a subset of `bot`).
    /// - `req`: the action being evaluated.
    #[must_use]
    pub fn resolve(
        &self,
        global: &Policy,
        bot: Option<&Policy>,
        subagent: Option<&Policy>,
        req: &ActionRequest,
    ) -> Decision {
        // Agent-internal tools without external effect are always allowed.
        if req.category == PermissionCategory::Meta {
            return Decision::Allow;
        }

        // Step 1 — compute the effective level for this category and
        // initiator kind (data-driven via `Rule::by_initiator`).
        let level = Self::effective_level(global, bot, subagent, req);

        // Step 2 — convert level to decision.
        let mut decision = level_to_decision(&level);

        // Step 4 — absolute floors.
        // 4a. Money always requires at least Ask.
        if req.category == PermissionCategory::Money && decision == Decision::Allow {
            decision = Decision::ask();
        }

        // 4b. Never-list match always requires Ask.
        if !req.raw_command.is_empty() && self.never_list.matches(&req.raw_command) {
            decision = decision.stricter(Decision::ask());
        }

        decision
    }

    /// Compute the effective [`Level`] for the request's category and
    /// initiator kind after applying override and clamp rules.
    fn effective_level(
        global: &Policy,
        bot: Option<&Policy>,
        subagent: Option<&Policy>,
        req: &ActionRequest,
    ) -> Level {
        // Rule 1: bot overrides global per-category; fallback to global.
        let bot_rule: Rule = if let Some(b) = bot {
            b.categories
                .get(&req.category)
                .or_else(|| global.categories.get(&req.category))
                .cloned()
                .unwrap_or_default()
        } else {
            global
                .categories
                .get(&req.category)
                .cloned()
                .unwrap_or_default()
        };

        // The bot's level for this initiator kind is the ceiling.
        let bot_level = bot_rule.level_for(req.initiator);

        // Rule 2: sub-agent cannot exceed bot's effective level.
        //
        // The sub-agent's level for this initiator kind (its override, or its
        // base level) is clamped by the bot's level for the *same* initiator
        // kind, so a bot-side per-initiator restriction cannot be bypassed by
        // a sub-agent that has no override of its own.
        if let Some(sa) = subagent {
            let sa_rule: Rule = sa
                .categories
                .get(&req.category)
                .cloned()
                .unwrap_or_default();
            sa_rule.level_for(req.initiator).clamp_to(bot_level)
        } else {
            bot_level.clone()
        }
    }
}

impl Default for PermissionResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a resolved [`Level`] to a [`Decision`].
fn level_to_decision(level: &Level) -> Decision {
    match level {
        Level::Deny => Decision::Deny,
        Level::Ask => Decision::ask(),
        // Limits (`path_patterns`, `max_per_hour`, `max_amount_minor`) are not
        // enforced yet: `ActionRequest` carries no path/URL/amount and there is
        // no rate counter.  Until they are, fail closed and ask, and keep the
        // constraints on the decision so a later layer cannot waive them.
        Level::AllowWithLimits(limits) => Decision::Ask {
            unsupported_limits: Some(limits.clone()),
        },
        Level::Allow => Decision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Level, Limits, Policy, Rule};
    use protocol::InitiatorKind;

    fn req_human(cat: PermissionCategory) -> ActionRequest {
        ActionRequest {
            category: cat,
            raw_command: String::new(),
            initiator: InitiatorKind::Human,
        }
    }

    fn req_routine(cat: PermissionCategory) -> ActionRequest {
        ActionRequest {
            category: cat,
            raw_command: String::new(),
            initiator: InitiatorKind::Routine,
        }
    }

    // ── Rule 1: category-level override ──────────────────────────────────────

    #[test]
    fn bot_category_overrides_global() {
        let global = Policy::approve_destructive(); // all Ask
        let mut bot = Policy::default();
        bot.categories
            .insert(PermissionCategory::Fs, Rule::new(Level::Allow));

        let resolver = PermissionResolver::new();
        let decision = resolver.resolve(
            &global,
            Some(&bot),
            None,
            &req_human(PermissionCategory::Fs),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn missing_bot_category_falls_back_to_global() {
        let global = Policy::full(); // all Allow
        let bot = Policy::default(); // empty — no overrides

        let resolver = PermissionResolver::new();
        let decision = resolver.resolve(
            &global,
            Some(&bot),
            None,
            &req_human(PermissionCategory::Shell),
        );
        assert_eq!(decision, Decision::Allow);
    }

    // ── Rule 2: sub-agent clamp ───────────────────────────────────────────────

    #[test]
    fn subagent_cannot_exceed_bot_level() {
        let global = Policy::full();
        let mut bot = Policy::default();
        bot.categories
            .insert(PermissionCategory::Shell, Rule::new(Level::Ask));

        let mut subagent = Policy::default();
        subagent
            .categories
            .insert(PermissionCategory::Shell, Rule::new(Level::Allow));

        let resolver = PermissionResolver::new();
        let decision = resolver.resolve(
            &global,
            Some(&bot),
            Some(&subagent),
            &req_human(PermissionCategory::Shell),
        );
        // Sub-agent wants Allow but bot ceiling is Ask → must be Ask.
        assert_eq!(decision, Decision::ask());
    }

    #[test]
    fn subagent_can_be_more_restrictive_than_bot() {
        let global = Policy::full();
        let mut bot = Policy::default();
        bot.categories
            .insert(PermissionCategory::Shell, Rule::new(Level::Allow));

        let mut subagent = Policy::default();
        subagent
            .categories
            .insert(PermissionCategory::Shell, Rule::new(Level::Deny));

        let resolver = PermissionResolver::new();
        let decision = resolver.resolve(
            &global,
            Some(&bot),
            Some(&subagent),
            &req_human(PermissionCategory::Shell),
        );
        assert_eq!(decision, Decision::Deny);
    }

    // ── AllowWithLimits is fail-closed ───────────────────────────────────────

    #[test]
    fn allow_with_limits_asks_until_limits_are_enforced() {
        let mut global = Policy::default();
        global.categories.insert(
            PermissionCategory::Fs,
            Rule::new(Level::AllowWithLimits(crate::policy::Limits {
                max_per_hour: Some(1),
                max_amount_minor: None,
                path_patterns: vec!["src/**".into()],
            })),
        );
        let resolver = PermissionResolver::new();
        let d = resolver.resolve(&global, None, None, &req_human(PermissionCategory::Fs));
        let Some(limits) = d.unsupported_limits() else {
            panic!("AllowWithLimits must stay an ask and carry the limits, got {d:?}");
        };
        assert_eq!(limits.max_per_hour, Some(1));
        assert_eq!(limits.path_patterns, vec!["src/**".to_string()]);
    }

    #[test]
    fn allow_with_limits_flag_survives_a_plain_ask_floor() {
        // A never-list hit is a plain ask. Combining it with AllowWithLimits
        // must not clear the unsupported-limits flag.
        let mut global = Policy::default();
        global.categories.insert(
            PermissionCategory::Shell,
            Rule::new(Level::AllowWithLimits(Limits {
                max_per_hour: None,
                max_amount_minor: None,
                path_patterns: vec!["workspace/**".into()],
            })),
        );
        let resolver = PermissionResolver::new();
        let d = resolver.resolve(
            &global,
            None,
            None,
            &ActionRequest {
                category: PermissionCategory::Shell,
                raw_command: "rm -rf /".into(),
                initiator: InitiatorKind::Human,
            },
        );
        let Some(limits) = d.unsupported_limits() else {
            panic!("flag dropped when combined with the never-list, got {d:?}");
        };
        assert_eq!(limits.path_patterns, vec!["workspace/**".to_string()]);
        // A plain ask (no limits configured) does not carry the flag.
        assert!(
            resolver
                .resolve(
                    &Policy::approve_destructive(),
                    None,
                    None,
                    &req_human(PermissionCategory::Fs)
                )
                .unsupported_limits()
                .is_none()
        );
    }

    // ── Meta category ─────────────────────────────────────────────────────────

    #[test]
    fn meta_is_always_allowed() {
        let resolver = PermissionResolver::new();
        for global in [Policy::read_only(), Policy::approve_destructive()] {
            let d = resolver.resolve(&global, None, None, &req_human(PermissionCategory::Meta));
            assert_eq!(d, Decision::Allow);
        }
        let mut bot = Policy::default();
        bot.categories
            .insert(PermissionCategory::Meta, Rule::new(Level::Deny));
        let d = resolver.resolve(
            &Policy::read_only(),
            Some(&bot),
            None,
            &req_routine(PermissionCategory::Meta),
        );
        assert_eq!(d, Decision::Allow);
    }

    // ── Rule 3a: Money absolute floor ─────────────────────────────────────────

    #[test]
    fn money_always_requires_ask_even_with_full_policy() {
        let global = Policy::full();
        let resolver = PermissionResolver::new();
        let decision = resolver.resolve(&global, None, None, &req_human(PermissionCategory::Money));
        assert_eq!(decision, Decision::ask());
    }

    // ── Rule 3b: Never-list match forces Ask ──────────────────────────────────

    #[test]
    fn never_list_match_requires_ask_even_with_full_policy() {
        let global = Policy::full();
        let resolver = PermissionResolver::new();
        let r = ActionRequest {
            category: PermissionCategory::Shell,
            raw_command: "rm -rf /".into(),
            initiator: InitiatorKind::Human,
        };
        let decision = resolver.resolve(&global, None, None, &r);
        assert_eq!(decision, Decision::ask());
    }

    // ── Rule 4: by_initiator data-driven overrides ───────────────────────────

    #[test]
    fn routine_initiator_asks_for_messaging_when_rule_says_so() {
        let global = Policy::full();
        let mut bot = Policy::full();

        // Bot configures: Messaging is Allow for everyone EXCEPT Routine → Ask.
        let mut messaging_rule = Rule::new(Level::Allow);
        messaging_rule
            .by_initiator
            .insert(InitiatorKind::Routine, Level::Ask);
        bot.categories
            .insert(PermissionCategory::Messaging, messaging_rule);

        let resolver = PermissionResolver::new();

        // Routine gets Ask.
        let routine_req = req_routine(PermissionCategory::Messaging);
        assert_eq!(
            resolver.resolve(&global, Some(&bot), None, &routine_req),
            Decision::ask()
        );

        // Human gets Allow.
        let human_req = req_human(PermissionCategory::Messaging);
        assert_eq!(
            resolver.resolve(&global, Some(&bot), None, &human_req),
            Decision::Allow
        );
    }

    #[test]
    fn no_by_initiator_entry_both_get_allow() {
        let global = Policy::full();
        // No by_initiator configured: everyone gets Allow.
        let resolver = PermissionResolver::new();

        assert_eq!(
            resolver.resolve(
                &global,
                None,
                None,
                &req_routine(PermissionCategory::Messaging)
            ),
            // Money floor does not apply to Messaging.
            Decision::Allow
        );
        assert_eq!(
            resolver.resolve(
                &global,
                None,
                None,
                &req_human(PermissionCategory::Messaging)
            ),
            Decision::Allow
        );
    }

    #[test]
    fn bot_permission_wins_no_hardcoded_routine_messaging_rule() {
        // Bot explicitly allows Messaging with no per-initiator entry:
        // a routine must get Allow (ADR-0012: the bot's permission wins).
        let global = Policy::approve_destructive();
        let mut bot = Policy::default();
        bot.categories
            .insert(PermissionCategory::Messaging, Rule::new(Level::Allow));
        let resolver = PermissionResolver::new();
        assert_eq!(
            resolver.resolve(
                &global,
                Some(&bot),
                None,
                &req_routine(PermissionCategory::Messaging)
            ),
            Decision::Allow
        );
    }

    #[test]
    fn subagent_cannot_bypass_bot_per_initiator_restriction() {
        let global = Policy::full();
        let mut bot = Policy::default();
        let mut rule = Rule::new(Level::Allow);
        rule.by_initiator.insert(InitiatorKind::Routine, Level::Ask);
        bot.categories.insert(PermissionCategory::Messaging, rule);

        // Sub-agent allows everything with no per-initiator entry.
        let mut subagent = Policy::default();
        subagent
            .categories
            .insert(PermissionCategory::Messaging, Rule::new(Level::Allow));

        let resolver = PermissionResolver::new();
        assert_eq!(
            resolver.resolve(
                &global,
                Some(&bot),
                Some(&subagent),
                &req_routine(PermissionCategory::Messaging)
            ),
            Decision::ask()
        );
        assert_eq!(
            resolver.resolve(
                &global,
                Some(&bot),
                Some(&subagent),
                &req_human(PermissionCategory::Messaging)
            ),
            Decision::Allow
        );
    }

    fn req_schedule(cat: PermissionCategory, cmd: &str) -> ActionRequest {
        ActionRequest {
            category: cat,
            raw_command: cmd.to_owned(),
            initiator: InitiatorKind::Schedule,
        }
    }

    #[test]
    fn schedule_behaves_like_the_user_by_default() {
        let resolver = PermissionResolver::new();
        for global in [
            Policy::full(),
            Policy::approve_destructive(),
            Policy::read_only(),
        ] {
            for cat in [PermissionCategory::Fs, PermissionCategory::Browser] {
                assert_eq!(
                    resolver.resolve(&global, None, None, &req_schedule(cat, "")),
                    resolver.resolve(&global, None, None, &req_human(cat)),
                );
            }
        }
    }

    #[test]
    fn schedule_never_auto_approves_money_or_never_list() {
        let resolver = PermissionResolver::new();
        let full = Policy::full();
        assert_eq!(
            resolver.resolve(
                &full,
                None,
                None,
                &req_schedule(PermissionCategory::Money, "")
            ),
            Decision::ask()
        );
        assert_eq!(
            resolver.resolve(
                &full,
                None,
                None,
                &req_schedule(PermissionCategory::Shell, "rm -rf /")
            ),
            Decision::ask()
        );
        // A schedule-specific Allow override cannot lower the floors either.
        let mut bot = Policy::default();
        let mut rule = Rule::new(Level::Ask);
        rule.by_initiator
            .insert(InitiatorKind::Schedule, Level::Allow);
        bot.categories.insert(PermissionCategory::Money, rule);
        assert_eq!(
            resolver.resolve(
                &full,
                Some(&bot),
                None,
                &req_schedule(PermissionCategory::Money, "")
            ),
            Decision::ask()
        );
    }

    #[test]
    fn schedule_override_is_independent_of_human() {
        let global = Policy::full();
        let mut bot = Policy::default();
        let mut rule = Rule::new(Level::Allow);
        rule.by_initiator
            .insert(InitiatorKind::Schedule, Level::Deny);
        bot.categories.insert(PermissionCategory::Messaging, rule);
        let resolver = PermissionResolver::new();
        assert_eq!(
            resolver.resolve(
                &global,
                Some(&bot),
                None,
                &req_schedule(PermissionCategory::Messaging, "")
            ),
            Decision::Deny
        );
        assert_eq!(
            resolver.resolve(
                &global,
                Some(&bot),
                None,
                &req_human(PermissionCategory::Messaging)
            ),
            Decision::Allow
        );
    }
}
