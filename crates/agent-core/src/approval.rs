//! Permission decisions, interactive approvals, user questions and the
//! [`ToolGate`] handed to `script_run`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate, cwd_for, raw_command_for};
use computer_target::ComputerLabel;
use parking_lot::Mutex;
use permission_engine::{
    ActionRequest, AllowReason, AutonomyMode, CallFacts, Decision, EgressReach, Impact, Policy,
    balanced_lane, egress_must_ask, executes_must_ask,
};
use protocol::event::{
    ApprovalNeeded, HandoverKind, QuestionAsked, ToolCallApproved, ToolCallDenied,
};
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
    ApprovalScope, ControlGrant, Grant, GrantTarget, Pending, PendingKind, RunCtx, RunToolGrant,
    Shared, Verdict, control_kind_of,
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

/// Audit reason code of a browser or web-read call the Full level let through after a page was
/// read (the code predates the web tools; it stays because audit consumers know it).
const FULL_BROWSE_REASON: &str = "mode.full.owner_browse";

/// The managed browser's calls that Full lets an owner-started run make after reading a page:
/// open, go back, click, type, select, press, select text, tabs and the batch. NOT
/// `browser_eval` (a page script can send anything anywhere) and not the user's own computer
/// (`pc_*`).
pub(crate) const FULL_BROWSER_CALLS: [&str; 11] = [
    "browser_open",
    "browser_back",
    "browser_click",
    "browser_fill",
    "browser_select",
    "browser_press",
    "browser_select_text",
    "browser_dismiss_consent",
    "browser_tabs",
    "browser_act",
    "browser_pointer",
];

/// The public-web READ tools (`web-tools`): a search, a page, a feed, a sitemap, a page's
/// metadata. They follow the same rule as `browser_open` in the Full level: no card for an
/// owner-started run unless the run holds private data AND read untrusted content and the query
/// or address carries a private word, a data shape or exceeds the budget. They fetch over HTTP
/// with the tool's own address guard (private networks refused); they start no program and
/// type nothing into a page.
pub(crate) const FULL_WEB_READ_CALLS: [&str; 5] = [
    "web_search",
    "web_extract",
    "web_feed",
    "web_sitemap",
    "web_metadata",
];

/// A shell command of this many characters or more is a payload, not a command: it asks after
/// a page read.
pub(crate) const SHELL_COMMAND_LIMIT: usize = 400;
/// Most shell commands one run may start without a card after it read untrusted content.
pub(crate) const SHELL_WAIVER_BUDGET: usize = 60;

/// Longest `web_search` query a run that holds private data may send without a card, in
/// characters (the tool accepts 400). Longer is a paragraph, not a query.
const WEB_QUERY_LIMIT: usize = 160;

