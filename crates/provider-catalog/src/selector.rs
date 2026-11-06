//! Model selection logic.
//!
//! Selection priority (ADR-0016):
//! 1. Sub-agent explicit model.
//! 2. Bot-role model.
//! 3. Global default.
//! 4. First registered provider's default.

use crate::provider::ModelRef;

/// Which role a model is being selected for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotRole {
    Planner,
    Executor,
    Classifier,
    Fallback,
}

/// Stateless model selector.
pub struct ModelSelector;

impl ModelSelector {
    /// Pick the most appropriate model reference.
    ///
    /// Parameters follow the priority order from ADR-0016:
    /// - `subagent`: explicit override from the sub-agent (highest priority).
    /// - `bot_role`: model pinned to this bot + role combination.
    /// - `global_default`: workspace-wide default.
    /// - `first_added`: the first provider registered (lowest priority).
    #[must_use]
    pub fn pick(
        subagent: Option<&ModelRef>,
        bot_role: Option<&ModelRef>,
        global_default: Option<&ModelRef>,
        first_added: Option<&ModelRef>,
    ) -> Option<ModelRef> {
        subagent
            .or(bot_role)
            .or(global_default)
            .or(first_added)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mref(s: &str) -> ModelRef {
        ModelRef(s.into())
    }

    #[test]
    fn subagent_wins_over_all() {
        let result = ModelSelector::pick(
            Some(&mref("sa/model")),
            Some(&mref("bot/model")),
            Some(&mref("global/model")),
            Some(&mref("first/model")),
        );
        assert_eq!(result.unwrap().0, "sa/model");
    }

    #[test]
    fn bot_role_wins_when_no_subagent() {
        let result = ModelSelector::pick(
            None,
            Some(&mref("bot/model")),
            Some(&mref("global/model")),
            Some(&mref("first/model")),
        );
        assert_eq!(result.unwrap().0, "bot/model");
    }

    #[test]
    fn global_wins_when_no_subagent_or_bot() {
        let result = ModelSelector::pick(
            None,
            None,
            Some(&mref("global/model")),
            Some(&mref("first/model")),
        );
        assert_eq!(result.unwrap().0, "global/model");
    }

    #[test]
    fn first_added_is_last_resort() {
        let result = ModelSelector::pick(None, None, None, Some(&mref("first/model")));
        assert_eq!(result.unwrap().0, "first/model");
    }

    #[test]
    fn all_none_returns_none() {
        let result = ModelSelector::pick(None, None, None, None);
        assert!(result.is_none());
    }
}
