//! Bounded deterministic checks. No model, run, credentials, or ambient JS APIs.
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use script_runtime::{Limits, ScriptHost, ScriptRuntime};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Precheck, Result, SchedulerError};

/// Total bytes returned by all tools, and maximum final result.
pub const MAX_BYTES: usize = 64 * 1024;
/// Deadline includes tool waits and JavaScript execution.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Fixed note; upstream errors/source bytes must never enter a run's metadata.
pub const FAILURE_NOTE: &str =
    "The precheck failed or timed out. This run continued so a possible change is not missed.";

/// Durable runtime state; independent error streaks cannot reset each other.
#[derive(Debug, Clone, Default)]
pub struct CheckState {
    /// Previous successful `HashChanged` observation.
    pub last_hash: Option<String>,
    /// Firings avoided without starting an agent run.
    pub skipped_by_precheck: u64,
    /// Consecutive precheck failures.
    pub precheck_errors: u32,
    /// Consecutive watcher/polling failures.
    pub trigger_errors: u32,
    /// Deduplication, pending events and rate accounting survive restart.
    pub watch: Option<crate::watch::WatchState>,
}

/// Successful deterministic decision.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Whether to create the agent run.
    pub run: bool,
    /// Optional external data, never a trusted user message.
    pub input: Option<String>,
    /// New successful hash observation, including the first baseline.
    pub hash: Option<String>,
}

fn failure() -> SchedulerError {
    SchedulerError::Execution("precheck failed".into())
}

struct BoundedHost {
    inner: Arc<dyn ScriptHost>,
    budget: parking_lot::Mutex<(usize, usize)>,
}

#[async_trait]
impl ScriptHost for BoundedHost {
    async fn call_tool(&self, name: &str, input: Value) -> std::result::Result<Value, String> {
        {
            let mut b = self.budget.lock();
            if b.0 >= 10 || serde_json::to_vec(&input).map_or(true, |v| v.len() > MAX_BYTES) {
                return Err("precheck budget exceeded".into());
            }
            b.0 += 1;
        }
        let value = self
            .inner
            .call_tool(name, input)
            .await
            .map_err(|_| "precheck read refused or failed")?;
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| "invalid precheck output")?
            .len();
        let mut b = self.budget.lock();
        b.1 = b.1.saturating_add(bytes);
        if b.1 > MAX_BYTES {
            return Err("precheck output budget exceeded".into());
        }
        Ok(value)
    }
}

/// Evaluate with the host's registered, noninteractive Read permission gate.
///
/// # Errors
/// Denied tool, malformed result, exhausted resource budget, or timeout.
pub async fn evaluate(
    precheck: &Precheck,
    previous: Option<&str>,
    host: Arc<dyn ScriptHost>,
) -> Result<Outcome> {
    evaluate_with_timeout(precheck, previous, host, TIMEOUT).await
}

