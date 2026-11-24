//! Per-bot memory settings: the `memory` table of a bot's stored profile.
//!
//! Only the local owner can change it (the daemon's owner-only route); bot
//! create / update keep the stored table, like the engine settings.  Every
//! reader in the loop and in the tools goes through [`MemorySettings`], so the
//! switch cannot be honoured in one place and forgotten in another.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default recall budget (tokens).
pub const DEFAULT_BUDGET_TOKENS: usize = 800;
/// Smallest budget the slider offers.
pub const MIN_BUDGET_TOKENS: usize = 200;
/// Largest budget the slider offers.
pub const MAX_BUDGET_TOKENS: usize = 2000;

/// What this bot does with memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// Learns from chats and recalls.  Anything learned from web pages, files
    /// or tool output waits for the owner's OK first.
    #[default]
    On,
    /// No learning, no recall, no memory tools.
    Off,
    /// Like `On`, but every new memory waits for the owner's OK.
    Ask,
}

/// The `memory` table of a bot profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// On / Off / Ask before saving.
    pub mode: MemoryMode,
    /// Recall budget in tokens (advanced); `None` = the host default (800).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<usize>,
    /// Profile and episodic memories learned here are shared with the owner's
    /// other bots (default).  Off keeps everything this bot learns to itself.
    pub share_with_other_bots: bool,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            mode: MemoryMode::On,
            budget_tokens: None,
            share_with_other_bots: true,
        }
    }
}

impl MemorySettings {
    /// Settings stored in a bot profile JSON; a missing or unreadable table
    /// means the defaults (On, 800 tokens, shared).
    #[must_use]
    pub fn from_bot_config(config: &Value) -> Self {
        config
            .get("memory")
            .and_then(|v| serde_json::from_value::<Self>(v.clone()).ok())
            .unwrap_or_default()
    }

    /// The budget actually used: the slider range is enforced here too, so a
    /// hand-edited profile cannot turn recall into a giant prompt.
    #[must_use]
    pub fn budget(&self, host_default: usize) -> usize {
        self.budget_tokens.map_or(host_default, |b| {
            b.clamp(MIN_BUDGET_TOKENS, MAX_BUDGET_TOKENS)
        })
    }

    /// Whether the bot learns, recalls and offers memory tools at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.mode != MemoryMode::Off
    }

    /// Whether a new memory must wait for the owner's OK.
    #[must_use]
    pub fn holds(&self, untrusted_origin: bool) -> bool {
        match self.mode {
            MemoryMode::Off | MemoryMode::Ask => true,
            MemoryMode::On => untrusted_origin,
        }
    }

    /// Validate an owner's change.
    ///
    /// # Errors
    /// A budget outside the slider range.
    pub fn validate(&self) -> Result<(), String> {
        if self
            .budget_tokens
            .is_some_and(|b| !(MIN_BUDGET_TOKENS..=MAX_BUDGET_TOKENS).contains(&b))
        {
            return Err(format!(
                "memory budget must be between {MIN_BUDGET_TOKENS} and {MAX_BUDGET_TOKENS} tokens"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_clamp() {
        let d = MemorySettings::from_bot_config(&json!({}));
        assert_eq!(d, MemorySettings::default());
        assert!(d.enabled() && d.share_with_other_bots);
        let huge = MemorySettings::from_bot_config(&json!({"memory": {"budget_tokens": 999_999}}));
        assert_eq!(huge.budget(DEFAULT_BUDGET_TOKENS), MAX_BUDGET_TOKENS);
        assert_eq!(d.budget(DEFAULT_BUDGET_TOKENS), DEFAULT_BUDGET_TOKENS);
        assert!(huge.validate().is_err());
        // Unreadable table (unknown field) falls back to the defaults.
        let bad = MemorySettings::from_bot_config(&json!({"memory": {"mode": "on", "x": 1}}));
        assert_eq!(bad, MemorySettings::default());
    }

    #[test]
    fn hold_rules() {
        let on = MemorySettings::default();
        assert!(!on.holds(false) && on.holds(true));
        let ask = MemorySettings {
            mode: MemoryMode::Ask,
            ..MemorySettings::default()
        };
        assert!(ask.holds(false) && ask.holds(true));
    }
}
