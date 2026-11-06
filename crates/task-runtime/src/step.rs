//! Step description, retry policy and the executor trait.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::{Run, RunStep};
use tokio_util::sync::CancellationToken;

/// Retry data for **idempotent** steps; ignored for steps not flagged idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Total attempts including the first (1 = no retry).
    pub max_attempts: u32,
    /// Delay after the first failed attempt, in milliseconds.
    pub initial_backoff_ms: u64,
    /// Multiplier applied per further failure.
    pub multiplier: u32,
    /// Upper bound for the delay, in milliseconds.
    pub max_backoff_ms: u64,
}

impl RetryPolicy {
    /// Delay to wait after `failed_attempt` (1-based) failed.
    #[must_use]
    pub fn backoff(&self, failed_attempt: u32) -> Duration {
        let exp = failed_attempt.saturating_sub(1);
        let factor = u64::from(self.multiplier).saturating_pow(exp);
        let ms = self
            .initial_backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms);
        Duration::from_millis(ms)
    }
}

/// What the executor wants run next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepSpec {
    /// Step name (e.g. tool name).
    pub name: String,
    /// Step input, persisted as the intent before execution.
    pub input: Value,
    /// Safe to replay / retry.  Side-effecting steps must leave this `false`.
    pub idempotent: bool,
    /// Retry policy (only honoured when `idempotent`).
    pub retry: Option<RetryPolicy>,
}

impl StepSpec {
    pub(crate) fn from_row(row: &RunStep) -> Self {
        Self {
            name: row.name.clone(),
            input: row.input.clone(),
            idempotent: row.idempotent,
            retry: row
                .retry
                .as_ref()
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
        }
    }
}

/// Error returned by a [`StepExecutor`].
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ExecError(pub String);

/// Implemented by the agent loop (and by fakes in tests).
#[async_trait]
pub trait StepExecutor: Send + Sync {
    /// Decide the next step given the persisted history, or `None` when the
    /// run is complete.
    ///
    /// # Errors
    /// An error fails the run.
    async fn next_step(
        &self,
        run: &Run,
        history: &[RunStep],
    ) -> Result<Option<StepSpec>, ExecError>;

    /// Execute one step.  Should return promptly once `cancel` fires.
    ///
    /// # Errors
    /// An error fails the step (and the run, unless retried).
    async fn execute(
        &self,
        run: &Run,
        step: &StepSpec,
        cancel: &CancellationToken,
    ) -> Result<Value, ExecError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy {
            max_attempts: 5,
            initial_backoff_ms: 100,
            multiplier: 2,
            max_backoff_ms: 350,
        };
        let ms = |a| p.backoff(a).as_millis();
        assert_eq!((ms(1), ms(2), ms(3), ms(4)), (100, 200, 350, 350));
        assert_eq!(ms(200), 350, "no overflow");
    }
}
