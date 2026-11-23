//! Opt-in model tiering, with durable one-way escalation.
//!
//! Tier selection is deterministic and adds no model call. Any executed tool
//! output conservatively ends light inference. Prior chat and enabled memory
//! recall start at standard. Narrow these checks only using host-owned exposure
//! facts. Never classify trust from returned text or the model's own claims.

use agent_router::ladder::{Decision as RouteDecision, Route};
use permission_engine::Decision;
use protocol::event::ModelFallback;
use protocol::{Event, InitiatorKind};
use serde_json::{Value, json};
use storage::RunStep;
use task_runtime::ExecError;
use tool_registry::Effect;

use crate::exec::TurnExecutor;
use crate::shared::{RunCtx, STEP_MODEL, STEP_TOOLS, STEP_USER, xe};

pub(crate) const LIGHT: &str = "light";
pub(crate) const STANDARD: &str = "standard";

/// None preserves the complete old path, including serialized step/event shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TierPlan {
    pub tier: &'static str,
    pub upgrade: Option<&'static str>,
}

fn standard(upgrade: Option<&'static str>) -> TierPlan {
    TierPlan {
        tier: STANDARD,
        upgrade,
    }
}

/// A discarded light reply is never replayed as the assistant's decision. A
/// permission change between model completion and tool preparation can discard
/// the immediately preceding model step via its (still unexecuted) tool step.
pub(crate) fn discarded(steps: &[RunStep], index: usize) -> bool {
    let marked = |step: &RunStep| {
        step.output
            .as_ref()
            .is_some_and(|o| o["tier_discarded"].is_string())
    };
    marked(&steps[index])
        || (steps[index].name == STEP_MODEL
            && steps[index + 1..]
                .iter()
                .take_while(|s| s.name != STEP_MODEL)
                .any(marked))
}

pub(crate) fn discarded_tools(reason: &'static str) -> Value {
    json!({ "results": [], "tier_discarded": reason })
}

/// The authority of a completed response belongs to its persisted producer,
/// not the bot's current profile. A restart may load a different light model,
/// remove tiering, or disable it by adding a CLI fallback.
pub(crate) fn light_reply_model(step: &RunStep) -> Option<&str> {
    let output = step.output.as_ref()?;
    match output["tier"].as_str() {
        Some(LIGHT) => output["model"].as_str().filter(|model| !model.is_empty()),
        // Compatibility with earlier tiered rows: use the persisted planned
        // model, never today's configuration, and distinguish actual fallback.
        None if step.input["tier"] == LIGHT => {
            let planned = step.input["model"].as_str()?;
            output["model"]
                .as_str()
                .is_none_or(|actual| actual.is_empty() || actual == planned)
                .then_some(planned)
        }
        Some(_) | None => None,
    }
}

/// Also covers an interrupted light -> executor fallback whose model step has
/// no completed output yet. The planned light model was persisted before I/O.
pub(crate) fn light_origin(steps: &[RunStep]) -> Option<&str> {
    steps
        .iter()
        .filter(|step| step.name == STEP_MODEL)
        .find_map(|step| {
            light_reply_model(step).or_else(|| {
                (step.input["tier"] == LIGHT)
                    .then(|| step.input["model"].as_str())
                    .flatten()
            })
        })
}

