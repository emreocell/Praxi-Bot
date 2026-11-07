//! Loop configuration: guards, pricing, global policy.

use std::collections::HashMap;
use std::time::Duration;

use permission_engine::Policy;
use task_runtime::RetryPolicy;

/// Per-turn safety limits.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Maximum model calls in one turn.
    pub max_steps: u32,
    /// Timeout for a single tool call.
    pub tool_timeout: Duration,
    /// Identical consecutive tool calls at which a warning is injected.
    pub repeat_warn: u32,
    /// Identical consecutive tool calls at which the turn is stopped.
    pub repeat_stop: u32,
    /// Token budget (prompt + completion, summed over the turn's model calls).
    pub max_tokens_per_turn: u64,
    /// Tool output above this many bytes is spilled to a file.
    pub spill_threshold_bytes: usize,
    /// Bytes of a spilled output kept inline as a preview.
    pub spill_preview_bytes: usize,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            max_steps: 40,
            tool_timeout: Duration::from_secs(120),
            repeat_warn: 3,
            repeat_stop: 5,
            max_tokens_per_turn: 2_000_000,
            spill_threshold_bytes: 16 * 1024,
            spill_preview_bytes: 2 * 1024,
        }
    }
}

/// USD price per million tokens for a model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    /// Uncached prompt tokens.
    pub input_per_mtok: f64,
    /// Completion tokens.
    pub output_per_mtok: f64,
    /// Cached prompt tokens (falls back to `input_per_mtok`).
    pub cached_per_mtok: Option<f64>,
}

impl ModelPrice {
    /// Cost in USD for the given token counts.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn cost(&self, prompt: u64, completion: u64, cached: Option<u64>) -> f64 {
        let cached = cached.unwrap_or(0).min(prompt);
        let fresh = prompt - cached;
        let cached_price = self.cached_per_mtok.unwrap_or(self.input_per_mtok);
        (fresh as f64 * self.input_per_mtok
            + cached as f64 * cached_price
            + completion as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// Configuration of an [`AgentLoop`](crate::AgentLoop).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Guards.
    pub guards: Guards,
    /// Workspace-wide policy (ADR-0012); bots override per category.
    pub global_policy: Policy,
    /// Retry policy for model steps (they are idempotent).
    pub model_retry: RetryPolicy,
    /// Prices keyed by the full model reference (`provider/model`).
    pub prices: HashMap<String, ModelPrice>,
    /// Tools with `Effect::Read` skip an `Ask` that comes from the policy level
    /// (never from the absolute floors).
    pub read_tools_skip_ask: bool,
    /// Response token cap sent to the provider.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            guards: Guards::default(),
            global_policy: Policy::approve_destructive(),
            model_retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 500,
                multiplier: 2,
                max_backoff_ms: 8_000,
            },
            prices: HashMap::new(),
            read_tools_skip_ask: true,
            max_tokens: None,
            temperature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_splits_cached_tokens() {
        let p = ModelPrice {
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cached_per_mtok: Some(0.5),
        };
        let c = p.cost(1_000_000, 100_000, Some(400_000));
        assert!((c - (1.2 + 0.2 + 1.0)).abs() < 1e-9);
    }
}
