//! Permission decisions, interactive approvals, user questions and the
//! [`ToolGate`] handed to `script_run`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate, raw_command_for};
use parking_lot::Mutex;
use permission_engine::{ActionRequest, Decision, Policy};
use protocol::event::{ApprovalNeeded, QuestionAsked, ToolCallApproved, ToolCallDenied};
use protocol::{BotId, Event, InitiatorKind, PermissionCategory, RunId, ToolCallId};
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
        let d = r.resolve(
            &self.global_policy.read(),
            ctx.bot_policy.as_ref(),
            None,
            &req,
        );
        if !d.is_ask() {
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
    ) -> (Uuid, PendingGuard, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(call_id, Pending { tx });
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

    /// Park the run in `WaitingApproval` until `approve` / `deny` is called.
    ///
    /// `Ok(permit)` means the user approved and the post-approve recheck did
    /// not see [`Decision::Deny`]. The caller must [`ExecPermit::commit`]
    /// immediately before invoking the tool: that recheck runs under
    /// [`Shared::exec_gate`] and is what creates a grant. `Err` means the
    /// call is denied (no grant, tool must not run).
    pub(crate) async fn request_approval(
        self: &Arc<Self>,
        ctx: &Arc<RunCtx>,
        call_id: ToolCallId,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
    ) -> Result<ExecPermit, Option<String>> {
        let args = input.to_string();
        let summary = self.redact(format!("{name} {}", truncate_utf8(&args, 300)));
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary);
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
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary);
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
    pub(crate) fn commit(self) -> Result<(), Option<String>> {
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
        match self.scope {
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
                "scope": format!("{:?}", self.scope),
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
            Decision::Allow => Ok(()),
            Decision::Deny => Err(format!(
                "permission denied: {} is not allowed by policy",
                req.tool
            )),
            Decision::Ask { .. } => sh
                .request_approval(
                    &ctx,
                    ToolCallId::new(),
                    &req.tool,
                    req.effect,
                    req.category,
                    &req.input,
                )
                .await
                .and_then(ExecPermit::commit)
                .map_err(|reason| {
                    format!(
                        "permission denied by the user{}",
                        reason.map_or_else(String::new, |r| format!(": {r}"))
                    )
                }),
        }
    }
}
