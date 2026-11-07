//! Permission decisions, interactive approvals, user questions and the
//! [`ToolGate`] handed to `script_run`.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate, raw_command_for};
use permission_engine::{ActionRequest, Decision, Policy};
use protocol::event::{ApprovalNeeded, QuestionAsked, ToolCallApproved, ToolCallDenied};
use protocol::{Event, PermissionCategory, RunId, ToolCallId};
use serde_json::{Value, json};
use storage::{Approval, now_ms};
use task_runtime::{RunState, can_transition};
use tokio::sync::oneshot;
use tool_registry::Effect;
use uuid::Uuid;

use crate::AUDIT_GRANT_ALWAYS;
use crate::history::truncate_utf8;
use crate::shared::{ApprovalScope, Grant, GrantTarget, Pending, RunCtx, Shared, Verdict};

struct PendingGuard {
    sh: Arc<Shared>,
    id: ToolCallId,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.sh.pending.lock().remove(&self.id);
    }
}

/// Serialised detail of a grant audit entry.
pub(crate) fn grant_detail(g: &Grant) -> Value {
    match &g.target {
        GrantTarget::Tool(t) => json!({ "bot_id": g.bot_id.to_string(), "tool": t }),
        GrantTarget::Category(c) => json!({ "bot_id": g.bot_id.to_string(), "category": c }),
    }
}

/// Inverse of [`grant_detail`].
pub(crate) fn grant_target(detail: &Value) -> Option<GrantTarget> {
    if let Some(t) = detail["tool"].as_str() {
        return Some(GrantTarget::Tool(t.to_owned()));
    }
    serde_json::from_value::<PermissionCategory>(detail["category"].clone())
        .ok()
        .map(GrantTarget::Category)
}

impl Shared {
    /// Resolve policy + grants for one call.
    pub(crate) fn decide(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
    ) -> Decision {
        let mut raw_command = raw_command_for(name, input);
        if raw_command.is_empty() {
            input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .clone_into(&mut raw_command);
        }
        let req = ActionRequest {
            category,
            raw_command,
            initiator: ctx.initiator,
        };
        let r = &self.resolver;
        let d = r.resolve(&self.cfg.global_policy, ctx.bot_policy.as_ref(), None, &req);
        if d != Decision::Ask {
            return d;
        }
        // An `Ask` forced by an absolute floor (money, never-list) survives
        // even a full policy; only a policy-level `Ask` can be waived.
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if floor {
            return d;
        }
        let granted = {
            let g = self.grants.lock();
            g.this_chat.contains(&(ctx.chat_id, name.to_owned()))
                || g.covers(ctx.bot_id, name, category)
        };
        if granted || (self.cfg.read_tools_skip_ask && effect == Effect::Read) {
            Decision::Allow
        } else {
            d
        }
    }

    pub(crate) fn set_state(&self, run_id: RunId, to: RunState) {
        if let Ok(Some(run)) = self.storage.get_run(run_id)
            && let Some(from) = RunState::parse(&run.state)
            && can_transition(from, to)
        {
            let _ = self.storage.update_run_state(run_id, to.as_str(), None);
        }
    }

    fn park(
        self: &Arc<Self>,
        ctx: &RunCtx,
        call_id: ToolCallId,
        summary: &str,
    ) -> (Uuid, PendingGuard, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(call_id, Pending { tx });
        let guard = PendingGuard {
            sh: Arc::clone(self),
            id: call_id,
        };
        let approval_id = Uuid::now_v7();
        let _ = self.storage.insert_approval(&Approval {
            id: approval_id,
            user_id: ctx.user_id,
            run_id: ctx.run_id,
            step_id: None,
            summary: summary.to_owned(),
            decision: None,
            requested_at_ms: now_ms(),
            decided_at_ms: None,
        });
        self.set_state(ctx.run_id, RunState::WaitingApproval);
        (approval_id, guard, rx)
    }

