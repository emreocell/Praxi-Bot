//! `wait`: pause the run for a few seconds, then carry on.
//!
//! Short bursts ("every 5 seconds, 4 times") do not belong in a schedule: the
//! scheduler has a minimum interval. The bot does them inside ONE run: repeat
//! the action and call `wait` between the rounds. Bounds: 1..=300 s per call, a
//! cumulative cap per run, and Stop ends a wait at once (the loop calls
//! [`cancel_run_waits`] when the run is cancelled). Each call is an ordinary
//! step, so it counts toward the run's step and spend limits.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{parse, spec};

/// Longest single wait.
pub const MAX_WAIT_SECS: u64 = 300;
/// Most seconds one run may spend waiting in total (30 minutes).
pub const MAX_RUN_WAIT_SECS: u64 = 1800;
/// Runs remembered at once; the oldest idle entries are dropped beyond this.
const MAX_TRACKED_RUNS: usize = 512;

#[derive(Default)]
struct RunWait {
    used_secs: u64,
    token: CancellationToken,
}

static RUNS: LazyLock<Mutex<HashMap<String, RunWait>>> = LazyLock::new(Mutex::default);

/// End every wait of `run_id` now and refuse further ones. Called when the run
/// is stopped.
pub fn cancel_run_waits(run_id: &str) {
    let mut runs = RUNS.lock();
    runs.entry(run_id.to_owned()).or_default().token.cancel();
    drop(runs);
    // The owner stopped the run: nothing is kept for the bot's next run to pick up.
    crate::mail_wait::cancel_run_mail_waits(run_id, true);
}

/// Drop the background mail waits of `run_id`: the run is over (finished, failed, stopped or
/// paused), so nobody can collect them and their polling must not go on.
///
/// What a wait wrote to the data directory stays, so the bot's next run (the next mission
/// session, a run after a restart) can take it over; only an owner Stop
/// ([`cancel_run_waits`]) forgets it.
pub fn end_run_mail_waits(run_id: &str) {
    crate::mail_wait::cancel_run_mail_waits(run_id, false);
}

#[derive(Deserialize)]
struct WaitIn {
    seconds: u64,
}

/// The `wait` tool. `unit` is one "second" (tests shrink it).
pub struct WaitTool {
    spec: ToolSpec,
    unit: Duration,
}

/// Build `wait`.
#[must_use]
pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(WaitTool::new(Duration::from_secs(1)))]
}

impl WaitTool {
    fn new(unit: Duration) -> Self {
        Self {
            spec: spec(
                "wait",
                "Pause this run for `seconds` (1-300), then continue. Use it between the rounds of a short burst instead of a schedule: repeat the action, call `wait`, repeat. Total waiting per run is capped at 30 minutes, and Stop ends a wait at once. Example: {\"seconds\":5}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "seconds":{"type":"integer","minimum":1,"maximum":MAX_WAIT_SECS,"description":"How long to wait, in seconds."}
                },"required":["seconds"]}),
                Effect::Read,
                PermissionCategory::Meta,
            ),
            unit,
        }
    }

    /// Reserve `secs` of the run's budget; returns the run's token.
    fn reserve(run_id: &str, secs: u64) -> Result<CancellationToken, ToolError> {
        reserve_run_secs(run_id, secs)
    }
}

/// Reserve `secs` of the run's waiting budget (shared by `wait` and
/// `email_wait_for`) and return the token that ends every wait of the run when
/// it is stopped.
///
/// # Errors
/// The run was stopped, or its budget is used up.
pub(crate) fn reserve_run_secs(run_id: &str, secs: u64) -> Result<CancellationToken, ToolError> {
    let mut runs = RUNS.lock();
    if runs.len() >= MAX_TRACKED_RUNS && !runs.contains_key(run_id) {
        let idle: Vec<String> = runs.keys().take(MAX_TRACKED_RUNS / 4).cloned().collect();
        for k in idle {
            runs.remove(&k);
        }
    }
    let w = runs.entry(run_id.to_owned()).or_default();
    if w.token.is_cancelled() {
        return Err(ToolError::Execution("the run was stopped".into()));
    }
    if w.used_secs + secs > MAX_RUN_WAIT_SECS {
        return Err(ToolError::InvalidInput(format!(
            "this run already waited {} s of the {} s allowed; finish up or report what is done so far",
            w.used_secs, MAX_RUN_WAIT_SECS
        )));
    }
    w.used_secs += secs;
    Ok(w.token.clone())
}

/// Give back reserved seconds that were not used (a wait that ended early).
pub(crate) fn refund_run_secs(run_id: &str, secs: u64) {
    if let Some(w) = RUNS.lock().get_mut(run_id) {
        w.used_secs = w.used_secs.saturating_sub(secs);
    }
}

#[async_trait]
impl Tool for WaitTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: WaitIn = parse(input)?;
        if !(1..=MAX_WAIT_SECS).contains(&i.seconds) {
            return Err(ToolError::InvalidInput(format!(
                "`seconds` must be between 1 and {MAX_WAIT_SECS}"
            )));
        }
        let token = Self::reserve(&ctx.run_id, i.seconds)?;
        let pause = self
            .unit
            .saturating_mul(u32::try_from(i.seconds).unwrap_or(u32::MAX));
        tokio::select! {
            () = tokio::time::sleep(pause) => Ok(ToolOutput::text(format!("Waited {} s.", i.seconds))),
            () = token.cancelled() => Err(ToolError::Execution("wait cancelled: the run was stopped".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn ctx(run: &str) -> ToolContext {
        ToolContext {
            run_id: run.into(),
            ..ToolContext::default()
        }
    }

    #[tokio::test]
    async fn waits_and_rejects_out_of_range_seconds() {
        let t = WaitTool::new(Duration::from_millis(1));
        let out = t
            .call(&ctx("w-range"), json!({"seconds": 3}))
            .await
            .unwrap();
        assert!(format!("{out:?}").contains("Waited 3 s"));
        for bad in [0, 301] {
            let e = t
                .call(&ctx("w-range"), json!({"seconds": bad}))
                .await
                .unwrap_err();
            assert!(matches!(e, ToolError::InvalidInput(_)), "{bad}: {e:?}");
        }
    }

    #[tokio::test]
    async fn run_total_is_capped() {
        let t = WaitTool::new(Duration::from_micros(1));
        for _ in 0..6 {
            t.call(&ctx("w-cap"), json!({"seconds": 300}))
                .await
                .unwrap();
        }
        let e = t
            .call(&ctx("w-cap"), json!({"seconds": 1}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("1800"), "{e}");
        // another run has its own budget
        t.call(&ctx("w-cap-other"), json!({"seconds": 1}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stopping_the_run_interrupts_a_wait() {
        // 300 "seconds" of 100 ms = 30 s if nothing interrupts it.
        let t = Arc::new(WaitTool::new(Duration::from_millis(100)));
        let t2 = Arc::clone(&t);
        let started = Instant::now();
        let h = tokio::spawn(async move { t2.call(&ctx("w-stop"), json!({"seconds": 300})).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel_run_waits("w-stop");
        let e = h.await.unwrap().unwrap_err();
        assert!(e.to_string().contains("stopped"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // a stopped run cannot start another wait
        assert!(t.call(&ctx("w-stop"), json!({"seconds": 1})).await.is_err());
    }
}