/// What a call's card says when nobody decided it in time (the model sees it as the denial reason).
pub(crate) const EXPIRED_REASON: &str = "expired: nobody answered in time";
/// Reason on the denial a Stop records for an approval that was still open.
pub(crate) const STOPPED_REASON: &str = "denied: stopped by you";
/// Reason on the denial recorded for an approval the owner skipped by writing a new message.
/// The desktop app matches this text (`REDIRECT_DENY_REASON` in the UI): keep them equal.
pub(crate) const REDIRECTED_REASON: &str = "denied: the owner wrote a new message instead";

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
        self.sh.handovers.lock().remove(&self.id);
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
        // A file the run writes after it read untrusted content may later be run by a
        // waived shell command: remember it before anything else decides.
        ctx.taint.lock().note_write(name, effect, category, input);
        // A browser call that names a profile other than the throw-away one may be signed in to
        // the owner's accounts: what the run reads from then on is private and cannot be compared.
        // The same for an address on this computer or its network (file:, localhost, LAN): a
        // page there is private machine data, whatever the owner's card said.
        if name.starts_with("browser_")
            && (self.browser_may_hold_logins(ctx, input)
                || crate::browser_url::opens_non_public(name, input))
        {
            ctx.taint.lock().mark_private_unhashed();
        }
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
        // Asking another bot is for the owner's own chat only.
        if name == crate::delegation::ASK_BOT && ctx.initiator != InitiatorKind::Human {
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
            "docx_write" | "xlsx_write" | "docx_append" | "xlsx_append_rows"
            | "xlsx_update_cells" => Some("path"),
            "csv_transform" | "doc_convert" => Some("output_path"),
            "doc_preview" => Some("output_dir"),
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
        // The owner's per-site / per-app rules. A block and an ask only ever tighten: they
        // sit above grants, read shortcuts and the autonomy mode. An allow is one more
        // remembered grant (below), never above a floor or the untrusted-content gate.
        let profile = self.profile_verdict(ctx, name, input, target);
        let mut profile_allows = false;
        if let Some(v) = &profile {
            match v.action {
                permission_engine::ProfileAction::Block => {
                    self.audit_profile_applied(ctx, name, v);
                    return (Decision::Deny, None);
                }
                permission_engine::ProfileAction::Ask => {
                    self.audit_profile_applied(ctx, name, v);
                    if d == Decision::Allow {
                        d = Decision::ask();
                    }
                    return (d, None);
                }
                permission_engine::ProfileAction::Allow => profile_allows = true,
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
            && matches!(ctx.initiator, InitiatorKind::Human | InitiatorKind::Mission)
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
        // A commit (under the owner's identity) or a branch switch (it rewrites work tree files)
        // after the run read untrusted content asks on every level but the owner's Full, where
        // only money asks: the lane, a plain policy allow and a remembered grant sit below this.
        // Like the egress gate, a standing "always" does not waive it.
        if taint::git_mutation(name, input)
            && ctx.profile.permissions.mode() != AutonomyMode::Full
            && ctx.taint.lock().exposure.untrusted
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
                // The owner's Full level: an owner-started run browses after reading a page.
                if self.full_mode_waives(ctx, name, input) {
                    return (Decision::Allow, Some(FULL_BROWSE_REASON));
                }
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
        let granted = control_covered
            || run_tool_covered
            || profile_allows
            || self.plain_granted(ctx, name, category, input);
        if profile_allows
            && !control_covered
            && !run_tool_covered
            && let Some(v) = &profile
        {
            self.audit_profile_applied(ctx, name, v);
        }
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
        // A helper bot never inherits a standing, chat or scoped grant of
        // either bot: its approvals are once-only.
        if ctx.delegation().is_some() {
            return false;
        }
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
        // Another bot's helper run never counts as owner-started, whatever its
        // initiator kind or parent chain says: it never inherits the owner's autonomy.
        if ctx.initiator != InitiatorKind::Human || ctx.delegation().is_some() {
            return false;
        }
        owner_started_run(&self.storage, ctx.run_id, ctx.parent_run_id)
    }

    /// [`Self::pc_owner_started`] plus the owner's own paired Telegram chat: who may use the
    /// Full level's waivers. A webhook, a mail, a schedule, an unpaired or key-less channel
    /// message, a subagent or a helper is never the owner here.
    pub(crate) fn full_owner_started(&self, ctx: &RunCtx) -> bool {
        if ctx.initiator != InitiatorKind::Human || ctx.delegation().is_some() {
            return false;
        }
        owner_started_run_from(&self.storage, ctx.run_id, ctx.parent_run_id, true)
    }

    /// A top-level session of a mission on a bot at the Full level: the mission's own, narrow
    /// counterpart of [`Self::full_owner_started`].
    ///
    /// A mission session is never the owner at the keyboard: its first message is a brief
    /// built from a ledger that earlier sessions filled with text copied from pages and mails
    /// (stored with the source `mission_brief`, which never counts as the owner's words, see
    /// `Storage::owner_message_texts`). It gets the Full level's web, mail-link and shell
    /// waivers (so an unattended task can go on after reading a page) and nothing else of the
    /// owner's authority: no `pc_*` tools, no `ask_bot`, no typed-URL trust beyond what the owner
    /// wrote in the goal, no subagents or helpers. Fails closed: every message of the run must be
    /// the brief or the owner's own steering.
    pub(crate) fn mission_full_started(&self, ctx: &RunCtx) -> bool {
        if ctx.initiator != InitiatorKind::Mission
            || ctx.mission.is_none()
            || ctx.parent_run_id.is_some()
            || ctx.delegation().is_some()
            || ctx.profile.permissions.mode() != AutonomyMode::Full
        {
            return false;
        }
        match self.storage.run_user_message_sources(ctx.run_id) {
            Ok(sources) => {
                sources.iter().any(|s| s == "mission_brief")
                    && sources
                        .iter()
                        .all(|s| matches!(s.as_str(), "mission_brief" | "owner" | "peer_admin"))
            }
            Err(_) => false,
        }
    }

    /// Who may use the Full level's waivers: an owner-started run
    /// ([`Self::full_owner_started`]) or an unattended session of the owner's mission
    /// ([`Self::mission_full_started`]).
    pub(crate) fn full_waivers_apply(&self, ctx: &RunCtx) -> bool {
        self.full_owner_started(ctx) || self.mission_full_started(ctx)
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
        // A remembered site only waives untrusted-web-only egress; once the run has read
        // private data, navigating to any site still asks so private text cannot leak in URLs.
        if ctx.taint.lock().exposure.has_private() {
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

    /// Remember the host of an origin the owner just approved a navigation to (any scope), so a
    /// favicon may be fetched for it. Bounded; the oldest entry goes first.
    pub(crate) fn note_approved_origin(&self, name: &str, input: &Value) {
        let Derived::Scope(sc) = scoped_grant::derive(name, input) else {
            return;
        };
        if name != "browser_open" || sc.kind != ScopeKind::Origin {
            return;
        }
        let Some(host) = host_of_origin(&sc.pattern) else {
            return;
        };
        let mut hosts = self.approved_hosts.lock();
        if !hosts.contains(&host) {
            if hosts.len() >= 500 {
                hosts.pop_front();
            }
            hosts.push_back(host);
        }
    }

    /// Whether `host` already counts as allowed for some bot: a standing or chat grant for its
    /// origin, or a navigation the owner approved. Fetching its favicon is then no new egress.
    pub(crate) fn site_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let origin_match = |tool: &str, sc: &Scope| {
            tool == "browser_open"
                && sc.kind == ScopeKind::Origin
                && host_of_origin(&sc.pattern).is_some_and(|h| h == host)
        };
        if self.approved_hosts.lock().contains(&host) {
            return true;
        }
        let g = self.grants.lock();
        g.always.iter().any(|x| {
            !x.inert
                && matches!(&x.target, GrantTarget::Scoped { tool, scope } if origin_match(tool, scope))
        }) || g
            .this_chat_scoped
            .iter()
            .any(|(_, _, tool, sc, _)| origin_match(tool, sc))
            || g.gate_chat
                .iter()
                .any(|(_, _, class, dest)| *class == "web" && host_of_origin(dest).is_some_and(|h| h == host))
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
        // The helper's own autonomy mode must not allow what the asking bot
        // would have to ask for.
        if ctx.delegation().is_some()
            || name.starts_with(builtin_tools::pc::PC_PREFIX)
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
            named_browser_profile: self.browser_may_hold_logins(ctx, input),
        })
    }

    /// The one rule for "this browser call may run where the owner is signed in": it names a
    /// profile other than the throw-away one, or the bot's default profile holds a sign-in the
    /// owner did it for (a model that leaves `profile` out must not turn a saved account into
    /// anonymous browsing). The Balanced lane, the Full-level waiver and the private-data taint
    /// all ask this one question so they cannot drift apart.
    fn browser_may_hold_logins(&self, ctx: &RunCtx, input: &Value) -> bool {
        input
            .get("profile")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.is_empty() && p != "isolated")
            || self
                .browser_profiles
                .get()
                .is_some_and(|profiles| profiles.may_be_signed_in(&ctx.bot_id.to_string()))
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
            "shell_run" | "shell_interactive" | "script_run" => Some(("shell", String::new())),
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

    /// The URL of a plain navigation: `browser_open` with the `url` field alone, http(s), no
    /// userinfo, no query and no fragment. Only the structural part: whether it may go out
    /// silently is decided by [`Self::plain_navigation_waived`] (root URLs only).
    pub(crate) fn plain_navigation(name: &str, input: &Value) -> Option<url::Url> {
        if name != "browser_open" || input.as_object().is_none_or(|o| o.len() != 1) {
            return None;
        }
        let url = url::Url::parse(input.get("url")?.as_str()?).ok()?;
        (matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some()
            && url.query().is_none()
            && url.fragment().is_none())
        .then_some(url)
    }

    /// A bare token (no scheme) an owner wrote counts as a site name only when it is a
    /// registrable-domain shape under a real public TLD that is not a common file extension
    /// (`README.md`, `main.rs`, `script.sh` are files, not sites).
    fn bare_site_name(authority: &str) -> bool {
        const FILE_EXT: &[&str] = &[
            "md", "rs", "py", "sh", "js", "ts", "json", "toml", "txt", "html", "htm", "css", "exe",
            "zip", "pdf", "yml", "yaml", "log", "bat", "ps1", "cs", "pl", "rb", "go", "cc", "c",
            "h", "so", "dll", "doc", "docx", "xls", "xlsx", "png", "jpg", "gif", "svg", "xml",
            "csv", "ini", "cfg", "lock", "tmp", "bak", "mov", "mp3", "mp4", "tar", "gz", "rar",
            "7z", "jar", "apk", "iso", "dmg", "msi", "sql", "db", "php", "lua", "ex", "kt",
            "swift",
        ];
        const TLDS: &[&str] = &[
            "com", "net", "org", "io", "dev", "app", "co", "de", "uk", "edu", "gov", "info", "ai",
            "tr", "fr", "es", "it", "nl", "be", "ch", "at", "se", "no", "dk", "fi", "pl", "pt",
            "ie", "gr", "cz", "ru", "ua", "jp", "kr", "cn", "in", "au", "nz", "ca", "us", "br",
            "mx", "ar", "za", "eu", "biz", "xyz", "online", "site", "shop", "store", "tech",
            "cloud", "blog", "news", "me", "tv",
        ];
        let labels: Vec<&str> = authority.split('.').collect();
        let ok_label = |l: &&str| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        };
        let Some(tld) = labels.last().map(|t| t.to_ascii_lowercase()) else {
            return false;
        };
        labels.len() >= 2
            && labels.iter().all(ok_label)
            && TLDS.contains(&tld.as_str())
            && !FILE_EXT.contains(&tld.as_str())
    }

    /// `host:port` of a navigation, host lower case with no trailing dot.
    fn nav_site(url: &url::Url) -> Option<(String, u16)> {
        Some((
            url.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
            url.port_or_known_default()?,
        ))
    }

    /// A navigation to the ROOT of an origin (path `/` or empty, no query, fragment, userinfo
    /// or explicit port) that is the current page's origin, an origin the owner named in this
    /// chat (a full http(s) URL or a bare domain under a real TLD) or one they approved once.
    /// A root URL carries nothing but the host, so it is not "sending". Any non-empty path
    /// asks (unless the owner granted the whole site, see `gate_waived`): a path can carry a
    /// value copied from a page, and no shape or word rule on it is reliable.
    fn plain_navigation_waived(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        // With private data in the context even a host name is a channel: keep asking.
        if ctx.taint.lock().exposure.has_private() {
            return false;
        }
        let Some(url) = Self::plain_navigation(name, input) else {
            return false;
        };
        if url.path() != "/" || url.port().is_some() {
            return false;
        }
        let Some(site) = Self::nav_site(&url) else {
            return false;
        };
        let origin = format!("{}://{}:{}", url.scheme(), site.0, site.1);
        if ctx
            .page_origin
            .lock()
            .as_deref()
            .and_then(Self::origin_of)
            .is_some_and(|o| o == origin)
        {
            return true;
        }
        if self
            .grants
            .lock()
            .nav_origin
            .contains(&(ctx.chat_id, ctx.bot_id, origin))
        {
            return true;
        }
        let Ok(texts) = self.storage.owner_message_texts(ctx.chat_id) else {
            return false;
        };
        texts
            .iter()
            .flat_map(|m| m.split_whitespace())
            .map(|w| {
                w.trim_matches([
                    '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'',
                ])
            })
            .any(|w| {
                let (scheme, rest) = match w.split_once("://") {
                    Some((s, r)) if matches!(s, "http" | "https") => (Some(s), r),
                    Some(_) => return false,
                    None => (None, w),
                };
                let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
                // A bare domain must look like a real site, not a file name.
                if scheme.is_none() && !Self::bare_site_name(authority) {
                    return false;
                }
                let Ok(said) =
                    url::Url::parse(&format!("{}://{authority}", scheme.unwrap_or("https")))
                else {
                    return false;
                };
                // Userinfo in what the owner wrote is not a plain site name.
                if !said.username().is_empty() || said.password().is_some() {
                    return false;
                }
                // A site named with https is never used over plain http.
                Self::nav_site(&said) == Some(site.clone())
                    && (said.scheme() == url.scheme()
                        || (scheme.is_none() && url.scheme() == "https"))
            })
    }

    /// Hosts the owner named in a message they typed in this chat (a full http(s) address or a
    /// bare domain under a real TLD). Only `owner`-tagged messages count, so neither a page nor
    /// a mail nor the model can add to this list.
    fn owner_named_hosts(&self, ctx: &RunCtx) -> Vec<String> {
        let Ok(texts) = self.storage.owner_message_texts(ctx.chat_id) else {
            return Vec::new();
        };
        texts
            .iter()
            .flat_map(|m| m.split_whitespace())
            .filter_map(|w| {
                let w = w.trim_matches([
                    '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'',
                ]);
                let (scheme, rest) = match w.split_once("://") {
                    Some((s, r)) if matches!(s, "http" | "https") => (Some(s), r),
                    Some(_) => return None,
                    None => (None, w),
                };
                let authority = rest.split(['/', '?', '#']).next()?;
                if authority.contains('@') {
                    return None;
                }
                let host = authority.split(':').next()?.trim_end_matches('.');
                (scheme.is_some() || Self::bare_site_name(host))
                    .then(|| host.to_ascii_lowercase())
                    .filter(|h| h.contains('.'))
            })
            .collect()
    }

    /// Every word the owner typed in this chat (not copied content when it shows up in a URL).
    fn owner_words(&self, ctx: &RunCtx) -> HashSet<String> {
        self.storage
            .owner_message_texts(ctx.chat_id)
            .unwrap_or_default()
            .iter()
            .flat_map(|m| crate::full_autonomy::words(m))
            .collect()
    }

    /// `host` belongs to a site the OWNER named, or approved on a card or with a standing rule.
    /// The set never grows because the model navigated somewhere.
    fn trusted_host(&self, ctx: &RunCtx, host: &str, named: &[String]) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        named
            .iter()
            .any(|n| host == *n || host.ends_with(&format!(".{n}")))
            || self.site_allowed(&host)
            || self
                .grants
                .lock()
                .nav_origin
                .iter()
                .any(|(chat, bot, origin)| {
                    *chat == ctx.chat_id
                        && *bot == ctx.bot_id
                        && host_of_origin(origin).is_some_and(|h| h == host)
                })
    }

    /// The owner's Full level: an OWNER-STARTED run (typed in the desktop app, or sent from the
    /// owner's admin desktop) keeps working on the web after it read a page, without a card.
    ///
    /// Without this a page read taints the run (every browser call counts as reading the page
    /// AND as private) and every later open, type or key press asked, even a deep page of the
    /// very site the owner named, or the fifth sign-up site of a research task.
    ///
    /// * No private data of the owner in the run (files, mail, memory, screens, other bots):
    ///   every public site, new ones included, and every form passes. The details the owner
    ///   typed for the task are not private; secrets are typed by the daemon, never seen here.
    /// * Private data AND untrusted content in the run: a site the owner named or approved
    ///   (deep pages, subdomains, its own forms) still passes; anywhere else a call passes
    ///   only when no word of the private results (`SeenText`) is in it, it has no data shape,
    ///   it is small (a form field under 40 characters) and the run's running total sent to
    ///   such places is under a budget. Otherwise it asks, as before.
    ///
    /// The public-web read tools (`web_search`, `web_extract`, `web_feed`, `web_sitemap`,
    /// `web_metadata`) follow the same rules as `browser_open`: a query or an address is judged
    /// like the text and the URL of a browser call on a site the owner did not name (it is never
    /// "the owner's own page"), and a query of [`WEB_QUERY_LIMIT`] characters or more asks once
    /// the run holds private data and read untrusted content.
    ///
    /// Never covered, whatever the level: an owner-stored secret typed with `browser_fill`
    /// (always-ask, decided before this), money and the never-list (floors), mail and message
    /// sends (always-ask tools), `browser_eval`, a program start (`executes`), a named browser
    /// profile, the user's own computer, a non-public address, a card-like number or a key in
    /// typed text, hidden characters and anything the redactor would change. A run that is not
    /// owner-started (schedule, webhook, mail, channel, a peer, a subagent, an external agent)
    /// is unchanged, and so are the Normal, Ask and custom levels.
    fn full_mode_waives(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if name == "shell_run" {
            return self.full_shell_waives(ctx, input);
        }
        // A subagent (and a helper bot another bot called) keeps the old rule: it never inherits
        // the owner's Full autonomy from the run that started it.
        let web_read = FULL_WEB_READ_CALLS.contains(&name);
        if ctx.parent_run_id.is_some()
            || ctx.delegation().is_some()
            || !(web_read || FULL_BROWSER_CALLS.contains(&name))
            || ctx.profile.permissions.mode() != AutonomyMode::Full
            || !self.full_waivers_apply(ctx)
        {
            return false;
        }
        // A profile that may be signed in (a named one, or the default one after the owner
        // signed in there) keeps asking, like the Balanced lane does: signing in once is not a
        // per-call waiver. The public-web read tools never touch a browser profile.
        if !web_read && self.browser_may_hold_logins(ctx, input) {
            return false;
        }
        if self.content_class(ctx, name, input).executes
            || crate::browser_url::opens_non_public(name, input)
            || taint::input_forbids_egress(input)
            || taint::input_needs_a_card_to_show(input)
        {
            return false;
        }
        if let Some(r) = &self.cfg.redactor {
            let mut shown = input.clone();
            if crate::shared::redact_value(r, &mut shown) {
                return false;
            }
        }
        let (urls, texts) = crate::full_autonomy::split_input(input);
        // A key, a card number or an encoded blob is never typed or sent silently.
        if texts
            .iter()
            .any(|t| crate::full_autonomy::text_carries_data(t))
        {
            return false;
        }
        // Owner-initiated browsing of the public web (new sites, sign-ups, forms with the
        // details the owner typed) never asks, until the run holds the owner's PRIVATE data
        // AND read untrusted content: then a page could talk the model into carrying it out.
        let restricted = {
            let taint = ctx.taint.lock();
            taint.exposure.untrusted && taint.private_source
        };
        if !restricted {
            return true;
        }
        let named = self.owner_named_hosts(ctx);
        // What this call would send to places the owner did not name.
        let mut checked: Vec<String> = Vec::new();
        let mut sent = 0usize;
        for url in &urls {
            let Some(host) = url::Url::parse(url.trim())
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
            else {
                return false;
            };
            if self.trusted_host(ctx, &host, &named) {
                continue;
            }
            if crate::full_autonomy::url_carries_data(url) {
                return false;
            }
            sent += crate::full_autonomy::url_sent_chars(url);
            checked.push(crate::full_autonomy::decoded(url));
        }
        // Text typed into a page the owner trusts is the owner's business. A search query goes
        // to the search provider whatever page is open, so it is always judged.
        let page_is_owners = !web_read
            && ctx
                .page_origin
                .lock()
                .as_deref()
                .and_then(host_of_origin)
                .is_some_and(|h| self.trusted_host(ctx, &h, &named));
        if !texts.is_empty() && !page_is_owners {
            let limit = if web_read {
                WEB_QUERY_LIMIT
            } else {
                crate::full_autonomy::FOREIGN_TEXT_LIMIT
            };
            for text in &texts {
                let len = text.chars().count();
                if len >= limit {
                    return false;
                }
                sent += len;
                checked.push(text.clone());
            }
        }
        if checked.is_empty() {
            return true;
        }
        let owner_words = self.owner_words(ctx);
        let mut taint = ctx.taint.lock();
        // Private data that could not be hashed (memory without text, an earlier turn), copied
        // private words, and the run's running total all ask.
        if taint.seen.unhashed
            || taint.seen.sent + sent > crate::full_autonomy::SEND_BUDGET
            || checked.iter().any(|c| taint.seen.copied(c, &owner_words))
        {
            return false;
        }
        taint.seen.sent += sent;
        true
    }

    /// The owner's Full level for `shell_run` (the bot's OWN host shell) after the run read
    /// untrusted content. A program start after a page read always asked, because the page
    /// might have chosen the command. In Full it runs without a card only when ALL hold:
    ///
    /// * an owner-started run ([`Self::full_owner_started`]: the desktop, the admin desktop or
    ///   the owner's paired Telegram chat), not a subagent, not a helper, not a mail, webhook,
    ///   schedule or external-agent run, on the bot's own host;
    /// * the bot's level is Full, the policy already allowed the call, and the input is plain
    ///   (`command`, `shell`, `cwd`, `timeout` only) with no hidden character and nothing the
    ///   redactor would change;
    /// * the WHOLE command line is on the strict allow-list of [`crate::shell_allow`]: it
    ///   parses without any expansion, and every simple command in it is a read-only or
    ///   local-build program from a fixed table whose flags and paths are checked (paths stay
    ///   inside the bot's workspace). Anything else, including every network client, every
    ///   interpreter wrapper, persistence and system change, asks. Fails closed on any doubt;
    /// * an interpreter running a script, a build or test tool, `make` or `git` (they start code
    ///   that lives in the project) is waived only while the run has NOT written or changed
    ///   anything that could hold code AFTER it read untrusted content
    ///   ([`taint::ContentTaint::code_written_after_untrusted`], recorded for file tools when
    ///   they are decided and for redirects of waived commands): a script a page could have
    ///   shaped never becomes a waived run;
    /// * the run holds no private data of the owner, OR only private data whose text could be
    ///   hashed and the command copies none of its words (private data that cannot be
    ///   compared, or too much of it, asks);
    /// * the run has not used up [`SHELL_WAIVER_BUDGET`] such commands.
    ///
    /// Residual (documented): a command the owner approved on a card runs as approved; the
    /// files it writes are not tracked.
    fn full_shell_waives(&self, ctx: &RunCtx, input: &Value) -> bool {
        if ctx.parent_run_id.is_some()
            || ctx.delegation().is_some()
            || ctx.profile.permissions.mode() != AutonomyMode::Full
            || !self.full_waivers_apply(ctx)
            || !self.is_host_homed()
            || !matches!(
                ctx.resolved_computer.label,
                ComputerLabel::Host | ComputerLabel::Browser
            )
        {
            return false;
        }
        let Some(obj) = input.as_object() else {
            return false;
        };
        if !obj
            .keys()
            .all(|k| matches!(k.as_str(), "command" | "shell" | "cwd" | "timeout"))
        {
            return false;
        }
        if obj
            .get("shell")
            .is_some_and(|s| !matches!(s.as_str(), Some("powershell" | "cmd" | "bash" | "sh")))
        {
            return false;
        }
        let Some(command) = obj.get("command").and_then(Value::as_str) else {
            return false;
        };
        let cwd = obj.get("cwd").and_then(Value::as_str).unwrap_or_default();
        if command.chars().count() >= SHELL_COMMAND_LIMIT
            || crate::full_autonomy::text_carries_data(command)
            || taint::input_forbids_egress(input)
            || taint::input_needs_a_card_to_show(input)
        {
            return false;
        }
        let Some(workspace) = self.bot_workspace_dir(ctx.bot_id) else {
            return false;
        };
        let Some(plan) = crate::shell_allow::plan(command, cwd, &workspace) else {
            return false;
        };
        if let Some(r) = &self.cfg.redactor {
            let mut shown = input.clone();
            if crate::shared::redact_value(r, &mut shown) {
                return false;
            }
        }
        let owner_words = self.owner_words(ctx);
        let mut taint = ctx.taint.lock();
        // What the command writes is remembered even when this call still asks.
        taint.note_redirect_writes(&plan.writes);
        if plan.runs_project_code && taint.code_written_after_untrusted {
            return false;
        }
        if taint.private_source && (taint.seen.unhashed || taint.seen.copied(command, &owner_words))
        {
            return false;
        }
        if taint.seen.shell_waived >= SHELL_WAIVER_BUDGET {
            return false;
        }
        taint.seen.shell_waived += 1;
        true
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
            || ctx.delegation().is_some()
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
        // A helper bot never inherits the owner's typed URLs, chat grants or Full-level waivers.
        // A mission session reads the owner's goal (and approvals given in its chat) the same
        // way; the brief is not the owner's text, so it adds nothing to either list.
        if !(ctx.initiator == InitiatorKind::Human || self.mission_full_started(ctx))
            || ctx.delegation().is_some()
        {
            return false;
        }
        if self.own_mail_waived(ctx, name, input) {
            return true;
        }
        if self.user_typed_url(ctx, name, input) || self.plain_navigation_waived(ctx, name, input) {
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

    /// The two calls of a sign-up that the egress gate would ask about after the
    /// run read a page or a mail, waived for an OWNER-started run of a bot on the
    /// Full level: reading the bot's OWN inbox (`email_search`, `email_read`,
    /// `email_list_folders`, `email_wait_for` on an identity bound to or shared
    /// with it, never an account every bot may use), and opening exactly a link
    /// `email_wait_for` returned as followable in this run (https, on the site the
    /// sign-up started on or an owner-listed domain). A run started by mail, a
    /// webhook, a schedule or a channel is never waived here: it keeps asking.
    fn own_mail_waived(&self, ctx: &RunCtx, name: &str, input: &Value) -> bool {
        if ctx.delegation().is_some()
            || ctx.profile.permissions.mode() != AutonomyMode::Full
            || !self.full_waivers_apply(ctx)
        {
            return false;
        }
        match name {
            "email_search"
            | "email_read"
            | "email_list_folders"
            | builtin_tools::mail_wait::EMAIL_WAIT_FOR => self.mail.get().is_some_and(|c| {
                c.scoped(Some(&ctx.bot_id.to_string()))
                    .owns(input.get("account").and_then(Value::as_str))
            }),
            "browser_open" => {
                input.as_object().is_some_and(|o| o.len() == 1)
                    && input.get("url").and_then(Value::as_str).is_some_and(|u| {
                        u.starts_with("https://") && ctx.mail_links.lock().contains(u)
                    })
            }
            _ => false,
        }
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
        // Defence in depth: a run started by external data (a signed webhook,
        // a received mail, an external schedule trigger) or by an external
        // agent never consumes a standing grant, whatever rows exist and
        // whatever the tool would say. Every ask stays a one-time card.
        // A helper bot's approvals are once-only as well.
        if crate::shared::is_external_initiator(ctx.initiator) || ctx.delegation().is_some() {
            return None;
        }
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
        kind: PendingKind,
    ) -> (Uuid, PendingGuard, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        let approval_id = Uuid::now_v7();
        self.pending.lock().insert(
            call_id,
            Pending {
                tx,
                once_only,
                grantable,
                gate_grant: false,
                run_tool: false,
                always_refused: false,
                run_id: ctx.run_id,
                approval_id,
                kind,
            },
        );
        *self.parked.lock().entry(ctx.run_id).or_insert(0) += 1;
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

    /// Wait for the user's verdict.  A schedule-started run, and an unattended mission
    /// session, only wait
    /// [`LoopConfig::schedule_approval_timeout`](crate::LoopConfig); then the
    /// run is asked to pause (resumable; the mission supervisor reads the pause and
    /// blocks the mission for the owner).  Any other run waits at most
    /// [`LoopConfig::approval_timeout`](crate::LoopConfig) (zero = no limit);
    /// then the call is denied as expired and the run carries on.
    async fn wait_verdict(&self, ctx: &RunCtx, rx: oneshot::Receiver<Verdict>) -> Waited {
        // A card parked after Stop (a parallel call) is closed at once.
        if self.run_is_stopped(ctx.run_id) {
            self.deny_open_for_stop(ctx.run_id);
        }
        if !matches!(
            ctx.initiator,
            InitiatorKind::Schedule | InitiatorKind::ExternalSchedule | InitiatorKind::Mission
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

    /// The owner stopped this run (the marker is set while it winds down) or it
    /// already ended `Cancelled`. Nothing may be approved, granted or started for it.
    pub(crate) fn run_is_stopped(&self, run_id: RunId) -> bool {
        self.user_stopped.lock().contains(&run_id)
            || self
                .storage
                .get_run(run_id)
                .ok()
                .flatten()
                .and_then(|r| RunState::parse(&r.state))
                .is_some_and(|s| s == RunState::Cancelled)
    }

    /// Record one approval as denied by the owner's Stop: the row, the audit
    /// entry and the closing event, only when this call is the one that decided
    /// the row (so every path may call it and the result is still once).
    pub(crate) fn record_stop_denial(&self, run_id: RunId, call_id: ToolCallId, approval_id: Uuid) {
        if self.storage.decide_approval(approval_id, "denied").is_err() {
            return;
        }
        let ctx = self.runs.lock().get(&run_id).cloned().or_else(|| {
            let run = self.storage.get_run(run_id).ok().flatten()?;
            self.ctx(&run).ok()
        });
        let Some(ctx) = ctx else {
            return;
        };
        let redirected = self.redirected.lock().contains(&run_id);
        self.audit(
            &ctx,
            "user",
            "approval.denied",
            &json!({ "reason": if redirected {
                "the owner wrote a new message instead"
            } else {
                "stopped by you"
            } }),
        );
        self.emit(
            &ctx,
            Event::ToolCallDenied(ToolCallDenied {
                run_id,
                call_id,
                denied_by: ctx.user_id,
                reason: Some(
                    if redirected {
                        REDIRECTED_REASON
                    } else {
                        STOPPED_REASON
                    }
                    .into(),
                ),
            }),
        );
    }

    /// Close every open card of `run_id` as denied, now. The cards leave
    /// `pending`, so no later approve/answer can reach them, and their waits
    /// end without a verdict.
    pub(crate) fn deny_open_for_stop(&self, run_id: RunId) {
        // Cancel the run's token BEFORE its waits are released. Releasing a
        // card wakes the step; if the token were still live the step could
        // finish as "denied", the drive loop would plan the next model call and
        // only then see the cancel (a second model call after Stop).
        if let Some(rt) = self.rt.get().and_then(Weak::upgrade) {
            rt.signal_cancel(run_id);
        }
        builtin_tools::handover_hub().forget(&run_id.to_string());
        let drained: Vec<(ToolCallId, Pending)> = {
            let mut pending = self.pending.lock();
            let ids: Vec<ToolCallId> = pending
                .iter()
                .filter(|(_, p)| p.run_id == run_id)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|p| (id, p)))
                .collect()
        };
        for (call_id, p) in drained {
            self.secret_requests.lock().remove(&call_id);
            self.record_stop_denial(run_id, call_id, p.approval_id);
        }
        #[cfg(test)]
        {
            let hook = self.stop_split_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
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
        // The owner pressed Stop: that is a refusal, recorded once (the stop
        // itself normally did it already). A timeout, or a schedule run that
        // pauses because nobody answered, stays "expired".
        if self.run_is_stopped(run_id) {
            self.record_stop_denial(run_id, call_id, approval_id);
            return;
        }
        if self
            .storage
            .decide_approval(approval_id, "expired")
            .is_err()
        {
            // Already closed by someone else: nothing more to report.
            return;
        }
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
        // A timeout resumes so the model can carry on.
        if last_parked {
            self.set_state(run_id, RunState::Running);
        }
        let ctx = self.runs.lock().get(&run_id).cloned();
        if let Some(ctx) = ctx {
            let reason = EXPIRED_REASON;
            self.emit(
                &ctx,
                Event::ToolCallDenied(ToolCallDenied {
                    run_id,
                    call_id,
                    denied_by: ctx.user_id,
                    reason: Some(reason.into()),
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
        // A mail reply's recipients and subject are filled in by the tool: the egress checks and
        // the card must see what is really sent, not the model's partial input.
        let sent = taint::input_as_sent(name, input, details.as_ref());
        self.refuse_unshowable_egress(ctx, name, &sent, token.as_deref(), true)
            .map_err(Some)?;
        let disclosure = self.egress_card(ctx, name, &sent);
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
                    && ctx.delegation().is_none()
                    && ctx.initiator == InitiatorKind::Human
                    && (!gate_grant || sc.kind == ScopeKind::Origin) =>
            {
                Some(sc.clone())
            }
            _ => None,
        };
        let unscopable = matches!(derived, Derived::Unscopable);
        // A helper bot's approvals are once-only: no remembered choice for either bot.
        let once_only = ctx.delegation().is_some()
            || !gate_grant
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
        // Whose request this is, from the daemon's own record of the hand-off (never model text).
        if let Some(link) = ctx.delegation() {
            prefix.push_str("[Helper bot: ");
            prefix.push_str(&link.caller_name);
            prefix.push_str(" asked ");
            prefix.push_str(&link.callee_name);
            prefix.push_str("] ");
            if let Some(o) = details.get_or_insert_with(|| json!({})).as_object_mut() {
                o.insert(
                    "delegation".into(),
                    json!({"caller": link.caller_name, "callee": link.callee_name}),
                );
            }
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
        // Destructive, credential and payment actions never offer or accept "Always allow".
        let site_egress_offered = why == "egress"
            && name == "browser_open"
            && !exposure.has_private()
            && memory.as_ref().is_some_and(|m| m.kind == ScopeKind::Origin);
        #[cfg(test)]
        let legacy_always = self
            .legacy_always
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let legacy_always = false;
        let always_refused = (Self::run_tool_eligible(name) && exposure.has_private())
            || matches!(why, "floor" | "always_ask" | "executes" | "browser_address")
            || (why == "egress" && !site_egress_offered)
            || !always_allowed(
                name,
                effect,
                category,
                memory.as_ref().is_some_and(|m| m.kind == ScopeKind::Origin),
                legacy_always,
            );
        let always_offered = !once_only && !always_refused;
        let class = self.content_class(ctx, name, input);
        if let Some(obj) = details.get_or_insert_with(|| json!({})).as_object_mut() {
            obj.insert("why".into(), json!(why));
            obj.insert("once_only".into(), json!(once_only));
            obj.insert("run_grant".into(), json!(run_tool));
            obj.insert("always_offered".into(), json!(always_offered));
            // Channels that cannot show the whole card (Telegram) offer buttons for "low" only;
            // a missing value counts as "high". Low: an ordinary policy ask, never destructive,
            // never on the owner's PC or browser, never for an external caller.
            let low_risk = why == "policy"
                && effect != Effect::Destructive
                && ctx.initiator == InitiatorKind::Human
                && !class.in_owner_browser
                && !name.starts_with(builtin_tools::pc::PC_PREFIX)
                && !obj.contains_key("computer_target");
            obj.insert("risk".into(), json!(if low_risk { "low" } else { "high" }));
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
        let (approval_id, mut guard, rx) = self.park(
            ctx,
            call_id,
            &summary,
            once_only,
            grantable,
            PendingKind::Approval,
        );
        if let Some(p) = self.pending.lock().get_mut(&call_id) {
            p.gate_grant = gate_grant || (memory.is_some() && !once_only);
            p.run_tool = run_tool;
            p.always_refused = always_refused;
        }
        let parked_at = policy_revision(self);
        self.remember_parked(
            call_id,
            ctx,
            name,
            effect,
            category,
            input,
            page_origin.as_deref(),
            target.as_ref(),
        );
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
                    if self.run_is_stopped(ctx.run_id) {
                        self.record_stop_denial(ctx.run_id, call_id, approval_id);
                        return Err(Some(STOPPED_REASON.into()));
                    }
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
            Ok(Verdict::Answered { .. } | Verdict::SecretStored | Verdict::HandoverDone)
            | Err(_) => Err(Some("approval request was dropped".into())),
        }
    }

    #[allow(clippy::too_many_arguments)] // the card's facts, as the recheck needs them
    fn remember_parked(
        &self,
        call_id: ToolCallId,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: PermissionCategory,
        input: &Value,
        page_origin: Option<&str>,
        target: Option<&ControlTarget>,
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
                page_origin: page_origin.map(str::to_owned),
                target: target.cloned(),
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
    pub(crate) fn add_grant(
        &self,
        ctx: &RunCtx,
        target: GrantTarget,
        category: PermissionCategory,
        name: &str,
    ) {
        // Defence in depth (the callers already force `Once` for these runs):
        // an externally started run never creates a standing grant.
        if crate::shared::is_external_initiator(ctx.initiator) {
            return;
        }
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
        let (approval_id, mut guard, rx) =
            self.park(ctx, call_id, &summary, false, false, PendingKind::Question);
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
            Ok(Verdict::Approved { .. } | Verdict::SecretStored | Verdict::HandoverDone)
            | Err(_) => {
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
    #[allow(clippy::too_many_lines)] // One linear park / wait / outcome sequence.
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
            if ctx.mission.is_some() {
                (mission_refusal_text(why), true)
            } else {
                secret_outcome(name, false, why)
            }
        };
        if !self.cfg.secret_requests {
            return refuse("secret requests are switched off");
        }
        if ctx.initiator != InitiatorKind::Human && ctx.mission.is_none() {
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
        // Nobody is at the keyboard in a mission: the item that needs the credential is
        // parked in the ledger and the session moves on.
        if let Some(mission) = &ctx.mission {
            return self.park_item_for_secret(ctx, mission, name);
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
        let (approval_id, mut guard, rx) =
            self.park(ctx, call_id, &summary, true, false, PendingKind::Secret);
        self.secret_requests.lock().insert(
            call_id,
            crate::shared::SecretRequestInfo {
                run_id: ctx.run_id,
                name: name.to_owned(),
                fields,
                replace: card.replace,
                bot_id: ctx.bot_id.to_string(),
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

/// What the model reads when a mission asked for help or a credential more often than the run
/// allows: nothing was shown, and it has to park the item itself.
fn mission_refusal_text(why: &str) -> String {
    format!(
        "Not asked: {why}. This is a mission, so nobody waits for an answer: mark the item you are working on as blocked with mission_item (say what the owner has to do), do not retry it, and take the next open item now."
    )
}

/// What the model reads when a mission session parks an item for the owner instead of waiting.
fn mission_park_text(key: Option<&str>, reason: &str) -> String {
    match key {
        Some(key) => format!(
            "Not waiting: this is a mission, so a person is not asked right now. Item {key:?} is parked as blocked ({reason}); the owner sees it in the blocked list. Do not retry it. Take the next open item now."
        ),
        None => format!(
            "Not waiting: this is a mission, so a person is not asked right now. Mark the item you are working on as blocked with mission_item (reason: {reason}), do not retry it, and take the next open item now."
        ),
    }
}

/// What the model reads when a `browser_handover` ends. Never a code.
fn handover_outcome(kind: HandoverKind, done: bool, why: &str) -> (String, bool) {
    match (kind, done) {
        (HandoverKind::Code, true) => (
            "done: the owner entered the code and Praxi typed it into the page (you never see it). Submit the form if the page needs it, then take a fresh browser_snapshot.".to_owned(),
            false,
        ),
        (HandoverKind::Captcha, true) => (
            "done: the owner solved the check and the page moved on. Take a fresh browser_snapshot and continue.".to_owned(),
            false,
        ),
        (_, false) => (
            format!(
                "cancelled: the owner did not complete the step ({why}). Do not try to get around it; tell the owner what is blocked."
            ),
            true,
        ),
    }
}

impl Shared {
    /// A CAPTCHA or 2FA in a mission session blocks only the item it stops: nobody
    /// waits for the owner. The item the bot is working on is parked in the ledger
    /// (the owner finds it in the blocked list and the digest) and the session moves on.
    fn park_item_for_handover(
        &self,
        ctx: &RunCtx,
        mission: &str,
        kind: HandoverKind,
        card: &builtin_tools::HandoverCard,
    ) -> (String, bool) {
        builtin_tools::handover_hub().disarm(&ctx.run_id.to_string());
        let site = truncate_utf8(&card.site, 80);
        let reason = match kind {
            HandoverKind::Captcha => format!("a CAPTCHA on {site} has to be solved by the owner"),
            // A one-time code is never typed into a mission answer (it would sit in the ledger
            // and reach the model): the owner finishes this step live on the site or skips it.
            HandoverKind::Code => format!(
                "a 2FA / verification code for {site} is needed; Praxi never stores it or shows it to the bot, so finish this step yourself on the site, or skip the item"
            ),
        };
        let store = self.mission_store.read().clone();
        let key = store.and_then(|s| {
            s.block_sole_doing_item(
                mission,
                &reason,
                kind == HandoverKind::Code,
                storage::now_ms(),
            )
            .ok()
            .flatten()
        });
        self.audit(
            ctx,
            "system",
            "handover.parked",
            &json!({"kind": kind, "site": card.site, "item": key}),
        );
        (mission_park_text(key.as_deref(), &reason), true)
    }

    /// A credential the bot needs in a mission session: park the item being worked on, like a
    /// CAPTCHA. The owner adds the secret in the app (never in the answer text) and releases the item.
    fn park_item_for_secret(&self, ctx: &RunCtx, mission: &str, name: &str) -> (String, bool) {
        let name = truncate_utf8(name, 80);
        let reason = format!(
            "needs the credential {name:?}: add it in the app (never write a password in the answer), then release this item"
        );
        let store = self.mission_store.read().clone();
        let key = store.and_then(|s| {
            s.block_sole_doing_item(mission, &reason, false, storage::now_ms())
                .ok()
                .flatten()
        });
        self.audit(
            ctx,
            "system",
            "secret_request.parked",
            &json!({"name": name, "item": key}),
        );
        (mission_park_text(key.as_deref(), &reason), true)
    }

    /// Run a `browser_handover`: show the owner's card and park the run until the
    /// owner's app answers through the daemon, cancels it, or it expires. A code
    /// never passes through here: the owner-only route types it into the page and
    /// then calls `AgentLoop::finish_handover`.
    ///
    /// Refused (nothing shown) unless the run was started by the owner at the
    /// desktop and the per-run limit is not used up.
    pub(crate) async fn request_handover(
        self: &Arc<Self>,
        ctx: &RunCtx,
        call_id: ToolCallId,
        card: &builtin_tools::HandoverCard,
    ) -> (String, bool) {
        let kind = match card.kind {
            builtin_tools::HandoverKind::Captcha => HandoverKind::Captcha,
            builtin_tools::HandoverKind::Code => HandoverKind::Code,
        };
        let refuse = |why: &str| {
            builtin_tools::handover_hub().disarm(&ctx.run_id.to_string());
            self.audit(
                ctx,
                "system",
                "handover.refused",
                &json!({"kind": kind, "reason": why}),
            );
            if ctx.mission.is_some() {
                (mission_refusal_text(why), true)
            } else {
                handover_outcome(kind, false, why)
            }
        };
        if ctx.initiator != InitiatorKind::Human && ctx.mission.is_none() {
            return refuse(
                "only a run the owner started at the desktop can hand a step over; tell the owner in your answer instead",
            );
        }
        // The per-run limit counts parked handovers of a mission too: a page that throws a
        // CAPTCHA at every item cannot make a session park without end.
        {
            let mut st = ctx.state.lock();
            if st.handovers >= self.cfg.max_handovers_per_run {
                drop(st);
                return refuse("this run has asked for help too often");
            }
            st.handovers += 1;
        }
        if let Some(mission) = &ctx.mission {
            return self.park_item_for_handover(ctx, mission, kind, card);
        }
        let tainted = self
            .root_run_id(ctx)
            .is_none_or(|r| self.router.tainted_runs.lock().contains(&r));
        let limit = if self.cfg.approval_timeout.is_zero() {
            self.cfg.handover_timeout
        } else {
            self.cfg.handover_timeout.min(self.cfg.approval_timeout)
        };
        let summary = format!("handover: {}", truncate_utf8(&card.site, 80));
        let (approval_id, mut guard, rx) =
            self.park(ctx, call_id, &summary, true, false, PendingKind::Handover);
        self.handovers.lock().insert(
            call_id,
            crate::shared::HandoverInfo {
                run_id: ctx.run_id,
                kind,
            },
        );
        self.audit(
            ctx,
            "system",
            "handover.requested",
            &json!({"kind": kind, "site": card.site}),
        );
        self.emit(
            ctx,
            Event::HandoverRequested(protocol::event::HandoverRequested {
                run_id: ctx.run_id,
                call_id,
                kind,
                site: self.redact(card.site.clone()),
                untrusted_content: tainted,
                expires_in_secs: limit.as_secs(),
            }),
        );
        let mut rx = rx;
        let mut waited = tokio::time::timeout(limit, &mut rx).await;
        // An answer that claimed the request is typing the code right now: let it
        // finish, so the model never hears a false "not done".
        while waited.is_err() {
            if self.handovers.lock().remove(&call_id).is_some() {
                break;
            }
            waited = tokio::time::timeout(std::time::Duration::from_millis(100), &mut rx).await;
        }
        guard.resume();
        self.handovers.lock().remove(&call_id);
        builtin_tools::handover_hub().disarm(&ctx.run_id.to_string());
        let (done, why, decision) = match waited {
            Ok(Ok(Verdict::HandoverDone)) => (true, "", "approved"),
            Err(_) => (false, "the owner did not answer in time", "expired"),
            _ => (false, "the owner cancelled", "declined"),
        };
        let _ = self.storage.decide_approval(approval_id, decision);
        self.audit(
            ctx,
            "user",
            if done {
                "handover.done"
            } else {
                "handover.cancelled"
            },
            &json!({"kind": kind, "reason": decision}),
        );
        handover_outcome(kind, done, why)
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
        // Stop wins over a click that was already sent: no grant, no tool.
        if self.sh.run_is_stopped(self.ctx.run_id) {
            self.sh
                .record_stop_denial(self.ctx.run_id, self.call_id, self.approval_id);
            return Err(Some(STOPPED_REASON.into()));
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
        // A navigation the owner approved makes its host fetchable for favicons.
        self.sh.note_approved_origin(&self.name, &self.input);
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
        // Approving a plain navigation remembers its origin for this chat, but only for the
        // silent ROOT navigation (see `plain_navigation_waived`); it never covers other paths.
        if self.ctx.initiator == InitiatorKind::Human
            && let Some(url) = Shared::plain_navigation(&self.name, &self.input)
            && let Some((host, port)) = Shared::nav_site(&url)
        {
            self.sh.grants.lock().nav_origin.insert((
                self.ctx.chat_id,
                self.ctx.bot_id,
                format!("{}://{host}:{port}", url.scheme()),
            ));
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
                "bot_id": self.ctx.bot_id.to_string(),
                "subject": crate::site_profiles::approval_subject(
                    &self.name,
                    &self.input,
                    self.page_origin.as_deref(),
                    self.target.as_ref(),
                )
                .map(|(kind, key, capability)| json!({
                    "kind": kind.as_str(),
                    "key": key,
                    "capability": capability.as_str(),
                })),
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
    /// The page origin the card was shown for (site rules judge the card by it).
    page_origin: Option<String>,
    /// The program the card was shown for.
    target: Option<ControlTarget>,
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
        // The owner's block rules are judged for what the card was shown for.
        let blocked_by_rule = sh
            .profile_verdict_at(
                ctx.bot_id,
                &call.name,
                &call.input,
                call.page_origin.as_deref(),
                call.target.as_ref(),
            )
            .is_some_and(|v| v.action == permission_engine::ProfileAction::Block);
        if !blocked_by_rule
            && !matches!(
                sh.decide(&ctx, &call.name, call.effect, call.category, &call.input),
                Decision::Deny
            )
        {
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

    /// The same decision as [`Self::authorize`] with every way of asking removed:
    /// a call the profile hides, policy denies or policy would ask about
    /// (including every call in a run that read untrusted content) is refused,
    /// and nothing is recorded as read, because the caller discards the output.
    async fn authorize_unattended(&self, req: &GateRequest) -> Result<(), String> {
        let sh = self.sh.upgrade().ok_or("agent loop is gone")?;
        let run_id = req
            .run_id
            .parse::<Uuid>()
            .map(RunId)
            .map_err(|_| "no run context".to_owned())?;
        let ctx = sh
            .runs
            .lock()
            .get(&run_id)
            .cloned()
            .ok_or("no active run")?;
        let tool = sh
            .tools
            .get(&req.tool)
            .filter(|t| sh.spec_visible(&ctx.profile, ctx.bot_id, t.spec()))
            .ok_or_else(|| format!("unknown tool: {}", req.tool))?;
        // The registry's spec, not the caller's claim, decides what the call is.
        let spec = tool.spec();
        match sh.decide(&ctx, &req.tool, spec.effect, spec.category, &req.input) {
            Decision::Allow => {
                sh.refuse_unshowable_egress(&ctx, &req.tool, &req.input, None, false)
            }
            Decision::Deny => Err(format!("{} is not allowed by policy", req.tool)),
            Decision::Ask { .. } => Err(format!("{} needs approval", req.tool)),
        }
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
/// Read-only tools a standing "Always allow" may cover tool-wide. Everything else is refused:
/// the list is an allow-list on purpose, because a deny-list keyed on tool names misses sends,
/// writes, shell commands and any MCP / Composio tool with an innocent name.
const ALWAYS_READ_ONLY_TOOLS: &[&str] = &["fs_read", "fs_list", "fs_stat", "fs_search"];

/// Whether "Always allow" may be offered and accepted for this call at all. Allowed: a
/// `browser_open` scoped to one exact origin, and an explicitly listed read-only tool whose
/// effect really is a read. Not allowed: email / send / message / post / upload, file writes,
/// shell, MCP and Composio tools of any name, desktop input, and anything destructive, paid
/// or credential-related.
pub(crate) fn always_allowed(
    name: &str,
    effect: Effect,
    category: PermissionCategory,
    origin_scoped: bool,
    legacy_test_lift: bool,
) -> bool {
    if legacy_test_lift {
        return !(matches!(category, PermissionCategory::Money) || effect == Effect::Destructive);
    }
    if matches!(category, PermissionCategory::Money) || effect == Effect::Destructive {
        return false;
    }
    if name == "browser_open" {
        return origin_scoped;
    }
    effect == Effect::Read && ALWAYS_READ_ONLY_TOOLS.contains(&name)
}

fn host_of_origin(origin: &str) -> Option<String> {
    url::Url::parse(origin)
        .ok()?
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
}

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
    run_id: RunId,
    parent: Option<RunId>,
) -> bool {
    owner_started_run_from(storage, run_id, parent, false)
}

/// [`owner_started_run`], optionally also accepting `telegram_owner`: the owner writing to
/// their own bot from their paired Telegram chat (paired user id AND owner key, see the
/// daemon's message route). Only the Full-level waivers accept it; reaching the user's own
/// computer (`pc_*`) stays desktop / admin-desktop only.
pub(crate) fn owner_started_run_from(
    storage: &storage::Storage,
    mut run_id: RunId,
    mut parent: Option<RunId>,
    owner_channel: bool,
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
                && sources.iter().all(|s| {
                    matches!(s.as_str(), "owner" | "peer_admin")
                        || (owner_channel && s == "telegram_owner")
                })
        }
        Err(_) => false,
    }
}
