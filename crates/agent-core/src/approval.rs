//! Permission decisions, interactive approvals, user questions and the
//! [`ToolGate`] handed to `script_run`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate, cwd_for, raw_command_for};
use computer_target::ComputerLabel;
use parking_lot::Mutex;
use permission_engine::{
    ActionRequest, AllowReason, AutonomyMode, CallFacts, Decision, EgressReach, Impact, Policy,
    balanced_lane, egress_must_ask, executes_must_ask,
};
use protocol::event::{ApprovalNeeded, QuestionAsked, ToolCallApproved, ToolCallDenied};
use protocol::{BotId, Event, InitiatorKind, PermissionCategory, RunId, ToolCallId};
use serde_json::{Value, json};
use storage::{Approval, now_ms};
use task_runtime::{RunState, can_transition};
use tokio::sync::oneshot;
use tool_registry::{ContentClass, ControlTarget, Effect};
use uuid::Uuid;

use crate::AUDIT_GRANT_ALWAYS;
use crate::history::truncate_utf8;
use crate::scoped_grant::{self, Derived, Scope, ScopeKind};
use crate::shared::{
    ApprovalScope, ControlGrant, Grant, GrantTarget, Pending, RunCtx, RunToolGrant, Shared,
    Verdict, control_kind_of,
};
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

/// What a call's card says when nobody decided it in time (the model sees it as the denial reason).
pub(crate) const EXPIRED_REASON: &str = "expired: nobody answered in time";

/// How a wait for the user ended.
enum Waited {
    Verdict(Result<Verdict, oneshot::error::RecvError>),
    /// A schedule-started run: nobody answered, the run is asked to pause.
    Paused,
    /// Nobody answered within the approval timeout: the call is denied as expired.
    Expired,
}

struct PendingGuard {
    sh: Arc<Shared>,
    id: ToolCallId,
    run_id: RunId,
    /// The persisted approval row of this park (expired when the wait is abandoned).
    approval_id: Uuid,
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
        // A parked `secret_request` must not stay submittable once its wait is gone.
        self.sh.secret_requests.lock().remove(&self.id);
        forget_parked(&self.sh, self.id);
        let abandoned = !self.released;
        let last = self.release();
        if abandoned {
            // The wait was dropped without a verdict (the tool around it timed out, or the run
            // was stopped): the card is dead, so it must not stay pending and the run must not
            // stay in `waiting_approval`.
            self.sh
                .expire_abandoned(self.run_id, self.id, self.approval_id, last);
        }
    }
}

/// The full image path can be shown unchanged on every renderer.
fn path_fits_card(path: &str) -> bool {
    !path.trim().is_empty()
        && path.chars().count() <= 1024
        && !protocol::approval_text::hides_characters(path)
        && !path.chars().any(char::is_control)
}

/// What the card says about a run-scoped control grant: which program it
/// would cover, what it covers and what still asks. Daemon-authored.
fn attach_control_card(details: &mut Option<Value>, t: &ControlTarget, offered: bool) {
    let Some(obj) = details.as_mut().and_then(Value::as_object_mut) else {
        return;
    };
    obj.insert(
        "control_grant".into(),
        json!({
            "offered": offered,
            "refusal": t.refusal,
            "program": t.program_name(),
            "image_path": t.identity.image_path,
            "pid": t.identity.pid,
            "window_title": t.window_title,
            "covers": ["type", "set_value", "key"],
            // The program deny list is a heuristic by file name; the card says so.
            "heuristic": true,
        }),
    );
}

/// Serialised detail of a grant audit entry.
pub(crate) fn grant_detail(g: &Grant) -> Value {
    let mut detail = match &g.target {
        GrantTarget::Tool(t) => json!({ "bot_id": g.bot_id.to_string(), "tool": t }),
        GrantTarget::Category(c) => json!({ "bot_id": g.bot_id.to_string(), "category": c }),
        GrantTarget::Scoped { tool, scope } => json!({
            "bot_id": g.bot_id.to_string(),
            "tool": tool,
            "scope_kind": scope.kind.as_str(),
            "scope_pattern": scope.pattern,
        }),
    };
    detail["computer"] = json!(g.computer);
    detail
}

/// Inverse of [`grant_detail`].
pub(crate) fn grant_target(detail: &Value) -> Option<GrantTarget> {
    if let Some(kind) = detail["scope_kind"].as_str() {
        return Some(GrantTarget::Scoped {
            tool: detail["tool"].as_str()?.to_owned(),
            scope: Scope {
                kind: ScopeKind::parse(kind)?,
                pattern: detail["scope_pattern"].as_str()?.to_owned(),
            },
        });
    }
    if let Some(t) = detail["tool"].as_str() {
        return Some(GrantTarget::Tool(t.to_owned()));
    }
    serde_json::from_value::<PermissionCategory>(detail["category"].clone())
        .ok()
        .map(GrantTarget::Category)
}

/// The per-run read allowance a `pc_*` READ tool needs (`None`: not a read of
/// the user's computer that asks, e.g. `pc_info`).
pub(crate) fn pc_read_family(name: &str) -> Option<&'static str> {
    let inner = name.strip_prefix(builtin_tools::pc::PC_PREFIX)?;
    if inner.starts_with("fs_") {
        Some("files")
    } else if inner.starts_with("browser_") {
        Some("browser")
    } else if inner.starts_with("desktop_") {
        Some("screen")
    } else if inner.starts_with("shell_") {
        Some("commands")
    } else {
        None
    }
}

/// Add the immutable computer identity to both event details and the summary.
fn computer_approval(
    ctx: &RunCtx,
    name: &str,
    category: PermissionCategory,
    details: &mut Option<Value>,
) -> String {
    // The `pc_*` family names the user's own computer, not the one the run is on.
    let own = name.starts_with(builtin_tools::pc::PC_PREFIX);
    let node = if own {
        ctx.mypc.as_ref()
    } else {
        ctx.resolved_computer.node.as_ref()
    };
    let computer = ctx.tool_grant_scope(name, category).map(|_| {
        json!({
            "kind": if own { "your computer" } else { ctx.resolved_computer.label.as_str() },
            "node_id": node.map(|n| &n.id),
            "node_name": node.map_or("Local computer", |n| n.name.as_str()),
        })
    });
    let prefix = computer.as_ref().map_or(String::new(), |c| {
        format!(
            "[{}: {} ({})] ",
            c["kind"].as_str().unwrap_or_default(),
            c["node_name"].as_str().unwrap_or_default(),
            c["node_id"].as_str().unwrap_or("local")
        )
    });
    if let Some(computer) = computer {
        let detail = details.get_or_insert_with(|| json!({}));
        if let Some(obj) = detail.as_object_mut() {
            obj.insert("computer_target".into(), computer);
        }
    }
    prefix
}