    /// Park the run in `WaitingApproval` until `approve` / `deny` is called.
    /// `Ok(())` = approved, `Err(reason)` = denied.
    pub(crate) async fn request_approval(
        self: &Arc<Self>,
        ctx: &RunCtx,
        call_id: ToolCallId,
        name: &str,
        category: PermissionCategory,
        input: &Value,
    ) -> Result<(), Option<String>> {
        let args = input.to_string();
        let summary = self.redact(format!("{name} {}", truncate_utf8(&args, 300)));
        let (approval_id, _guard, rx) = self.park(ctx, call_id, &summary);
        self.emit(
            ctx,
            Event::ApprovalNeeded(ApprovalNeeded {
                run_id: ctx.run_id,
                call_id,
                tool_name: name.to_owned(),
                input: input.clone(),
                summary,
            }),
        );
        let verdict = rx.await;
        self.set_state(ctx.run_id, RunState::Running);

        match verdict {
            Ok(Verdict::Approved { scope, by }) => {
                let _ = self.storage.decide_approval(approval_id, "approved");
                let target = match scope {
                    ApprovalScope::Once => None,
                    ApprovalScope::ThisChat => {
                        self.grants
                            .lock()
                            .this_chat
                            .insert((ctx.chat_id, name.to_owned()));
                        None
                    }
                    ApprovalScope::Always => Some(GrantTarget::Tool(name.to_owned())),
                    ApprovalScope::AlwaysCategory => Some(GrantTarget::Category(category)),
                };
                if let Some(target) = target {
                    self.add_grant(ctx, target);
                }
                self.audit(
                    ctx,
                    "user",
                    "approval.approved",
                    &json!({ "tool": name, "scope": format!("{scope:?}") }),
                );
                self.emit(
                    ctx,
                    Event::ToolCallApproved(ToolCallApproved {
                        run_id: ctx.run_id,
                        call_id,
                        approved_by: by,
                    }),
                );
                Ok(())
            }
            Ok(Verdict::Denied { by, reason }) => {
                let _ = self.storage.decide_approval(approval_id, "denied");
                self.audit(
                    ctx,
                    "user",
                    "approval.denied",
                    &json!({ "tool": name, "reason": reason }),
                );
                self.emit(
                    ctx,
                    Event::ToolCallDenied(ToolCallDenied {
                        run_id: ctx.run_id,
                        call_id,
                        denied_by: by,
                        reason: reason.clone(),
                    }),
                );
                Err(reason)
            }
            Ok(Verdict::Answered { .. }) | Err(_) => {
                Err(Some("approval request was dropped".into()))
            }
        }
    }

    /// Persist a new "always allow" rule for the run's bot (idempotent) and
    /// keep the audit entry.
    fn add_grant(&self, ctx: &RunCtx, target: GrantTarget) {
        let grant = Grant {
            bot_id: ctx.bot_id,
            target,
            granted_at_ms: now_ms(),
        };
        {
            let mut g = self.grants.lock();
            if g.always
                .iter()
                .any(|x| x.bot_id == grant.bot_id && x.target == grant.target)
            {
                return;
            }
            g.always.push(grant.clone());
        }
        self.audit(ctx, "user", AUDIT_GRANT_ALWAYS, &grant_detail(&grant));
    }

    /// Park the run until the user answers a question card.
    /// `Ok(answer)`, or `Err(reason)` if the user declined to answer.
    pub(crate) async fn request_answer(
        self: &Arc<Self>,
        ctx: &RunCtx,
        call_id: ToolCallId,
        question: &str,
        options: Vec<String>,
    ) -> Result<String, Option<String>> {
        let question = self.redact(question.to_owned());
        let summary = format!("question: {}", truncate_utf8(&question, 300));
        let (approval_id, _guard, rx) = self.park(ctx, call_id, &summary);
        self.emit(
            ctx,
            Event::QuestionAsked(QuestionAsked {
                run_id: ctx.run_id,
                call_id,
                question,
                options,
            }),
        );
        let verdict = rx.await;
        self.set_state(ctx.run_id, RunState::Running);
        match verdict {
            Ok(Verdict::Answered { answer, .. }) => {
                let _ = self.storage.decide_approval(approval_id, "answered");
                self.audit(ctx, "user", "question.answered", &json!({}));
                Ok(answer)
            }
            Ok(Verdict::Denied { reason, .. }) => {
                let _ = self.storage.decide_approval(approval_id, "declined");
                Err(reason)
            }
            Ok(Verdict::Approved { .. }) | Err(_) => {
                let _ = self.storage.decide_approval(approval_id, "declined");
                Err(Some("question was dropped".into()))
            }
        }
    }
}

/// [`ToolGate`] for `script_run`: the loop's resolver and approval flow, so an
/// inner call can ask the user interactively.
pub(crate) struct LoopGate {
    pub sh: Weak<Shared>,
}

#[async_trait]
impl ToolGate for LoopGate {
    async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
        let sh = self.sh.upgrade().ok_or("agent loop is gone")?;
        let run_id = req
            .run_id
            .parse::<Uuid>()
            .map(RunId)
            .map_err(|_| "script has no run context".to_owned())?;
        let ctx = sh
            .runs
            .lock()
            .get(&run_id)
            .cloned()
            .ok_or("script has no active run")?;
        if !ctx.profile.tool_visible(&req.tool) {
            return Err(format!("unknown tool: {}", req.tool));
        }
        match sh.decide(&ctx, &req.tool, req.effect, req.category, &req.input) {
            Decision::Allow => Ok(()),
            Decision::Deny => Err(format!(
                "permission denied: {} is not allowed by policy",
                req.tool
            )),
            Decision::Ask => sh
                .request_approval(&ctx, ToolCallId::new(), &req.tool, req.category, &req.input)
                .await
                .map_err(|reason| {
                    format!(
                        "permission denied by the user{}",
                        reason.map_or_else(String::new, |r| format!(": {r}"))
                    )
                }),
        }
    }
}
