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
    policy::{Level, Policy, Rule},
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
    Ask,
    /// The action is unconditionally denied.
    Deny,
}

impl Decision {
    /// Return the stricter (least permissive) of two decisions.
    #[must_use]
    pub fn stricter(self, other: Decision) -> Decision {
        match (self, other) {
            (Decision::Deny, _) | (_, Decision::Deny) => Decision::Deny,
            (Decision::Ask, _) | (_, Decision::Ask) => Decision::Ask,
            _ => Decision::Allow,
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
            decision = Decision::Ask;
        }

        // 4b. Never-list match always requires Ask.
        if !req.raw_command.is_empty() && self.never_list.matches(&req.raw_command) {
            decision = decision.stricter(Decision::Ask);
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
        Level::Ask => Decision::Ask,
        Level::Allow | Level::AllowWithLimits(_) => Decision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Level, Policy, Rule};
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
        assert_eq!(decision, Decision::Ask);
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
        assert_eq!(decision, Decision::Ask);
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
        assert_eq!(decision, Decision::Ask);
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
            Decision::Ask
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
            Decision::Ask
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
}