async fn evaluate_with_timeout(
    precheck: &Precheck,
    previous: Option<&str>,
    host: Arc<dyn ScriptHost>,
    timeout: Duration,
) -> Result<Outcome> {
    precheck.validate()?;
    let host: Arc<dyn ScriptHost> = Arc::new(BoundedHost {
        inner: host,
        budget: parking_lot::Mutex::new((0, 0)),
    });
    match precheck {
        Precheck::HashChanged { tool, input } => {
            let output = tokio::time::timeout(timeout, host.call_tool(tool, input.clone()))
                .await
                .map_err(|_| failure())?
                .map_err(|_| failure())?;
            let bytes = serde_json::to_vec(&output)?;
            let hash = hex::encode(Sha256::digest(&bytes));
            Ok(Outcome {
                run: previous.is_some_and(|p| p != hash),
                input: Some(String::from_utf8(bytes).map_err(|_| failure())?),
                hash: Some(hash),
            })
        }
        Precheck::Script { code } => {
            // A precheck is short end to end: the same bound for the script's own compute and
            // for the whole run, tool calls included.
            let limits = Limits {
                cpu_timeout: timeout,
                total_timeout: timeout,
                max_tool_calls: 10,
                max_output_bytes: MAX_BYTES,
                ..Limits::default()
            };
            let code = code.clone();
            let handle = tokio::runtime::Handle::current();
            // QuickJS's own interrupt handler stops CPU loops on this blocking worker.
            let result = tokio::task::spawn_blocking(move || {
                handle.block_on(ScriptRuntime::run(&code, host, limits))
            })
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
            if result.truncated || result.tool_calls.iter().any(|c| c.error.is_some()) {
                return Err(failure());
            }
            match result.result {
                Value::Bool(run) => Ok(Outcome {
                    run,
                    input: None,
                    hash: None,
                }),
                Value::Object(mut object) => {
                    let run = object
                        .remove("run")
                        .and_then(|v| v.as_bool())
                        .ok_or_else(failure)?;
                    let input = match object.remove("input") {
                        None => None,
                        Some(Value::String(s)) => Some(s),
                        _ => return Err(failure()),
                    };
                    if !object.is_empty() {
                        return Err(failure());
                    }
                    Ok(Outcome {
                        run,
                        input,
                        hash: None,
                    })
                }
                _ => Err(failure()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Echo;
    #[async_trait]
    impl ScriptHost for Echo {
        async fn call_tool(&self, _: &str, v: Value) -> std::result::Result<Value, String> {
            Ok(v)
        }
    }
    #[tokio::test]
    async fn initial_hash_and_two_same_observations_skip_change_runs() {
        let host = Arc::new(Echo);
        let mut check = Precheck::HashChanged {
            tool: "read".into(),
            input: Value::String("one".into()),
        };
        let mut hash = None;
        for _ in 0..3 {
            let out = evaluate(&check, hash.as_deref(), host.clone())
                .await
                .unwrap();
            assert!(!out.run);
            hash = out.hash;
        }
        if let Precheck::HashChanged { input, .. } = &mut check {
            *input = Value::String("two".into());
        }
        assert!(evaluate(&check, hash.as_deref(), host).await.unwrap().run);
    }
    #[tokio::test]
    async fn sandbox_has_no_ambient_capabilities_and_accepts_only_decision() {
        let check = Precheck::Script { code: "return {run: typeof require !== 'function' && typeof fetch !== 'function' && typeof process === 'undefined', input: 'external'};".into() };
        let out = evaluate(&check, None, Arc::new(Echo)).await.unwrap();
        assert!(out.run);
        assert_eq!(out.input.as_deref(), Some("external"));
        for code in [
            "return 1;",
            "return {run:true, input:3};",
            "return {run:true, extra:1};",
        ] {
            assert!(
                evaluate(
                    &Precheck::Script { code: code.into() },
                    None,
                    Arc::new(Echo)
                )
                .await
                .is_err()
            );
        }
    }
    struct Large;
    #[async_trait]
    impl ScriptHost for Large {
        async fn call_tool(&self, _: &str, _: Value) -> std::result::Result<Value, String> {
            Ok(Value::String("x".repeat(MAX_BYTES)))
        }
    }
    #[tokio::test]
    async fn output_from_read_tool_is_bounded_before_hashing_or_javascript() {
        let hash = Precheck::HashChanged {
            tool: "read".into(),
            input: Value::Null,
        };
        assert!(evaluate(&hash, None, Arc::new(Large)).await.is_err());
        let code = Precheck::Script {
            code: "await tools.call('read',{}); return true;".into(),
        };
        assert!(evaluate(&code, None, Arc::new(Large)).await.is_err());
    }
    #[tokio::test]
    async fn busy_loop_timeout_and_tool_output_and_call_limits_are_enforced() {
        let busy = Precheck::Script {
            code: "while(true){}".into(),
        };
        assert!(
            evaluate_with_timeout(&busy, None, Arc::new(Echo), Duration::from_millis(20))
                .await
                .is_err()
        );
        let calls = Precheck::Script {
            code: "for(let i=0;i<11;i++){await tools.call('read',{});} return true;".into(),
        };
        assert!(evaluate(&calls, None, Arc::new(Echo)).await.is_err());
        let output = Precheck::Script {
            code: "await tools.call('read','x'.repeat(65536)); return true;".into(),
        };
        assert!(evaluate(&output, None, Arc::new(Echo)).await.is_err());
    }
}
