//! Permission decisions, interactive approvals, user questions and the
//! [`ToolGate`] handed to `script_run`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate, raw_command_for};
use parking_lot::Mutex;
use permission_engine::{ActionRequest, Decision, Policy, egress_must_ask};
use protocol::event::{ApprovalNeeded, QuestionAsked, ToolCallApproved, ToolCallDenied};
use protocol::{BotId, Event, InitiatorKind, PermissionCategory, RunId, ToolCallId};
use serde_json::{Value, json};
use storage::{Approval, now_ms};
use task_runtime::{RunState, can_transition};
use tokio::sync::oneshot;
use tool_registry::{ContentClass, Effect};
use uuid::Uuid;

use crate::AUDIT_GRANT_ALWAYS;
use crate::history::truncate_utf8;
use crate::shared::{ApprovalScope, Grant, GrantTarget, Pending, RunCtx, Shared, Verdict};
use crate::taint;

/// The `approval_ref` token a tool put in its details, if any.
fn approval_ref(details: &Value) -> Option<String> {
    details
        .get("approval_ref")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Tells the tool when an approval it prepared ends without the call running
/// (drop = denied, cancelled, timed out, or an approved permit that never
/// executed); disarmed when the call is committed to run.
struct ClosedGuard {
    tool: Option<Arc<dyn tool_registry::Tool>>,
    ctx: tool_registry::ToolContext,
    input: Value,
    token: Option<String>,
}

impl ClosedGuard {
    fn disarm(&mut self) {
        self.tool = None;
    }
}

impl Drop for ClosedGuard {
    fn drop(&mut self) {
        if let Some(t) = self.tool.take() {
            t.approval_closed(&self.ctx, &self.input, self.token.as_deref());
        }
    }
}

struct PendingGuard {
    sh: Arc<Shared>,
    id: ToolCallId,
    run_id: RunId,
    released: bool,
}

impl PendingGuard {
    /// This park is over: the run goes back to `Running` only when it was the
    /// last one parked (parallel `ask_user` calls share one run).
    fn resume(&mut self) {
        if self.release() {
            self.sh.set_state(self.run_id, RunState::Running);
        }
    }

    /// Decrement the run's park counter once; `true` when it reached zero.
    fn release(&mut self) -> bool {
        if std::mem::replace(&mut self.released, true) {
            return false;
        }
        let mut parked = self.sh.parked.lock();
        match parked.get_mut(&self.run_id) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            _ => {
                parked.remove(&self.run_id);
                true
            }
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.sh.pending.lock().remove(&self.id);
        forget_parked(&self.sh, self.id);
        // A dropped (cancelled) wait only releases its slot; the run's own
        // state is settled by whoever cancelled it.
        self.release();
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
        if self.locked_deny(category) {
            return Decision::Deny;
        }
        let mut d = r.resolve(
            &self.global_policy.read(),
            ctx.bot_policy.as_ref(),
            None,
            &req,
        );
        let class = self
            .tools
            .get(name)
            .map_or_else(ContentClass::unspecified, |t| t.spec().content);
        let exposure = ctx.taint.lock().exposure;
        // `web_search` can only reach the configured search provider. Every
        // other egress tool, including fetch, can be aimed at an arbitrary host.
        let must_ask = egress_must_ask(
            exposure,
            class.egress,
            name == "web_search",
            ctx.profile.permissions.allow_egress_after_reads,
        );
        // A tool that delegates future authority is confirmed every time:
        // an allow becomes a plain ask, and no grant waives it (a deny stays).
        if self.always_ask(name) {
            if d == Decision::Allow {
                d = Decision::ask();
            }
            return d;
        }
        if !d.is_ask() {
            // A clean Allow becomes an ask once the run is tainted and this
            // call can leave the machine. A Deny is unchanged.
            if d == Decision::Allow && must_ask {
                return Decision::ask();
            }
            return d;
        }
        // An `Ask` forced by an absolute floor survives even a full policy.
        // Unevaluated `AllowWithLimits` is one of those floors: the constraints
        // ride on the decision, and read shortcuts / grants must not waive it.
        // Money and the never-list are the others (they stay `Ask` under
        // [`Policy::full`]). Only a plain policy-level `Ask` can be waived.
        if d.unsupported_limits().is_some() {
            return d;
        }
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if floor {
            return d;
        }
        // The egress gate is not a grant and not the read shortcut. The only
        // way past it is the bot's explicit `allow_egress_after_reads` flag,
        // which already made `must_ask` false.
        if must_ask {
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

    /// The registered tool `name` must be confirmed on every call
    /// ([`tool_registry::Tool::always_ask`]).
    pub(crate) fn always_ask(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|t| t.always_ask())
    }

    /// Egress card for a tainted run. `None` when the run is clean or the tool
    /// cannot send data off the machine. Attached even when the ask came from
    /// `always_ask` or a floor, so the card still names the read and the host.
    fn egress_card(&self, ctx: &RunCtx, name: &str, input: &Value) -> Option<Value> {
        let class = self
            .tools
            .get(name)
            .map_or_else(ContentClass::unspecified, |t| t.spec().content);
        if !class.egress {
            return None;
        }
        let taint = ctx.taint.lock();
        taint::egress_details(&taint, name, input)
    }

    fn closed_guard(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        token: Option<String>,
    ) -> ClosedGuard {
        ClosedGuard {
            token,
            tool: self.tools.get(name),
            ctx: tool_registry::ToolContext {
                run_id: ctx.run_id.to_string(),
                bot_id: Some(ctx.bot_id.to_string()),
                ..tool_registry::ToolContext::default()
            },
            input: input.clone(),
        }
    }

    /// What the tool wants the approval card to show for this call, plus the
    /// bot's permission picture (it applies to every future firing).
    ///
    /// # Errors
    /// The tool rejects the call; it must not be offered for approval.
    pub(crate) async fn approval_details(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
    ) -> Result<Option<Value>, String> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(None);
        };
        let tctx = tool_registry::ToolContext {
            run_id: ctx.run_id.to_string(),
            bot_id: Some(ctx.bot_id.to_string()),
            ..tool_registry::ToolContext::default()
        };
        let details = tool
            .approval_details(&tctx, input)
            .await
            .map_err(|e| e.to_string())?;
        // The card is redacted on its way to the user while the tool stores
        // the raw text: if redaction would change anything the user would
        // approve something other than what is stored, so refuse.
        // Checked per string leaf with the very function the event path uses
        // (a serialized-JSON comparison would miss secrets with newlines or quotes).
        if let (Some(d), Some(r)) = (&details, &self.cfg.redactor) {
            let mut shown = d.clone();
            if crate::shared::redact_value(r, &mut shown) {
                tool.approval_closed(&tctx, input, approval_ref(d).as_deref());
                return Err(
                    "the request contains text that looks like a secret (a key or token), which cannot be shown for approval; remove it and ask again"
                        .into(),
                );
            }
        }
        Ok(details.map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.insert("permission".into(), self.scheduled_permission(ctx));
            }
            v
        }))
    }

    /// What a scheduled firing of this run's bot may do: the policy level per
    /// category for initiator `Schedule`, with category grants applied.
    fn scheduled_permission(&self, ctx: &RunCtx) -> Value {
        use PermissionCategory as C;
        let (mut allow, mut ask, mut deny) = (Vec::new(), Vec::new(), Vec::new());
        let cats = [
            C::Fs,
            C::Shell,
            C::Desktop,
            C::Browser,
            C::Accounts,
            C::Messaging,
            C::Money,
            C::System,
            C::Remote,
            C::Script,
        ];
        for c in cats {
            let req = ActionRequest {
                category: c,
                raw_command: String::new(),
                initiator: InitiatorKind::Schedule,
            };
            let mut d = if self.locked_deny(c) {
                Decision::Deny
            } else {
                self.resolver.resolve(
                    &self.global_policy.read(),
                    ctx.bot_policy.as_ref(),
                    None,
                    &req,
                )
            };
            let floor = self.resolver.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
            if d.is_ask()
                && d.unsupported_limits().is_none()
                && !floor
                && self
                    .grants
                    .lock()
                    .always
                    .iter()
                    .any(|g| g.bot_id == ctx.bot_id && g.target == GrantTarget::Category(c))
            {
                d = Decision::Allow;
            }
            let name = serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            match d {
                Decision::Allow => allow.push(name),
                Decision::Deny => deny.push(name),
                Decision::Ask { .. } => ask.push(name),
            }
        }
        let level = if deny.len() == cats.len() {
            "read_only"
        } else if ask.is_empty() && deny.is_empty() {
            "full"
        } else if allow.is_empty() {
            "ask_first"
        } else {
            "mixed"
        };
        json!({ "level": level, "allow": allow, "ask": ask, "deny": deny })
    }

    /// Locked deny-all mode applies to `category`.
    fn locked_deny(&self, category: PermissionCategory) -> bool {
        category != PermissionCategory::Meta
            && self.policy_locked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Decision for a human-initiated call of `category` under a bot's policy
    /// plus its category grants; the flag says a grant made the difference.
    pub(crate) fn effective_category(
        &self,
        bot_id: BotId,
        bot_policy: Option<&Policy>,
        category: PermissionCategory,
    ) -> (Decision, bool) {
        let req = ActionRequest {
            category,
            raw_command: String::new(),
            initiator: InitiatorKind::Human,
        };
        let r = &self.resolver;
        if self.locked_deny(category) {
            return (Decision::Deny, false);
        }
        let d = r.resolve(&self.global_policy.read(), bot_policy, None, &req);
        // Same rule as `decide`: only a plain policy-level `Ask` can be waived.
        // An unsupported-limit ask is not waivable, grant or not.
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if d.is_ask()
            && d.unsupported_limits().is_none()
            && !floor
            && self
                .grants
                .lock()
                .always
                .iter()
                .any(|g| g.bot_id == bot_id && g.target == GrantTarget::Category(category))
        {
            return (Decision::Allow, true);
        }
        (d, false)
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
        once_only: bool,
    ) -> (Uuid, PendingGuard, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .insert(call_id, Pending { tx, once_only });
        *self.parked.lock().entry(ctx.run_id).or_insert(0) += 1;
        let guard = PendingGuard {
            sh: Arc::clone(self),
            id: call_id,
            run_id: ctx.run_id,
            released: false,
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

    /// Wait for the user's verdict.  A schedule-started run only waits
    /// [`LoopConfig::schedule_approval_timeout`](crate::LoopConfig); then the
    /// run is asked to pause (resumable) and `None` is returned.
    async fn wait_verdict(
        &self,
        ctx: &RunCtx,
        rx: oneshot::Receiver<Verdict>,
    ) -> Option<Result<Verdict, oneshot::error::RecvError>> {
        if ctx.initiator != InitiatorKind::Schedule {
            return Some(rx.await);
        }
        let limit = self.cfg.schedule_approval_timeout;
        if let Ok(v) = tokio::time::timeout(limit, rx).await {
            return Some(v);
        }
        self.approval_paused.lock().insert(ctx.run_id);
        self.audit(
            ctx,
            "system",
            "approval.timed_out",
            &json!({ "timeout_secs": limit.as_secs() }),
        );
        if let Some(rt) = self.rt.get().and_then(Weak::upgrade) {
            rt.request_pause(ctx.run_id, true);
        }
        None
    }

    /// True when redaction would change the card or the input that would run.
    fn shown_text_would_change(&self, details: Option<&Value>, input: &Value) -> bool {
        let Some(r) = &self.cfg.redactor else {
            return false;
        };
        let details_changed = details.is_some_and(|d| {
            let mut shown = d.clone();
            crate::shared::redact_value(r, &mut shown)
        });
        let mut input_shown = input.clone();
        details_changed || crate::shared::redact_value(r, &mut input_shown)
    }

    /// Refuse, and tell the tool its preview is closed, when the user would
    /// approve different text from the text that runs.
    fn refuse_hidden_secret(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        details: Option<&Value>,
        token: Option<&str>,
    ) -> Result<(), String> {
        if !self.shown_text_would_change(details, input) {
            return Ok(());
        }
        if let Some(tool) = self.tools.get(name) {
            let tctx = tool_registry::ToolContext {
                run_id: ctx.run_id.to_string(),
                bot_id: Some(ctx.bot_id.to_string()),
                ..tool_registry::ToolContext::default()
            };
            tool.approval_closed(&tctx, input, token);
        }
        Err(
            "the request contains text that looks like a secret (a key or token), which cannot be shown for approval; remove it and ask again"
                .into(),
        )
    }

    /// Park the run in `WaitingApproval` until `approve` / `deny` is called.
    ///
    /// `Ok(permit)` means the user approved and the post-approve recheck did
    /// not see [`Decision::Deny`]. The caller must [`ExecPermit::commit`]
    /// immediately before invoking the tool: that recheck runs under
    /// [`Shared::exec_gate`] and is what creates a grant. `Err` means the
    /// call is denied (no grant, tool must not run).
    #[allow(clippy::too_many_arguments)] // `details` joined an already wide call
    pub(crate) async fn request_approval(
        self: &Arc<Self>,
        ctx: &Arc<RunCtx>,
        call_id: ToolCallId,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        mut details: Option<Value>,
    ) -> Result<ExecPermit, Option<String>> {
        // The tool's token for this approval: kept for the close guard, never shown.
        let token = details.as_ref().and_then(approval_ref);
        if let Some(o) = details.as_mut().and_then(Value::as_object_mut) {
            o.remove("approval_ref");
        }
        if let Some(extra) = self.egress_card(ctx, name, input) {
            taint::merge_egress(&mut details, extra);
        }
        // The tool's own card was checked before this merge. The egress
        // payload is part of the card the user sees and part of what the
        // permit will run.
        self.refuse_hidden_secret(ctx, name, input, details.as_ref(), token.as_deref())
            .map_err(Some)?;
        // An always-ask tool must show the user what it does: refuse to ask
        // without details rather than fall back to the generic card.
        if self.always_ask(name) && details.is_none() {
            return Err(Some(
                "internal error: this tool needs approval details to be asked about; it was not run"
                    .into(),
            ));
        }
        let once_only = details.is_some() || self.always_ask(name);
        let closed = self.closed_guard(ctx, name, input, token);
        let args = input.to_string();
        let summary = self.redact(format!("{name} {}", truncate_utf8(&args, 300)));
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary, once_only);
        let parked_at = policy_revision(self);
        self.remember_parked(call_id, ctx, name, effect, category, input);
        self.emit(
            ctx,
            Event::ApprovalNeeded(ApprovalNeeded {
                run_id: ctx.run_id,
                call_id,
                tool_name: name.to_owned(),
                input: input.clone(),
                summary,
                details,
            }),
        );
        let verdict = self.wait_verdict(ctx, rx).await;
        guard.resume();
        let Some(verdict) = verdict else {
            let _ = self.storage.decide_approval(approval_id, "expired");
            return Err(Some("nobody approved in time; the run is paused".into()));
        };

        match verdict {
            Ok(Verdict::Approved { scope, by }) => {
                // The policy may have tightened while this card was open.
                // A hard deny wins over the click: do not run the tool and do
                // not mint a grant from a decision the current policy rejects.
                // The grant itself is deferred to [`ExecPermit::commit`], which
                // repeats this check under the same mutex immediately before
                // the tool is invoked.
                {
                    let _gate = self.exec_gate.lock();
                    if matches!(
                        self.decide(ctx, name, effect, category, input),
                        Decision::Deny
                    ) {
                        self.record_policy_denial(ctx, approval_id, call_id, name, by);
                        return Err(Some("denied by policy".into()));
                    }
                }
                Ok(ExecPermit {
                    closed,
                    sh: Arc::clone(self),
                    ctx: Arc::clone(ctx),
                    approval_id,
                    call_id,
                    name: name.to_owned(),
                    effect,
                    category,
                    input: input.clone(),
                    scope,
                    by,
                    parked_at,
                })
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

    fn remember_parked(
        &self,
        call_id: ToolCallId,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
    ) {
        note_parked(
            self,
            call_id,
            ParkedCall {
                name: name.to_owned(),
                effect,
                category,
                input: input.clone(),
                run_id: ctx.run_id,
            },
        );
    }

    /// The user approved, but the current policy now hard-denies the call.
    fn record_policy_denial(
        &self,
        ctx: &RunCtx,
        approval_id: Uuid,
        call_id: ToolCallId,
        name: &str,
        by: protocol::UserId,
    ) {
        let _ = self.storage.decide_approval(approval_id, "denied");
        self.audit(
            ctx,
            "user",
            "approval.denied",
            &json!({ "tool": name, "reason": "denied by policy" }),
        );
        self.emit(
            ctx,
            Event::ToolCallDenied(ToolCallDenied {
                run_id: ctx.run_id,
                call_id,
                denied_by: by,
                reason: Some("denied by policy".into()),
            }),
        );
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
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary, false);
        self.emit(
            ctx,
            Event::QuestionAsked(QuestionAsked {
                run_id: ctx.run_id,
                call_id,
                question,
                options,
            }),
        );
        let verdict = self.wait_verdict(ctx, rx).await;
        guard.resume();
        let Some(verdict) = verdict else {
            let _ = self.storage.decide_approval(approval_id, "expired");
            return Err(Some("nobody answered in time; the run is paused".into()));
        };
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

/// An approval the user accepted, not yet committed to execution.
///
/// [`ExecPermit::commit`] re-checks [`Decision::Deny`] under [`Shared::exec_gate`]
/// and only then records the approval and any grant. Dropping the permit
/// without committing runs nothing and keeps no grant.
pub(crate) struct ExecPermit {
    /// Armed until the call is committed to run: dropping the permit (or a
    /// failed commit) tells the tool this approval ended without executing.
    closed: ClosedGuard,
    sh: Arc<Shared>,
    ctx: Arc<RunCtx>,
    approval_id: Uuid,
    call_id: ToolCallId,
    name: String,
    effect: Effect,
    category: PermissionCategory,
    input: Value,
    scope: ApprovalScope,
    by: protocol::UserId,
    parked_at: u64,
}

impl ExecPermit {
    /// Last check before the tool runs. Holds `exec_gate` so
    /// [`install_global_policy`] cannot change the policy between this
    /// `decide` and the caller's decision to invoke the tool.
    ///
    /// `Err` — do not call the tool; no grant is stored.
    pub(crate) fn commit(mut self) -> Result<(), Option<String>> {
        let _gate = self.sh.exec_gate.lock();
        #[cfg(test)]
        {
            let hook = self.sh.exec_commit_hook.lock().clone();
            if let Some(hook) = hook {
                hook(&self.sh);
            }
        }
        if matches!(
            self.sh.decide(
                &self.ctx,
                &self.name,
                self.effect,
                self.category,
                &self.input
            ),
            Decision::Deny
        ) {
            self.sh.record_policy_denial(
                &self.ctx,
                self.approval_id,
                self.call_id,
                &self.name,
                self.by,
            );
            return Err(Some("denied by policy".into()));
        }
        let _ = self
            .sh
            .storage
            .decide_approval(self.approval_id, "approved");
        // Approving an always-ask tool confirms this one call only.
        let scope = if self.sh.always_ask(&self.name) {
            ApprovalScope::Once
        } else {
            self.scope
        };
        match scope {
            ApprovalScope::Once => {}
            ApprovalScope::ThisChat => {
                self.sh
                    .grants
                    .lock()
                    .this_chat
                    .insert((self.ctx.chat_id, self.name.clone()));
            }
            ApprovalScope::Always => {
                self.sh
                    .add_grant(&self.ctx, GrantTarget::Tool(self.name.clone()));
            }
            ApprovalScope::AlwaysCategory => {
                self.sh
                    .add_grant(&self.ctx, GrantTarget::Category(self.category));
            }
        }
        self.sh.audit(
            &self.ctx,
            "user",
            "approval.approved",
            &json!({
                "tool": self.name,
                "scope": format!("{scope:?}"),
                "policy_revision": policy_revision(&self.sh),
                "parked_at": self.parked_at,
            }),
        );
        self.sh.emit(
            &self.ctx,
            Event::ToolCallApproved(ToolCallApproved {
                run_id: self.ctx.run_id,
                call_id: self.call_id,
                approved_by: self.by,
            }),
        );
        self.closed.disarm();
        Ok(())
    }
}

/// One open approval. Re-checked against the live policy when the revision moves.
#[derive(Clone)]
struct ParkedCall {
    name: String,
    effect: Effect,
    category: PermissionCategory,
    input: Value,
    run_id: RunId,
}

/// Policy generation for one agent loop, plus the approvals parked against it.
///
/// Lives beside [`Shared`] rather than inside it: `shared.rs` is owned by
/// another change. The map is keyed by the `Shared` allocation address, which
/// is stable for the life of the `Arc`.
#[derive(Default)]
struct PolicySlot {
    revision: u64,
    parked: HashMap<ToolCallId, ParkedCall>,
}

fn policy_slots() -> &'static Mutex<HashMap<usize, PolicySlot>> {
    static SLOTS: OnceLock<Mutex<HashMap<usize, PolicySlot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn slot_key(sh: &Shared) -> usize {
    std::ptr::from_ref(sh) as usize
}

fn policy_revision(sh: &Shared) -> u64 {
    policy_slots()
        .lock()
        .get(&slot_key(sh))
        .map_or(0, |slot| slot.revision)
}

fn note_parked(sh: &Shared, id: ToolCallId, call: ParkedCall) {
    let mut slots = policy_slots().lock();
    let slot = slots.entry(slot_key(sh)).or_default();
    slot.parked.insert(id, call);
}

fn forget_parked(sh: &Shared, id: ToolCallId) {
    if let Some(slot) = policy_slots().lock().get_mut(&slot_key(sh)) {
        slot.parked.remove(&id);
    }
}

/// Install `policy` and deny parked approvals it hard-denies.
///
/// Caller holds [`Shared::exec_gate`] for this whole update (the write, the
/// revision bump, and the pending denial) so it cannot interleave with
/// [`ExecPermit::commit`].
pub(crate) fn install_global_policy(sh: &Shared, policy: Policy) {
    *sh.global_policy.write() = policy;
    on_global_policy_replaced(sh);
}

/// Bump the policy revision and deny parked approvals the new policy rejects.
///
/// Called from [`install_global_policy`] after the new policy is installed.
/// A pending card whose category is now [`Decision::Deny`] is failed closed:
/// the tool does not run and no grant is created.
pub(crate) fn on_global_policy_replaced(sh: &Shared) {
    let stale: Vec<(ToolCallId, ParkedCall)> = {
        let mut slots = policy_slots().lock();
        let slot = slots.entry(slot_key(sh)).or_default();
        slot.revision = slot.revision.wrapping_add(1);
        slot.parked
            .iter()
            .map(|(id, call)| (*id, call.clone()))
            .collect()
    };
    let mut drop_ids = Vec::new();
    for (id, call) in stale {
        let ctx = sh.runs.lock().get(&call.run_id).cloned();
        let Some(ctx) = ctx else {
            drop_ids.push(id);
            continue;
        };
        if !matches!(
            sh.decide(&ctx, &call.name, call.effect, call.category, &call.input),
            Decision::Deny
        ) {
            continue;
        }
        drop_ids.push(id);
        if let Some(pending) = sh.pending.lock().remove(&id) {
            let _ = pending.tx.send(Verdict::Denied {
                by: ctx.user_id,
                reason: Some("denied by policy".into()),
            });
        }
    }
    if drop_ids.is_empty() {
        return;
    }
    if let Some(slot) = policy_slots().lock().get_mut(&slot_key(sh)) {
        for id in drop_ids {
            slot.parked.remove(&id);
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
            Decision::Allow => {
                note_gate(&ctx, req);
                Ok(())
            }
            Decision::Deny => Err(format!(
                "permission denied: {} is not allowed by policy",
                req.tool
            )),
            Decision::Ask { .. } => {
                let details = sh
                    .approval_details(&ctx, &req.tool, &req.input)
                    .await
                    .map_err(|e| format!("{}: {e}", req.tool))?;
                sh.request_approval(
                    &ctx,
                    ToolCallId::new(),
                    &req.tool,
                    req.effect,
                    req.category,
                    &req.input,
                    details,
                )
                .await
                .and_then(ExecPermit::commit)
                .map(|()| note_gate(&ctx, req))
                .map_err(|reason| {
                    format!(
                        "permission denied by the user{}",
                        reason.map_or_else(String::new, |r| format!(": {r}"))
                    )
                })
            }
        }
    }
}

/// A script or workflow call that was allowed has put its result in front of
/// the model by the time the next call is authorized. Record that now so the
/// next inner call sees the taint. A denial records nothing.
fn note_gate(ctx: &RunCtx, req: &GateRequest) {
    ctx.taint
        .lock()
        .note(req.content, &taint::source_label(&req.tool, &req.input));
}