impl TurnExecutor {
    /// CLI sessions and subagents keep their established model selection.
    pub(crate) fn light_model<'a>(&self, ctx: &'a RunCtx) -> Option<&'a str> {
        let light = ctx.profile.models.light.as_deref()?;
        if ctx.sub.is_some()
            || ctx.parent_run_id.is_some()
            || light == ctx.profile.models.executor
            || light.starts_with("cli:")
            || std::iter::once(&ctx.profile.models.executor)
                .chain(&ctx.profile.models.fallback)
                .any(|m| m.starts_with("cli:"))
            || (!ctx.profile.models.executor.contains('/')
                && self.sh.providers.read().default_id().starts_with("cli:"))
        {
            return None;
        }
        Some(light)
    }

    /// Select a tier deterministically after applying the exposure and
    /// initiator checks.
    pub(crate) fn initial_tier(
        &self,
        ctx: &RunCtx,
        text: &str,
        decision: Option<&RouteDecision>,
    ) -> Option<&'static str> {
        self.light_model(ctx)?;
        let clean_history = self
            .sh
            .storage
            .list_messages_bounded(ctx.chat_id, Some(ctx.run_id), 0, 0)
            .is_ok_and(|(_, any)| !any);
        if ctx.initiator != InitiatorKind::Human
            || !clean_history
            || self.sh.cfg.memory.as_ref().is_some_and(|m| m.enabled)
            || text.chars().count() > 200
        {
            return Some(STANDARD);
        }
        let read_hint = decision.is_some_and(|d| match &d.route {
            Route::ToolHint { tools } if !tools.is_empty() => tools.iter().all(|name| {
                self.sh.tools.get(name).is_some_and(|t| {
                    let spec = t.spec();
                    spec.effect == Effect::Read
                        && self.sh.spec_visible(&ctx.profile, ctx.bot_id, spec)
                })
            }),
            _ => false,
        });
        // The card's greeting acceptance case otherwise conflicts with its
        // ToolHint-only fallback. Keep this exception exact and deliberately
        // tiny: appended instructions never qualify as a greeting.
        let greeting = decision.is_none_or(|d| matches!(d.route, Route::Agent))
            && matches!(
                text.trim().to_lowercase().as_str(),
                "merhaba" | "selam" | "hello" | "hi"
            );
        Some(if read_hint || greeting {
            LIGHT
        } else {
            STANDARD
        })
    }

    /// Derive the current tier from persisted facts, so resume cannot downgrade.
    pub(crate) fn tier_plan(&self, ctx: &RunCtx, steps: &[RunStep]) -> Option<TierPlan> {
        let historical = light_origin(steps);
        let configured = self.light_model(ctx);
        let light = historical.or(configured)?;
        if steps.first().and_then(|s| s.input["tier"].as_str()) != Some(LIGHT) {
            return Some(standard(None));
        }
        if self.tier_was_upgraded(ctx).unwrap_or(true) {
            return Some(standard(None));
        }
        if historical.is_some() && configured != historical {
            return Some(standard(Some("profile_changed")));
        }
        // A completed fallback is standard too, even if its planned step was light.
        if steps.iter().any(|s| {
            s.name == STEP_MODEL
                && (s.input["tier"] == STANDARD
                    || s.output
                        .as_ref()
                        .and_then(|o| o["model"].as_str())
                        .is_some_and(|model| !model.is_empty() && model != light))
        }) {
            return Some(standard(None));
        }
        let mut invalid = 0;
        for step in steps {
            if let Some(reason) = step
                .output
                .as_ref()
                .and_then(|o| o["tier_discarded"].as_str())
            {
                return Some(standard(Some(match reason {
                    "approval_required" => "approval_required",
                    "profile_changed" => "profile_changed",
                    _ => "non_read_tool",
                })));
            }
            if step.name != STEP_TOOLS {
                continue;
            }
            if let Some(results) = step.output.as_ref().and_then(|o| o["results"].as_array()) {
                for result in results {
                    // Missing metadata is deliberately unsafe (old/resumed rows).
                    if result["tier_executed"] != false {
                        return Some(standard(Some("tool_output")));
                    }
                    invalid = if result["tier_invalid"] == true {
                        invalid + 1
                    } else {
                        0
                    };
                    if invalid >= 2 {
                        return Some(standard(Some("invalid_tool_calls")));
                    }
                }
            } else {
                // Interrupted execution has unknown exposure, so fail closed.
                return Some(standard(Some("tool_output")));
            }
            if step.input["calls"].as_array().is_some_and(|calls| {
                calls.iter().any(|c| {
                    c["repeat"]
                        .as_u64()
                        .is_some_and(|n| n >= u64::from(self.sh.cfg.guards.repeat_warn))
                })
            }) {
                return Some(standard(Some("repeat_warn")));
            }
        }
        if steps.iter().filter(|s| s.name == STEP_MODEL).count() >= 3 {
            return Some(standard(Some("fourth_model_step")));
        }
        if steps.iter().filter(|s| s.name == STEP_USER).count() > 1 {
            return Some(standard(Some("steering")));
        }
        Some(TierPlan {
            tier: LIGHT,
            upgrade: None,
        })
    }

    /// Check effective targets, including discovery's `use_tool` wrapper, before
    /// executing anything or constructing an approval card. Invalid calls are
    /// left to the existing host-side error path and counted separately.
    pub(crate) fn light_call_blocker<'a>(
        &self,
        ctx: &RunCtx,
        hints: &[String],
        offered: Option<&[String]>,
        calls: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Option<&'static str> {
        for (name, args) in calls {
            let Ok(input) = (if args.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str::<Value>(args)
            }) else {
                continue;
            };
            let Some(r) = self.sh.resolve_offered(ctx, name, &input, hints, offered) else {
                continue;
            };
            let Some(spec) = r.spec else {
                continue;
            };
            if spec.effect != Effect::Read {
                return Some("non_read_tool");
            }
            if matches!(
                self.sh
                    .decide(ctx, &r.name, spec.effect, spec.category, &r.input),
                Decision::Ask { .. }
            ) {
                return Some("approval_required");
            }
        }
        None
    }

    pub(crate) fn tier_was_upgraded(&self, ctx: &RunCtx) -> Result<bool, ExecError> {
        self.sh
            .storage
            .has_run_audit_action(ctx.user_id, ctx.run_id, "model.tier_escalated")
            .map_err(|_| xe("could not read the persisted model tier"))
    }

    /// A fallback may happen inside a model step. Commit the one-way latch
    /// before the executor request so an interrupted step cannot retry light.
    pub(crate) fn record_tier_upgrade(
        &self,
        ctx: &RunCtx,
        from: &str,
        reason: &str,
    ) -> Result<bool, ExecError> {
        if self.tier_was_upgraded(ctx)? {
            return Ok(false);
        }
        let detail = json!({
            "from": self.sh.redact(from.to_owned()),
            "to": self.sh.redact(ctx.profile.models.executor.clone()),
            "reason": self.sh.redact(reason.to_owned()),
        });
        self.sh
            .storage
            .append_audit(
                ctx.user_id,
                Some(ctx.run_id),
                "bot",
                "model.tier_escalated",
                &detail,
            )
            .map_err(|_| xe("could not persist model tier escalation"))?;
        Ok(true)
    }

    pub(crate) fn emit_tier_upgrade(
        &self,
        ctx: &RunCtx,
        from: &str,
        reason: &str,
    ) -> Result<(), ExecError> {
        if !self.record_tier_upgrade(ctx, from, reason)? {
            return Ok(());
        }
        self.sh.emit(
            ctx,
            Event::ModelFallback(ModelFallback {
                run_id: ctx.run_id,
                from: from.to_owned(),
                to: ctx.profile.models.executor.clone(),
                reason: format!("model tier escalation: {reason}"),
            }),
        );
        Ok(())
    }
}