/// Scheme, userinfo, host, port, path, query and fragment of a normalised URL.
type UrlParts = (
    String,
    String,
    String,
    Option<u16>,
    String,
    Option<String>,
    Option<String>,
);

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
        self.decide_with_target(ctx, name, effect, category, input, None)
    }

    /// [`Self::decide`] with the program a desktop input call would reach.
    ///
    /// A run-scoped control grant that covers this call (card 243-1) satisfies
    /// the egress and executor terms and a plain policy-level ask, exactly like
    /// a this-chat grant satisfies a plain ask, and nothing more: a deny,
    /// `always_ask`, unsupported limits and the absolute floors are all decided
    /// before the grant is looked at, in the same order as without it.
    pub(crate) fn decide_with_target(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        target: Option<&ControlTarget>,
    ) -> Decision {
        self.decide_with_reason(ctx, name, effect, category, input, target)
            .0
    }

    /// [`Self::decide_with_target`] plus the audit reason code when the
    /// autonomy mode (not the policy, a grant or the read shortcut) is what
    /// turned a plain ask into an allow.
    ///
    /// Source taint is monotonic for the run, including calls after a
    /// model-facing workflow handover: once set, an allow becomes an ask. Every
    /// caller goes through here so no path can skip it. Denies/floors win first.
    pub(crate) fn decide_with_reason(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        target: Option<&ControlTarget>,
    ) -> (Decision, Option<&'static str>) {
        // Taint is kept on the top-level run so subagents spawned after a
        // tainted replay inherit it and no per-child entry has to be cleaned up.
        let root = self.root_run_id(ctx);
        if let Some(root) = root
            && workflow_replay::replay::source_tainted()
        {
            self.router.tainted_runs.lock().insert(root);
        }
        let (decision, reason) =
            self.decide_with_reason_inner(ctx, name, effect, category, input, target);
        // An unreadable parent chain counts as tainted: ask, never allow.
        let tainted = root.is_none_or(|r| self.router.tainted_runs.lock().contains(&r));
        if decision == Decision::Allow && tainted {
            (Decision::ask(), None)
        } else {
            (decision, reason)
        }
    }

    /// The top-level run above `ctx`, or `None` when the parent chain cannot
    /// be read or is too deep.
    fn root_run_id(&self, ctx: &RunCtx) -> Option<RunId> {
        let mut run_id = ctx.run_id;
        let mut parent = ctx.parent_run_id;
        for _ in 0..8 {
            let Some(p) = parent else { return Some(run_id) };
            run_id = p;
            parent = match self.storage.get_run(p) {
                Ok(Some(r)) => r.parent_run_id,
                _ => return None,
            };
        }
        parent.is_none().then_some(run_id)
    }

    #[allow(clippy::too_many_lines)] // One decision pipeline: deny floors, policy, gate, grants.
    fn decide_with_reason_inner(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        target: Option<&ControlTarget>,
    ) -> (Decision, Option<&'static str>) {
        // Scoped admission allows only two active runs per credential. Until
        // child admission shares that counter, external agents cannot fan out.
        if ctx.initiator == InitiatorKind::ExternalAgent && name == crate::subagent::SPAWN_SUBAGENT
        {
            return (Decision::Deny, None);
        }
        // The user's own computer only serves runs the owner started by
        // hand: never a schedule, webhook, channel, routine or external
        // agent, and never a run whose message came in through Telegram, the
        // local API or a paired device that is not the owner's admin desktop.
        if name.starts_with(builtin_tools::pc::PC_PREFIX) && !self.pc_owner_started(ctx) {
            return (Decision::Deny, None);
        }
        // Document converters are shell-free writers too. In particular a
        // one-column CSV can be a valid SKILL.md or workflow/config document.
        let output_field = match name {
            "docx_write" | "xlsx_write" => Some("path"),
            "csv_transform" => Some("output_path"),
            _ => None,
        };
        if let Some(path) = output_field
            .and_then(|field| input.get(field))
            .and_then(Value::as_str)
            && builtin_tools::files::check_daemon_write(self.storage.data_dir(), path).is_err()
        {
            return (Decision::Deny, None);
        }
        let mut raw_command = raw_command_for(name, input);
        if raw_command.is_empty() {
            input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .clone_into(&mut raw_command);
        }
        let req = ActionRequest {
            cwd: cwd_for(name, input),
            category,
            raw_command,
            // Preserve the bot's existing Human/Schedule policy as well as any new
            // source-specific restrictions. External authority cannot relax it.
            initiator: match ctx.initiator {
                InitiatorKind::ExternalAgent => InitiatorKind::Human,
                InitiatorKind::ExternalSchedule => InitiatorKind::Schedule,
                other => other,
            },
        };
        let r = &self.resolver;
        if self.locked_deny(category) {
            return (Decision::Deny, None);
        }
        let mut d = r.resolve(
            &self.global_policy.read(),
            ctx.bot_policy.as_ref(),
            None,
            &req,
        );
        // Resolve the source-specific policy too: a Human/Schedule Allow must
        // not bypass a base Deny or an explicit source-specific Deny, and the
        // durable initiator used for external-data isolation cannot relax it.
        if matches!(
            ctx.initiator,
            InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
        ) {
            let source_req = ActionRequest {
                cwd: req.cwd.clone(),
                category,
                raw_command: req.raw_command.clone(),
                initiator: ctx.initiator,
            };
            if r.resolve(
                &self.global_policy.read(),
                ctx.bot_policy.as_ref(),
                None,
                &source_req,
            ) == Decision::Deny
            {
                d = Decision::Deny;
            }
        }
        let must_ask = self.gate_must_ask(ctx, name, input);
        // Only a call that already has to ask can be covered, so the lookup is
        // skipped for every other call.
        let control_covered =
            must_ask && target.is_some_and(|t| ctx.control_covers(name, category, t));
        // A site the user remembered for this bot (owner-confirmed) satisfies the egress
        // gate for a plain navigation to exactly that origin, like a URL they typed.
        let remembered = must_ask
            && ctx.initiator == InitiatorKind::Human
            && self.remembered_origin(ctx, name, category, input);
        // "Allow for this task" for this one MCP tool: the egress term and a plain policy ask,
        // until the run reads something new. Always-ask tools, external callers and floors are
        // decided below before it is looked at.
        let run_tool_covered = self.run_tool_covers(ctx, name, input);
        let must_ask = must_ask && !control_covered && !remembered && !run_tool_covered;
        // A tool that delegates future authority is confirmed every time:
        // an allow becomes a plain ask, and no grant waives it (a deny stays).
        // The same holds for a call that types an owner-stored secret
        // ([`Tool::always_ask_for`]): the policy level, the Full preset, read
        // shortcuts, "this chat" / "always" grants and any run-scoped control
        // grant sit BELOW this early return, so none of them can waive it.
        if self.must_ask(name, input)
            || matches!(
                ctx.initiator,
                InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
            )
        {
            if d == Decision::Allow {
                d = Decision::ask();
            }
            return (d, None);
        }
        // A read of the user's computer asks once per run and family, whatever
        // the mode, unless the owner remembered it for this bot and computer.
        let pc_read_pending = effect == Effect::Read
            && pc_read_family(name).is_some_and(|f| !ctx.pc_reads.lock().contains(f))
            && !self.plain_granted(ctx, name, category, input);
        if !d.is_ask() {
            if d == Decision::Allow && pc_read_pending {
                return (Decision::ask(), None);
            }
            // A clean Allow becomes an ask once the run is tainted and this
            // call can leave the machine. A Deny is unchanged.
            if d == Decision::Allow && must_ask && !self.gate_waived(ctx, name, input) {
                return (Decision::ask(), None);
            }
            return (d, None);
        }
        // An `Ask` forced by an absolute floor survives even a full policy.
        // Unevaluated `AllowWithLimits` is one of those floors: the constraints
        // ride on the decision, and read shortcuts / grants must not waive it.
        // Money and the never-list are the others (they stay `Ask` under
        // [`Policy::full`]). Only a plain policy-level `Ask` can be waived.
        if d.unsupported_limits().is_some() {
            return (d, None);
        }
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if floor {
            return (d, None);
        }
        // The egress gate and the executor rule are not grants and not the read
        // shortcut. The only way past the egress gate is the bot's explicit
        // `allow_egress_after_reads` flag, which already made that term false;
        // it does not make the executor term false.
        if must_ask {
            return (d, None);
        }
        let granted =
            control_covered || run_tool_covered || self.plain_granted(ctx, name, category, input);
        let read_skip = effect == Effect::Read
            && if name.starts_with(builtin_tools::pc::PC_PREFIX) {
                !pc_read_pending
            } else {
                self.cfg.read_tools_skip_ask
            };
        if granted || read_skip {
            return (Decision::Allow, None);
        }
        // The autonomy mode's lane. It sits after every floor, the taint gate
        // and the grants, so it can only turn a plain policy-level ask into an
        // allow, never override one of those.
        match self.mode_lane(ctx, name, effect, category, input) {
            Some(reason) => (Decision::Allow, Some(reason.code())),
            None => (d, None),
        }
    }

    /// A this-chat or remembered grant of this bot covers the call (scoped to
    /// the computer the tool acts on).
    fn plain_granted(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
    ) -> bool {
        self.scoped_covers(ctx, name, category, input) || {
            let g = self.grants.lock();
            g.this_chat.contains(&(
                ctx.chat_id,
                ctx.bot_id,
                name.to_owned(),
                ctx.tool_grant_scope(name, category),
            )) || g.covers(
                ctx.bot_id,
                name,
                category,
                ctx.tool_grant_scope(name, category).as_deref(),
            )
        }
    }

    /// The run (or the top-level run of a subagent) was started by the owner
    /// by hand: a human turn whose every user message was typed in the
    /// desktop app on this machine (`owner`) or sent from the owner's admin
    /// desktop paired with this host (`peer_admin`). Fails closed.
    pub(crate) fn pc_owner_started(&self, ctx: &RunCtx) -> bool {
        if ctx.initiator != InitiatorKind::Human {
            return false;
        }
        owner_started_run(&self.storage, ctx.run_id, ctx.parent_run_id)
    }

    /// Record that the autonomy mode, not an approval, let this call run: the
    /// reason code and the mode, so "why did it not ask?" has an answer.
    pub(crate) fn audit_auto_allow(&self, ctx: &RunCtx, tool: &str, reason: &str) {
        self.audit(
            ctx,
            "system",
            "approval.auto_allowed",
            &json!({
                "tool": tool,
                "reason": reason,
                "mode": ctx.profile.permissions.mode().as_str(),
            }),
        );
    }

    /// A remembered rule (this chat, or persisted for this bot) covers this call's one target.
    /// Never wider than the rule: a site, a folder or a command prefix, compared strictly.
    fn scoped_covers(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
    ) -> bool {
        let Derived::Scope(call) = scoped_grant::derive(name, input) else {
            return false;
        };
        let computer = ctx.tool_grant_scope(name, category);
        let g = self.grants.lock();
        g.this_chat_scoped
            .iter()
            .any(|(chat, bot, tool, rule, comp)| {
                *chat == ctx.chat_id
                    && *bot == ctx.bot_id
                    && tool == name
                    && *comp == computer
                    && rule.covers(&call)
            })
            || g.covers_scoped(ctx.bot_id, name, &call, computer.as_deref())
    }

    /// A persisted rule names this navigation's origin.
    fn remembered_origin(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
    ) -> bool {
        if name != "browser_open" || input.as_object().is_none_or(|o| o.len() != 1) {
            return false;
        }
        let Derived::Scope(call) = scoped_grant::derive(name, input) else {
            return false;
        };
        self.grants.lock().covers_scoped(
            ctx.bot_id,
            name,
            &call,
            ctx.tool_grant_scope(name, category).as_deref(),
        )
    }

    /// The Balanced lane for this call, `None` in every other mode.
    ///
    /// Only the user's own runs (typed or scheduled) get a lane: a peer, an
    /// external agent or a routine keep asking. A call counts as "own computer"
    /// only when the run resolved to the bot's host, never the user's PC; the
    /// `pc_*` family (the user's own computer) never gets a lane at all.
    fn mode_lane(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
    ) -> Option<AllowReason> {
        if name.starts_with(builtin_tools::pc::PC_PREFIX)
            || ctx.profile.permissions.mode() != AutonomyMode::Balanced
            || !matches!(
                ctx.initiator,
                InitiatorKind::Human | InitiatorKind::Schedule
            )
        {
            return None;
        }
        balanced_lane(&CallFacts {
            category,
            impact: match effect {
                Effect::Read => Impact::Read,
                Effect::Write => Impact::Write,
                Effect::Destructive => Impact::Destructive,
            },
            own_computer: self.is_host_homed()
                && matches!(
                    ctx.resolved_computer.label,
                    ComputerLabel::Host | ComputerLabel::Browser
                ),
            named_browser_profile: input
                .get("profile")
                .and_then(Value::as_str)
                .is_some_and(|p| !p.is_empty() && p != "isolated"),
        })
    }

    /// The site of a URL as a grant boundary: `scheme://host:port`, lower case, no trailing dot,
    /// default port made explicit. Only `http` / `https` with a host qualify.
    fn origin_of(url: &str) -> Option<String> {
        scoped_grant::origin_of(url)
    }

    /// Tool class and destination a taint-gate grant is keyed on: web (`browser_open`, per
    /// origin), page actions (browser-reported origin), shell, desktop input. An unknown page
    /// stays once-only (a grant for site A must never cover site B). `None` for everything else
    /// (sends, schedules, apps, program launches).
    pub(crate) fn gate_key(
        name: &str,
        input: &Value,
        page_origin: Option<&str>,
    ) -> Option<(&'static str, String)> {
        match name {
            // A page action is bound to the origin the browser reported when it was resolved
            // (checked again by the tool when it runs); unknown stays once-only.
            n if crate::shared::is_page_action(n) => page_origin.map(|o| ("page", o.to_owned())),
            "shell_run" | "script_run" => Some(("shell", String::new())),
            "desktop_type" | "desktop_key" | "desktop_set_value" | "desktop_drag" => {
                Some(("desktop_input", String::new()))
            }
            "browser_open" => Some(("web", Self::origin_of(input.get("url")?.as_str()?)?)),
            _ => None,
        }
    }

    /// The destination of this call is a URL the user wrote in this chat: the COMPLETE
    /// destination (scheme, userinfo, host, port, path, query, fragment) must be equal after
    /// standard normalisation only (case of scheme and host, default port, trailing dot), and
    /// the call must be a plain navigation: any other field (`dialog`, `prompt_text`, `profile`)
    /// can carry model-chosen text, so such a call stays behind approval. Only messages tagged
    /// `owner` count (typed in the desktop app); a block the owner pasted from a page is still
    /// theirs by this test (the app cannot tell a paste from typing).
    fn user_typed_url(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if name != "browser_open" || input.as_object().is_none_or(|o| o.len() != 1) {
            return false;
        }
        let parts = |u: &str| -> Option<UrlParts> {
            let url = url::Url::parse(u).ok()?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            Some((
                url.scheme().to_owned(),
                format!("{}:{}", url.username(), url.password().unwrap_or_default()),
                url.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
                url.port_or_known_default(),
                url.path().to_owned(),
                url.query().map(str::to_owned),
                url.fragment().map(str::to_owned),
            ))
        };
        let Some(want) = input.get("url").and_then(Value::as_str).and_then(parts) else {
            return false;
        };
        // Only messages the owner typed in the desktop app (tagged `owner` by the route that
        // received them): not a peer's, Telegram's, a schedule's or another local client's.
        let Ok(texts) = self.storage.owner_message_texts(ctx.chat_id) else {
            return false;
        };
        texts
            .iter()
            .flat_map(|m| m.split_whitespace())
            .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
            .map(|w| {
                w.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\''])
            })
            .filter_map(parts)
            .any(|got| got == want)
    }

    /// A call that only the taint gate (not the policy, not an always-ask tool, not a floor)
    /// makes ask, on a bot whose level allows it: the card may offer "always for this chat".
    pub(crate) fn gate_grantable(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
        page_origin: Option<&str>,
    ) -> bool {
        if ctx.initiator != InitiatorKind::Human
            || self.must_ask(name, input)
            || Self::gate_key(name, input, page_origin).is_none()
        {
            return false;
        }
        let mut raw = raw_command_for(name, input);
        if raw.is_empty() {
            input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .clone_into(&mut raw);
        }
        let req = ActionRequest {
            cwd: cwd_for(name, input),
            category,
            raw_command: raw,
            initiator: InitiatorKind::Human,
        };
        let policy = self.resolver.resolve(
            &self.global_policy.read(),
            ctx.bot_policy.as_ref(),
            None,
            &req,
        );
        policy == Decision::Allow && self.gate_must_ask(ctx, name, input)
    }

    /// The gate is satisfied by an earlier "always for this chat" for this tool class and
    /// destination, or by a URL the user typed.
    fn gate_waived(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if ctx.initiator != InitiatorKind::Human {
            return false;
        }
        if self.user_typed_url(ctx, name, input) {
            return true;
        }
        Self::gate_key(name, input, ctx.page_origin.lock().as_deref()).is_some_and(
            |(class, dest)| {
                self.grants
                    .lock()
                    .gate_chat
                    .contains(&(ctx.chat_id, ctx.bot_id, class, dest))
            },
        )
    }

    /// The taint gate for this call: egress after a read, or a program start
    /// after untrusted content. Not a grant and not the read shortcut.
    ///
    /// `web_search` can only reach the configured search provider. Every other
    /// egress tool, including fetch, can be aimed at an arbitrary host. A click
    /// or a scroll does not carry bytes; a drag can, and `browser_click` does
    /// when `prompt_text` is set. A call that starts a program chosen by its
    /// arguments also asks after untrusted content, and the egress waiver does
    /// not cover it.
    ///
    /// An action inside the browser the owner connected as a browser (an MCP
    /// server of kind `browser` or the built-in Playwright preset, marked on
    /// the tool's class by the bridge from owner configuration) passes while
    /// the run has read only untrusted content, and asks once it has read
    /// something private.
    pub(crate) fn gate_must_ask(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        let class = self.content_class(ctx, name, input);
        let exposure = ctx.taint.lock().exposure;
        let reach = if name == "web_search" {
            EgressReach::ConfiguredSearch
        } else if class.in_owner_browser {
            EgressReach::OwnerBrowser
        } else {
            EgressReach::Arbitrary
        };
        egress_must_ask(
            exposure,
            taint::call_can_egress(name, class, input),
            reach,
            ctx.profile.permissions.allow_egress_after_reads,
        ) || executes_must_ask(exposure, class.executes)
            || (class.in_owner_browser && crate::browser_url::opens_non_public(name, input))
    }

    /// The tools "allow for this task" ([`ApprovalScope::RunTool`]) can name:
    /// one tool of one MCP server (`mcp:<server>/<tool>`), nothing else.
    pub(crate) fn run_tool_eligible(name: &str) -> bool {
        name.strip_prefix("mcp:")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(server, tool)| !server.is_empty() && !tool.is_empty())
    }

    /// An "allow for this task" grant of this run covers this call: the same
    /// tool, a run that has read nothing new since the grant, and never a
    /// program start after untrusted content (that asks every time).
    fn run_tool_covers(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if !Self::run_tool_eligible(name) {
            return false;
        }
        // A grant never lets a file, localhost or LAN address through: the URL is judged again
        // at every call.
        if self.content_class(ctx, name, input).in_owner_browser
            && crate::browser_url::opens_non_public(name, input)
        {
            return false;
        }
        let exposure = ctx.taint.lock().exposure;
        if executes_must_ask(exposure, self.content_class(ctx, name, input).executes) {
            return false;
        }
        ctx.tool_grants
            .lock()
            .iter()
            .any(|g| g.covers(name, exposure))
    }

    /// Content class of tool `name` for this run's bot. A native step that
    /// replay or a script authorizes without a registered tool still gets its
    /// built-in class, so a launch or a shell step is never treated as a
    /// non-executor. A tool can raise `egress` for one bot through
    /// [`tool_registry::Tool::egress_for`] (knowledge search with a remote
    /// embedding provider); that can only add egress, never remove it.
    pub(crate) fn content_class(&self, ctx: &RunCtx, name: &str, input: &Value) -> ContentClass {
        // Discovery has no private read or egress. Its returned metadata is
        // classified separately as output exposure.
        if self.is_host_status_call(ctx, name, input)
            || matches!(
                name,
                agent_router::TOOL_SEARCH | agent_router::TOOL_DESCRIBE
            )
        {
            return ContentClass::none();
        }
        let Some(tool) = self.tools.get(name) else {
            // The discovery tools live on the surface, not in the registry. They only list
            // tool names (`use_tool` is judged as its target): they never read user data
            // or send anything. Without this they fell into "unspecified" (egress) and asked
            // for approval after any read, even under the Full preset.
            if matches!(
                name,
                agent_router::FIND_TOOL | agent_router::GET_TOOL_DETAILS
            ) {
                return ContentClass::none();
            }
            return builtin_tools::classify(name).unwrap_or_else(ContentClass::unspecified);
        };
        let mut class = tool.spec().content;
        if tool.egress_for(Some(&ctx.bot_id.to_string())) == Some(true) {
            class.egress = true;
        }
        class
    }

    /// A host-status command (`uptime`, `df -h`, ... see
    /// [`permission_engine::is_host_status_command`]) on the bot's OWN host computer.
    ///
    /// Such a call prints a fixed status line, takes no path, URL or free text, and cannot send
    /// or change anything, so it neither taints the run (its output carries no outsider's text)
    /// nor counts as a program an untrusted page chose. It is judged from the input alone, never
    /// from the result. The user's PC, any other shell dialect, a working directory and every
    /// other shell command keep the full class. Policy-level asks (Ask mode, never-list) are not
    /// touched: this only removes the taint-driven repeats.
    pub(crate) fn is_host_status_call(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if name != "shell_run" || !self.is_host_homed() {
            return false;
        }
        if !matches!(
            ctx.resolved_computer.label,
            ComputerLabel::Host | ComputerLabel::Browser
        ) {
            return false;
        }
        let Some(obj) = input.as_object() else {
            return false;
        };
        let plain_fields = obj.keys().all(|k| match k.as_str() {
            "command" | "timeout" => true,
            "shell" => matches!(obj[k].as_str(), Some("sh" | "bash")),
            _ => false,
        });
        plain_fields
            && obj
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(permission_engine::is_host_status_command)
    }

    /// This very call must be confirmed (the tool always asks, or its
    /// [`tool_registry::Tool::always_ask_for`] says so for this input).
    pub(crate) fn must_ask(&self, name: &str, input: &Value) -> bool {
        self.tools
            .get(name)
            .is_some_and(|t| t.always_ask() || t.always_ask_for(input))
    }

    /// An owner-created standing grant that covers exactly this call, as the
    /// tool itself verifies it right now. Only meaningful for calls that
    /// [`Self::must_ask`]; recorded in the audit log by id.
    pub(crate) async fn standing_grant(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
    ) -> Option<tool_registry::StandingGrant> {
        let tool = self.tools.get(name)?;
        if !tool.always_ask_for(input) {
            return None;
        }
        // An owner's standing grant covers the secret-fill ask. It is a grant,
        // so it never waives the egress or executor ask of a tainted run.
        if self.gate_must_ask(ctx, name, input) {
            return None;
        }
        let tctx = tool_registry::ToolContext {
            run_id: ctx.run_id.to_string(),
            bot_id: Some(ctx.bot_id.to_string()),
            ..tool_registry::ToolContext::default()
        };
        let grant = tool.standing_grant(&tctx, input).await?;
        self.audit(
            ctx,
            "system",
            "secret.standing_grant_used",
            &json!({ "tool": name, "grant": grant.id, "label": grant.label }),
        );
        Some(grant)
    }

    /// Egress card for a tainted run. `None` when the run is clean, or the tool
    /// cannot send data off the machine. Attached even when the ask came from
    /// `always_ask` or a floor, so the card still names the read and the host.
    ///
    /// The payload is the revealed text. Tool-injected defaults (the browser
    /// profile, the search backend, a skill body) are part of that text.
    /// Every tool that starts a program is also an egress tool (the production
    /// registry audit checks it), so its card carries the full command.
    fn egress_card(&self, ctx: &RunCtx, name: &str, input: &Value) -> Option<Value> {
        let class = self.content_class(ctx, name, input);
        if !taint::call_can_egress(name, class, input) {
            return None;
        }
        let mut details = {
            let taint = ctx.taint.lock();
            taint::egress_record(&taint, name, input)?
        };
        let tctx = tool_registry::ToolContext {
            run_id: ctx.run_id.to_string(),
            bot_id: Some(ctx.bot_id.to_string()),
            content_tainted: true,
            ..tool_registry::ToolContext::default()
        };
        if let Some(tool) = self.tools.get(name) {
            if let Some(host) = tool.egress_host(input) {
                taint::set_egress_host(&mut details, &host);
            }
            if let Some(extra) = tool.egress_extra(&tctx, input) {
                taint::append_payload(&mut details, &extra);
            }
        }
        if executes_must_ask(ctx.taint.lock().exposure, class.executes) {
            taint::note_executes_on_egress(&mut details);
        }
        if class.in_owner_browser {
            taint::note_owner_browser(&mut details, name, input);
        }
        taint::finalize_egress_record(&mut details);
        Some(details)
    }

    /// Why this call asks, as a short kind the UI turns into one human line
    /// (`policy`, `egress`, `executes`, `always_ask`, `floor`).
    fn ask_kind(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
    ) -> &'static str {
        if self.must_ask(name, input) {
            return "always_ask";
        }
        let class = self.content_class(ctx, name, input);
        if class.in_owner_browser && crate::browser_url::opens_non_public(name, input) {
            return "browser_address";
        }
        let exposure = ctx.taint.lock().exposure;
        if executes_must_ask(exposure, class.executes) {
            return "executes";
        }
        if self.gate_must_ask(ctx, name, input) {
            return "egress";
        }
        let mut raw = raw_command_for(name, input);
        if raw.is_empty() {
            input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .clone_into(&mut raw);
        }
        let req = ActionRequest {
            cwd: cwd_for(name, input),
            category,
            raw_command: raw,
            initiator: InitiatorKind::Human,
        };
        if self.resolver.resolve(&Policy::full(), None, None, &req) != Decision::Allow {
            return "floor";
        }
        "policy"
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
                cwd: None,
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
                && self.grants.lock().always.iter().any(|g| {
                    !g.inert
                        && g.bot_id == ctx.bot_id
                        && g.target == GrantTarget::Category(c)
                        && g.computer == ctx.grant_scope(c)
                })
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
    pub(crate) fn locked_deny(&self, category: PermissionCategory) -> bool {
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
    ) -> (Decision, bool, Vec<String>) {
        let req = ActionRequest {
            cwd: None,
            category,
            raw_command: String::new(),
            initiator: InitiatorKind::Human,
        };
        let r = &self.resolver;
        let computer_scoped = matches!(
            category,
            PermissionCategory::Desktop | PermissionCategory::Browser
        );
        let scopes = {
            let mut scopes: Vec<String> = self
                .grants
                .lock()
                .always
                .iter()
                .filter(|g| {
                    !g.inert
                        && g.bot_id == bot_id
                        && g.target == GrantTarget::Category(category)
                        && g.computer.is_some()
                })
                .filter_map(|g| g.computer.clone())
                .collect();
            scopes.sort();
            scopes.dedup();
            scopes
        };
        if self.locked_deny(category) {
            return (Decision::Deny, false, scopes);
        }
        let d = r.resolve(&self.global_policy.read(), bot_policy, None, &req);
        // Same rule as `decide`: only a plain policy-level `Ask` can be waived.
        // An unsupported-limit ask is not waivable, grant or not.
        // Desktop and browser grants are per computer: the category stays ask
        // here, and `scopes` names the computers that are allowed.
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if d.is_ask()
            && d.unsupported_limits().is_none()
            && !floor
            && !computer_scoped
            && self.grants.lock().always.iter().any(|g| {
                !g.inert
                    && g.bot_id == bot_id
                    && g.target == GrantTarget::Category(category)
                    && g.computer.is_none()
            })
        {
            return (Decision::Allow, true, scopes);
        }
        (d, false, scopes)
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
        grantable: bool,
    ) -> (Uuid, PendingGuard, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(
            call_id,
            Pending {
                tx,
                once_only,
                grantable,
                gate_grant: false,
                run_tool: false,
                always_refused: false,
            },
        );
        *self.parked.lock().entry(ctx.run_id).or_insert(0) += 1;
        let approval_id = Uuid::now_v7();
        let guard = PendingGuard {
            sh: Arc::clone(self),
            id: call_id,
            run_id: ctx.run_id,
            approval_id,
            released: false,
        };
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
    /// run is asked to pause (resumable).  Any other run waits at most
    /// [`LoopConfig::approval_timeout`](crate::LoopConfig) (zero = no limit);
    /// then the call is denied as expired and the run carries on.
    async fn wait_verdict(&self, ctx: &RunCtx, rx: oneshot::Receiver<Verdict>) -> Waited {
        if !matches!(
            ctx.initiator,
            InitiatorKind::Schedule | InitiatorKind::ExternalSchedule
        ) {
            let limit = self.cfg.approval_timeout;
            if limit.is_zero() {
                return Waited::Verdict(rx.await);
            }
            if let Ok(v) = tokio::time::timeout(limit, rx).await {
                return Waited::Verdict(v);
            }
            self.audit(
                ctx,
                "system",
                "approval.expired",
                &json!({ "timeout_secs": limit.as_secs() }),
            );
            return Waited::Expired;
        }
        let limit = self.cfg.schedule_approval_timeout;
        if let Ok(v) = tokio::time::timeout(limit, rx).await {
            return Waited::Verdict(v);
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
        Waited::Paused
    }

    /// A wait that ended without a verdict (the tool around it timed out, the run was stopped):
    /// the approval row expires, every client's card closes (`tool_call_denied` with the expired
    /// reason) and a run that is still alive leaves `waiting_approval` when this was its last
    /// parked call.
    pub(crate) fn expire_abandoned(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
        approval_id: Uuid,
        last_parked: bool,
    ) {
        let _ = self.storage.decide_approval(approval_id, "expired");
        let alive = self
            .storage
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| RunState::parse(&r.state))
            .is_some_and(|s| matches!(s, RunState::Running | RunState::WaitingApproval));
        if !alive {
            return;
        }
        if last_parked {
            self.set_state(run_id, RunState::Running);
        }
        let ctx = self.runs.lock().get(&run_id).cloned();
        if let Some(ctx) = ctx {
            self.emit(
                &ctx,
                Event::ToolCallDenied(ToolCallDenied {
                    run_id,
                    call_id,
                    denied_by: ctx.user_id,
                    reason: Some(EXPIRED_REASON.into()),
                }),
            );
        }
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

    /// Refuse egress the user cannot safely approve.
    ///
    /// Characters that are never legitimate (tags, bidi overrides, invisible
    /// operators, private-use, U+FFFC, other controls) are refused whether or
    /// not a card is shown. Characters that real text uses (emoji variation
    /// selectors, ZWJ, ZWNJ, LRM/RLM, CR) are revealed on the card and may be
    /// approved. A prompt-free send has no card, so those same characters are
    /// refused unless they are in the small set that is ordinary in emoji and
    /// in Arabic, Hebrew, Persian, or Hindi. Large payloads require full review.
    ///
    /// `showing_card` is true on the approval path. The allow path (a search
    /// after untrusted content only, or the bot's egress waiver) passes false.
    pub(crate) fn refuse_unshowable_egress(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        token: Option<&str>,
        showing_card: bool,
    ) -> Result<(), String> {
        let class = self.content_class(ctx, name, input);
        if !taint::call_can_egress(name, class, input) || !ctx.taint.lock().exposure.is_tainted() {
            return Ok(());
        }
        let message = if taint::input_forbids_egress(input) {
            Some(
                "the outgoing text contains characters that cannot be sent (hidden tags, direction overrides, private-use or similar); remove them and ask again"
                    .to_owned(),
            )
        } else if !showing_card && taint::input_needs_a_card_to_show(input) {
            Some(
                "the outgoing text contains characters that would not be visible, and this call is not shown for approval; remove them and ask again"
                    .to_owned(),
            )
        } else {
            None
        };
        let Some(message) = message else {
            return Ok(());
        };
        if let Some(tool) = self.tools.get(name) {
            let tctx = tool_registry::ToolContext {
                run_id: ctx.run_id.to_string(),
                bot_id: Some(ctx.bot_id.to_string()),
                ..tool_registry::ToolContext::default()
            };
            tool.approval_closed(&tctx, input, token);
        }
        Err(message)
    }

    /// Whether the card for this call offers a run-scoped control grant, and
    /// the card text for it. Offered only on a once-only card, for a tool that
    /// is not always-ask, on a computer that has an identity, and only when the
    /// daemon resolved an eligible target for this very call. Returns the
    /// target (when the card carries a control block) and whether it is
    /// grantable.
    fn control_offer(
        &self,
        ctx: &RunCtx,
        name: &str,
        category: PermissionCategory,
        input: &Value,
        details: &mut Option<Value>,
        target: Option<ControlTarget>,
    ) -> (Option<ControlTarget>, bool) {
        let target = target.filter(|t| {
            control_kind_of(name) == Some(t.kind)
                && details.is_some()
                && !self.must_ask(name, input)
                && ctx.tool_grant_scope(name, category).is_some()
        });
        // The task-wide option exists only when the card can show the whole
        // program path: a path with hidden or control characters, or an absurd
        // length, is not offered (the call then asks once, as in #89).
        let grantable = target
            .as_ref()
            .is_some_and(|t| t.eligible() && path_fits_card(&t.identity.image_path));
        if let Some(t) = &target {
            attach_control_card(details, t, grantable);
        }
        (target, grantable)
    }

    /// Park the run in `WaitingApproval` until `approve` / `deny` is called.
    ///
    /// `Ok(permit)` means the user approved and the post-approve recheck did
    /// not see [`Decision::Deny`]. The caller must [`ExecPermit::commit`]
    /// immediately before invoking the tool: that recheck runs under
    /// [`Shared::exec_gate`] and is what creates a grant. `Err` means the
    /// call is denied (no grant, tool must not run).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // `details` and the control target joined an already wide call
    pub(crate) async fn request_approval(
        self: &Arc<Self>,
        ctx: &Arc<RunCtx>,
        call_id: ToolCallId,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        mut details: Option<Value>,
        target: Option<ControlTarget>,
    ) -> Result<ExecPermit, Option<String>> {
        // The tool's token for this approval: kept for the close guard, never shown.
        let token = details.as_ref().and_then(approval_ref);
        if let Some(o) = details.as_mut().and_then(Value::as_object_mut) {
            o.remove("approval_ref");
        }
        self.refuse_unshowable_egress(ctx, name, input, token.as_deref(), true)
            .map_err(Some)?;
        let disclosure = self.egress_card(ctx, name, input);
        let showed_disclosure = disclosure.is_some();
        if let Some(extra) = disclosure {
            taint::merge_egress(&mut details, extra);
        }
        // The tool's own card was checked before this merge. The egress
        // payload is part of the card the user sees and part of what the
        // permit will run.
        self.refuse_hidden_secret(ctx, name, input, details.as_ref(), token.as_deref())
            .map_err(Some)?;
        // An always-ask tool must show the user what it does: refuse to ask
        // without details rather than fall back to the generic card.
        let must_ask = self.must_ask(name, input);
        if must_ask && details.is_none() {
            return Err(Some(
                "internal error: this tool needs approval details to be asked about; it was not run"
                    .into(),
            ));
        }
        // A card that only the taint gate asks (a bot whose level allows the call) is not
        // once-only: it offers "always for this chat" for this tool class and site. The egress
        // disclosure in `details` is not tool-specific data.
        let page_origin = ctx.page_origin.lock().clone();
        let gate_grant = self.gate_grantable(ctx, name, category, input, page_origin.as_deref());
        // Approval memory: a tool remembered per target offers "this chat" / "always for this
        // bot" for that ONE target. A gate card (tainted run) offers it only for a site, where the
        // chat choice is keyed on the origin too; a call whose target cannot be named safely is
        // approved once.
        let derived = scoped_grant::derive(name, input);
        let memory = match &derived {
            Derived::Scope(sc)
                if !must_ask
                    && ctx.initiator == InitiatorKind::Human
                    && (!gate_grant || sc.kind == ScopeKind::Origin) =>
            {
                Some(sc.clone())
            }
            _ => None,
        };
        let unscopable = matches!(derived, Derived::Unscopable);
        let once_only = !gate_grant
            && (unscopable
                || (memory.is_none()
                    && (details.is_some()
                        || must_ask
                        || matches!(
                            ctx.initiator,
                            InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
                        ))));
        let (target, grantable) =
            self.control_offer(ctx, name, category, input, &mut details, target);
        let closed = self.closed_guard(ctx, name, input, token);
        let args = input.to_string();
        let mut prefix = computer_approval(ctx, name, category, &mut details);
        if ctx.initiator == InitiatorKind::ExternalAgent {
            prefix.push_str("[External agent: approve once only] ");
        }
        // A browser batch names every step in its summary (the usual cut at 300 characters
        // would hide the later ones) and never repeats typed text there.
        let shown = if name == "browser_act" {
            builtin_tools::act_summary(input)
                .unwrap_or_else(|| truncate_utf8(&args, 300).to_owned())
        } else {
            truncate_utf8(&args, 300).to_owned()
        };
        let summary = self.redact(format!("{prefix}{name} {shown}"));
        // Daemon-authored facts for the card: why it asks, and whether a standing choice
        // ("always for this chat") may be offered at all.
        let why = self.ask_kind(ctx, name, category, input);
        let exposure = ctx.taint.lock().exposure;
        // "Allow for this task": one MCP tool, this run. Not for an always-ask tool, a floor, a
        // program start after untrusted content, or an external caller.
        let run_tool = Self::run_tool_eligible(name)
            && matches!(why, "egress" | "policy")
            && !matches!(
                ctx.initiator,
                InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
            );
        // A standing tool grant for an MCP tool is not offered once the run read something
        // private: the owner decides that on a clean card, never next to private data.
        let always_refused = Self::run_tool_eligible(name) && exposure.has_private();
        let always_offered = !once_only
            && !always_refused
            && !matches!(
                why,
                "egress" | "executes" | "always_ask" | "browser_address"
            );
        let class = self.content_class(ctx, name, input);
        if let Some(obj) = details.get_or_insert_with(|| json!({})).as_object_mut() {
            obj.insert("why".into(), json!(why));
            obj.insert("once_only".into(), json!(once_only));
            obj.insert("run_grant".into(), json!(run_tool));
            obj.insert("always_offered".into(), json!(always_offered));
            if class.in_owner_browser {
                obj.insert("browser_action".into(), json!(true));
            }
            if let Some(sc) = memory.as_ref().filter(|_| !once_only) {
                obj.insert(
                    "memory".into(),
                    json!({ "kind": sc.kind.as_str(), "pattern": sc.pattern }),
                );
            }
        }
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary, once_only, grantable);
        if let Some(p) = self.pending.lock().get_mut(&call_id) {
            p.gate_grant = gate_grant || (memory.is_some() && !once_only);
            p.run_tool = run_tool;
            p.always_refused = always_refused;
        }
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
        let waited = self.wait_verdict(ctx, rx).await;
        guard.resume();
        let verdict = match waited {
            Waited::Verdict(v) => v,
            Waited::Paused => {
                let _ = self.storage.decide_approval(approval_id, "expired");
                return Err(Some("nobody approved in time; the run is paused".into()));
            }
            Waited::Expired => {
                let _ = self.storage.decide_approval(approval_id, "expired");
                self.emit(
                    ctx,
                    Event::ToolCallDenied(ToolCallDenied {
                        run_id: ctx.run_id,
                        call_id,
                        denied_by: ctx.user_id,
                        reason: Some(EXPIRED_REASON.into()),
                    }),
                );
                return Err(Some(EXPIRED_REASON.into()));
            }
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
                        self.decide_with_target(
                            ctx,
                            name,
                            effect,
                            category,
                            input,
                            target.as_ref()
                        ),
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
                    showed_disclosure,
                    page_origin,
                    target: target.filter(|_| grantable),
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
            Ok(Verdict::Answered { .. } | Verdict::SecretStored) | Err(_) => {
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
    fn add_grant(
        &self,
        ctx: &RunCtx,
        target: GrantTarget,
        category: PermissionCategory,
        name: &str,
    ) {
        let grant = Grant {
            bot_id: ctx.bot_id,
            target,
            granted_at_ms: now_ms(),
            computer: ctx.tool_grant_scope(name, category),
            inert: false,
        };
        {
            let mut g = self.grants.lock();
            if g.always.iter().any(|x| {
                x.bot_id == grant.bot_id && x.target == grant.target && x.computer == grant.computer
            }) {
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
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary, false, false);
        self.emit(
            ctx,
            Event::QuestionAsked(QuestionAsked {
                run_id: ctx.run_id,
                call_id,
                question,
                options,
            }),
        );
        let waited = self.wait_verdict(ctx, rx).await;
        guard.resume();
        let verdict = match waited {
            Waited::Verdict(v) => v,
            Waited::Paused => {
                let _ = self.storage.decide_approval(approval_id, "expired");
                return Err(Some("nobody answered in time; the run is paused".into()));
            }
            Waited::Expired => {
                // The question card closes with its call: the tool completes with this reason.
                let _ = self.storage.decide_approval(approval_id, "expired");
                return Err(Some(EXPIRED_REASON.into()));
            }
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
            Ok(Verdict::Approved { .. } | Verdict::SecretStored) | Err(_) => {
                let _ = self.storage.decide_approval(approval_id, "declined");
                Err(Some("question was dropped".into()))
            }
        }
    }
}

/// What the model reads when a `secret_request` ends. Never a value.
fn secret_outcome(name: &str, stored: bool, why: &str) -> (String, bool) {
    if stored {
        (
            format!(
                "stored: the owner saved `{name}`. Use it by name (secrets_list, then the `secret` parameter); you cannot read it."
            ),
            false,
        )
    } else {
        (
            format!("cancelled: nothing was stored for `{name}` ({why})."),
            true,
        )
    }
}

impl Shared {
    /// Run a `secret_request`: show the owner's form and park the run until the
    /// owner's app submits it to the daemon, cancels it, or it expires. The
    /// value never passes through here: the owner-only submit route writes it
    /// to the vault and then calls `AgentLoop::finish_secret_request`; this
    /// returns only `stored` / `cancelled` and the name.
    ///
    /// Refused (nothing shown) unless the run was started by the owner at the
    /// desktop (`Human`: never a channel such as Telegram, a schedule, a
    /// subagent or an external agent), requests are switched on, and the
    /// per-run limit is not used up.
    pub(crate) async fn request_secret(
        self: &Arc<Self>,
        ctx: &RunCtx,
        call_id: ToolCallId,
        card: &builtin_tools::SecretCard,
    ) -> (String, bool) {
        let name = card.name.as_str();
        let refuse = |why: &str| {
            self.audit(
                ctx,
                "system",
                "secret_request.refused",
                &json!({"name": name, "reason": why}),
            );
            secret_outcome(name, false, why)
        };
        if !self.cfg.secret_requests {
            return refuse("secret requests are switched off");
        }
        if ctx.initiator != InitiatorKind::Human {
            return refuse(
                "only a run the owner started at the desktop can ask for a credential; ask the owner to add it in the app instead",
            );
        }
        {
            let mut st = ctx.state.lock();
            if st.secret_requests >= self.cfg.max_secret_requests_per_run {
                drop(st);
                return refuse("this run has asked for credentials too often");
            }
            st.secret_requests += 1;
        }
        let tainted = self
            .root_run_id(ctx)
            .is_none_or(|r| self.router.tainted_runs.lock().contains(&r));
        let limit = if self.cfg.approval_timeout.is_zero() {
            self.cfg.secret_request_timeout
        } else {
            self.cfg
                .secret_request_timeout
                .min(self.cfg.approval_timeout)
        };
        let fields = match card.fields {
            builtin_tools::SecretFields::Secret => protocol::event::SecretFields::Secret,
            builtin_tools::SecretFields::Login => protocol::event::SecretFields::Login,
        };
        let summary = format!("secret request: {}", truncate_utf8(name, 80));
        let (approval_id, mut guard, rx) = self.park(ctx, call_id, &summary, true, false);
        self.secret_requests.lock().insert(
            call_id,
            crate::shared::SecretRequestInfo {
                run_id: ctx.run_id,
                name: name.to_owned(),
                fields,
                replace: card.replace,
            },
        );
        self.audit(
            ctx,
            "system",
            "secret_request.requested",
            &json!({"name": name}),
        );
        self.emit(
            ctx,
            Event::SecretRequested(protocol::event::SecretRequested {
                run_id: ctx.run_id,
                call_id,
                name: name.to_owned(),
                purpose: self.redact(card.purpose.clone()),
                fields,
                replaces_existing: card.replace,
                untrusted_content: tainted,
                expires_in_secs: limit.as_secs(),
            }),
        );
        let mut rx = rx;
        let mut waited = tokio::time::timeout(limit, &mut rx).await;
        // The timer may end the wait only while the form is still unclaimed. A
        // submit that claimed it is writing the value to the vault right now: wait
        // for it to finish, so the model hears `stored` (never a false "nothing was
        // stored", which would make it ask for the password in chat). If that
        // submit fails the form is put back and expires here on the next look.
        while waited.is_err() {
            if self.secret_requests.lock().remove(&call_id).is_some() {
                break;
            }
            waited = tokio::time::timeout(std::time::Duration::from_millis(100), &mut rx).await;
        }
        guard.resume();
        self.secret_requests.lock().remove(&call_id);
        let (stored, why, decision) = match waited {
            Ok(Ok(Verdict::SecretStored)) => (true, "", "stored"),
            Err(_) => (false, "the owner did not answer in time", "expired"),
            _ => (false, "the owner cancelled", "declined"),
        };
        let _ = self.storage.decide_approval(approval_id, decision);
        self.audit(
            ctx,
            "user",
            if stored {
                "secret_request.stored"
            } else {
                "secret_request.cancelled"
            },
            &json!({"name": name, "reason": decision}),
        );
        secret_outcome(name, stored, why)
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
    /// True when the card built for this permit included the egress
    /// disclosure. Captured then, not recomputed after the user answers:
    /// another call can taint the run while this card is open.
    pub(crate) showed_disclosure: bool,
    /// Immutable origin approved by this card, even if a later call observes another page.
    page_origin: Option<String>,
    /// The eligible control target the card offered a grant for. `Some` only
    /// when the card offered [`ApprovalScope::RunControl`].
    target: Option<ControlTarget>,
}

impl ExecPermit {
    /// Last check before the tool runs. Holds `exec_gate` so
    /// [`install_global_policy`] cannot change the policy between this
    /// `decide` and the caller's decision to invoke the tool.
    ///
    /// `Err` — do not call the tool; no grant is stored.
    #[allow(clippy::too_many_lines)] // Recheck, grant record and audit under one gate.
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
            self.sh.decide_with_target(
                &self.ctx,
                &self.name,
                self.effect,
                self.category,
                &self.input,
                self.target.as_ref(),
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
        // An approved read of the user's computer covers that family for the
        // rest of THIS run only.
        if self.effect == Effect::Read
            && let Some(family) = pc_read_family(&self.name)
        {
            self.ctx.pc_reads.lock().insert(family);
        }
        // Approving an always-ask tool confirms this one call only.
        let scope = if self.sh.must_ask(&self.name, &self.input)
            || matches!(
                self.ctx.initiator,
                InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
            ) {
            ApprovalScope::Once
        } else {
            self.scope
        };
        // A taint-gate card never creates a tool-wide or standing grant: the choice is kept
        // for this chat, keyed on tool class and site.
        let gate_grant = matches!(
            scope,
            ApprovalScope::ThisChat | ApprovalScope::Always | ApprovalScope::AlwaysCategory
        ) && self.sh.gate_grantable(
            &self.ctx,
            &self.name,
            self.category,
            &self.input,
            self.page_origin.as_deref(),
        );
        if gate_grant
            && let Some((class, dest)) =
                Shared::gate_key(&self.name, &self.input, self.page_origin.as_deref())
        {
            {
                self.sh.grants.lock().gate_chat.insert((
                    self.ctx.chat_id,
                    self.ctx.bot_id,
                    class,
                    dest,
                ));
            }
        }
        // "Always" on a site's gate card remembers that origin for the bot (owner-confirmed).
        if gate_grant
            && scope == ApprovalScope::Always
            && self.name == "browser_open"
            && let Derived::Scope(target) = scoped_grant::derive(&self.name, &self.input)
        {
            self.sh.add_grant(
                &self.ctx,
                GrantTarget::Scoped {
                    tool: self.name.clone(),
                    scope: target,
                },
                self.category,
                &self.name,
            );
        }
        let scope = if gate_grant {
            ApprovalScope::Once
        } else {
            scope
        };
        // A tool remembered per target remembers that one target and nothing wider.
        let derived = scoped_grant::derive(&self.name, &self.input);
        match scope {
            ApprovalScope::Once => {}
            ApprovalScope::ThisChat | ApprovalScope::Always if derived != Derived::NotScoped => {
                if let Derived::Scope(target) = derived {
                    if scope == ApprovalScope::ThisChat {
                        self.sh.grants.lock().this_chat_scoped.insert((
                            self.ctx.chat_id,
                            self.ctx.bot_id,
                            self.name.clone(),
                            target,
                            self.ctx.tool_grant_scope(&self.name, self.category),
                        ));
                    } else {
                        self.sh.add_grant(
                            &self.ctx,
                            GrantTarget::Scoped {
                                tool: self.name.clone(),
                                scope: target,
                            },
                            self.category,
                            &self.name,
                        );
                    }
                }
            }
            ApprovalScope::ThisChat => {
                self.sh.grants.lock().this_chat.insert((
                    self.ctx.chat_id,
                    self.ctx.bot_id,
                    self.name.clone(),
                    self.ctx.tool_grant_scope(&self.name, self.category),
                ));
            }
            ApprovalScope::Always => {
                self.sh.add_grant(
                    &self.ctx,
                    GrantTarget::Tool(self.name.clone()),
                    self.category,
                    &self.name,
                );
            }
            ApprovalScope::AlwaysCategory => {
                self.sh.add_grant(
                    &self.ctx,
                    GrantTarget::Category(self.category),
                    self.category,
                    &self.name,
                );
            }
            ApprovalScope::RunTool => {
                // `approve` refuses this scope unless the card offered it. The grant keeps the
                // exposure of this moment: a later new kind of read makes the next call ask.
                if Shared::run_tool_eligible(&self.name) {
                    let grant = RunToolGrant {
                        tool: self.name.clone(),
                        exposure: self.ctx.taint.lock().exposure,
                    };
                    let mut grants = self.ctx.tool_grants.lock();
                    if !grants.contains(&grant) {
                        grants.push(grant);
                    }
                }
            }
            ApprovalScope::RunControl => {
                // `approve` refuses this scope unless the card offered it, so a
                // target is present; without one nothing is granted.
                if let (Some(target), Some(computer)) = (
                    &self.target,
                    self.ctx.tool_grant_scope(&self.name, self.category),
                ) {
                    let mut grants = self.ctx.control_grants.lock();
                    let grant = ControlGrant {
                        computer,
                        identity: target.identity.clone(),
                    };
                    if !grants.contains(&grant) {
                        grants.push(grant);
                    }
                }
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
    fn note_exposure(&self, run_id: &str, content: ContentClass, label: &str) {
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return;
        };
        if let Some(sh) = self.sh.upgrade()
            && let Some(ctx) = sh.runs.lock().get(&protocol::RunId(id))
        {
            ctx.taint.lock().note(content, label);
        }
    }

    fn note_output_exposure(&self, run_id: &str, exposure: &tool_registry::OutputExposure) {
        let Ok(id) = run_id.parse::<Uuid>() else {
            return;
        };
        if let Some(sh) = self.sh.upgrade()
            && let Some(ctx) = sh.runs.lock().get(&RunId(id))
        {
            taint::ExecFact::from_exposure(exposure, "file read").apply(&mut ctx.taint.lock());
        }
    }

    fn content_untrusted(&self, run_id: &str) -> bool {
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return false;
        };
        let Some(sh) = self.sh.upgrade() else {
            return false;
        };
        sh.runs
            .lock()
            .get(&protocol::RunId(id))
            .is_some_and(|ctx| ctx.taint.lock().exposure.untrusted)
    }

    fn content_tainted(&self, run_id: &str) -> bool {
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return false;
        };
        let Some(sh) = self.sh.upgrade() else {
            return false;
        };
        sh.runs
            .lock()
            .get(&protocol::RunId(id))
            .is_some_and(|ctx| ctx.taint.lock().exposure.is_tainted())
    }

    async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
        self.authorize_inner(req, false).await.map(|_| ())
    }

    fn revoke_control(&self, run_id: &str) {
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return;
        };
        let Some(sh) = self.sh.upgrade() else {
            return;
        };
        if let Some(ctx) = sh.runs.lock().get(&protocol::RunId(id)) {
            ctx.control_grants.lock().clear();
        }
    }

    async fn authorize_control(
        &self,
        req: &GateRequest,
    ) -> Result<Option<tool_registry::ControlIdentity>, String> {
        self.authorize_inner(req, false)
            .await
            .map(|(identity, _)| identity)
    }

    async fn authorize_page(&self, req: &GateRequest) -> Result<Option<String>, String> {
        self.authorize_inner(req, true)
            .await
            .map(|(_, origin)| origin)
    }
}

impl LoopGate {
    /// The shared body of [`ToolGate::authorize`] and
    /// [`ToolGate::authorize_control`]. The returned program is the one the
    /// caller must pass to the backend's guarded path: `Some` only when a
    /// run-scoped control grant covers this call. A script call carries no
    /// target, so it is never covered.
    async fn authorize_inner(
        &self,
        req: &GateRequest,
        page_guard: bool,
    ) -> Result<(Option<tool_registry::ControlIdentity>, Option<String>), String> {
        // Safe mode: fail closed like `resolve_call` (replay, scripts, any gate caller).
        if self.sh.upgrade().is_some_and(|sh| sh.cfg.safe_mode) {
            return Err(format!("unknown tool: {}", req.tool));
        }
        self.authorize_checked(req, page_guard).await
    }

    async fn authorize_checked(
        &self,
        req: &GateRequest,
        page_guard: bool,
    ) -> Result<(Option<tool_registry::ControlIdentity>, Option<String>), String> {
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
        if !ctx.profile.spec_visible(&req.tool, req.category) {
            return Err(format!("unknown tool: {}", req.tool));
        }
        let origin = if page_guard && crate::shared::is_page_action(&req.tool) {
            let tctx = tool_registry::ToolContext {
                run_id: req.run_id.clone(),
                bot_id: Some(ctx.bot_id.to_string()),
                ..tool_registry::ToolContext::default()
            };
            if let Some(tool) = sh.tools.get(&req.tool) {
                tool.current_origin(&tctx, &req.input).await
            } else {
                None
            }
        } else {
            None
        };
        // Native replay sends through its own driver without a registry tool guard;
        // it stays once-only. Registry callers receive the origin to enforce at execution.
        if crate::shared::is_page_action(&req.tool) {
            ctx.page_origin.lock().clone_from(&origin);
        }
        let target = req.control.as_ref();
        let result = match sh.decide_with_target(
            &ctx,
            &req.tool,
            req.effect,
            req.category,
            &req.input,
            target,
        ) {
            Decision::Allow => {
                sh.refuse_unshowable_egress(&ctx, &req.tool, &req.input, None, false)
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
                if sh
                    .standing_grant(&ctx, &req.tool, &req.input)
                    .await
                    .is_some()
                {
                    Ok(())
                } else {
                    // Field/standing-grant inspection awaited above. Another concurrent
                    // script call may have observed a different page meanwhile; bind this
                    // card to this call's own observation before it captures the origin.
                    if crate::shared::is_page_action(&req.tool) {
                        ctx.page_origin.lock().clone_from(&origin);
                    }
                    sh.request_approval(
                        &ctx,
                        ToolCallId::new(),
                        &req.tool,
                        req.effect,
                        req.category,
                        &req.input,
                        details,
                        target.cloned(),
                    )
                    .await
                    .and_then(ExecPermit::commit)
                    .map_err(|reason| {
                        format!(
                            "permission denied by the user{}",
                            reason.map_or_else(String::new, |r| format!(": {r}"))
                        )
                    })
                }
            }
        };
        result?;
        // The slot is the host-wide computer semaphore. Take it only after the
        // user (or policy) has allowed the call, and record taint only once the
        // call will actually run: a denial or a failed admission read nothing.
        if matches!(
            req.category,
            PermissionCategory::Desktop | PermissionCategory::Browser
        ) {
            crate::control::acquire_computer_slot(&sh, &ctx).await?;
        }
        note_gate(&ctx, req);
        Ok((
            target
                .filter(|t| ctx.control_covers(&req.tool, req.category, t))
                .map(|t| t.identity.clone()),
            origin,
        ))
    }
}

/// A script or workflow call that was allowed has put its result in front of
/// the model by the time the next call is authorized. Record that now so the
/// next inner call sees the taint. A denial records nothing.
fn note_gate(ctx: &RunCtx, req: &GateRequest) {
    let mut content = req.content;
    if content.in_owner_browser && crate::browser_url::opens_non_public(&req.tool, &req.input) {
        content.reads_private = true;
    }
    ctx.taint
        .lock()
        .note(content, &taint::source_label(&req.tool, &req.input));
}

/// Whether the top-level run above `run_id` was started and steered only by the
/// owner: every user message delivered into it is `owner` or `peer_admin`.
/// Walks the parent chain; fails closed on storage errors and deep chains.
pub(crate) fn owner_started_run(
    storage: &storage::Storage,
    mut run_id: RunId,
    mut parent: Option<RunId>,
) -> bool {
    for _ in 0..8 {
        let Some(p) = parent else { break };
        run_id = p;
        parent = match storage.get_run(p) {
            Ok(Some(r)) => r.parent_run_id,
            _ => return false,
        };
    }
    if parent.is_some() {
        return false;
    }
    match storage.run_user_message_sources(run_id) {
        Ok(sources) => {
            !sources.is_empty()
                && sources
                    .iter()
                    .all(|s| matches!(s.as_str(), "owner" | "peer_admin"))
        }
        Err(_) => false,
    }
}
